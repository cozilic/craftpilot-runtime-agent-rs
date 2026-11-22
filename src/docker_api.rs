//! Docker Engine API via bollard för telemetrins nätverksräknare (2026-11-22).
//!
//! `docker stats`-CLI:t formaterar `NetIO` till tre värdesiffror ("353MB /
//! 247MB"), så ett delta på 40 kB blir antingen 0 eller ett hopp på ~100 kB.
//! Räknarna läses därför ur `/containers/{id}/stats` med bollard, samma
//! endpoint som masterns kontrakt beskriver
//! (`docs/RUNTIME-NODE-NETWORK-TELEMETRY.md` i CraftPilot-repot).
//!
//! Ett fel här får aldrig stoppa en rapport: alla vägar som inte ger ett
//! mätvärde lämnar `(None, None)` och servern rapporteras som vanligt.
//!
//! bollard 0.21 skickar sökvägen utan versionsprefix, så daemonen svarar med
//! sin egen API-version. Det är avsiktligt ofarligt: äldre daemoner saknar
//! `one-shot` och skickar då två samplingar, och läsaren tar den sista.

use std::collections::HashMap;
use std::time::Duration;

use bollard::models::ContainerNetworkStats;
use bollard::query_parameters::StatsOptions;
use bollard::{Docker, API_DEFAULT_VERSION};
use futures_util::StreamExt;
use tracing::debug;

/// Kort tidsgräns: hellre tomma räknare i rapporten än en försenad rapport.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// Klient för de få Docker-anrop som behöver rådata. CLI:t används fortfarande
/// för allt annat (start, stopp, inspect, loggar).
pub struct DockerApi {
    /// `None` när `DOCKER_HOST` pekar på något bollard inte kan nå härifrån
    /// (t.ex. `npipe://`) eller när socketen saknas. Då rapporteras inga
    /// räknare i stället för att agenten faller.
    client: Option<Docker>,
}

/// Hur `DOCKER_HOST` tolkades. Ren funktion så mappningen kan testas utan en
/// daemon.
#[derive(Debug, PartialEq, Eq)]
enum HostTarget {
    /// Docker-socketen (`unix://…`, en absolut sökväg eller standardvägen).
    Unix(String),
    /// `tcp://…`/`http://…`: daemonen nås över HTTP i stället.
    Http(String),
    /// `npipe://`, `https://` och okända scheme: ingen läsning.
    Unsupported,
}

impl DockerApi {
    pub fn from_env() -> Self {
        Self::from_docker_host(std::env::var("DOCKER_HOST").ok().as_deref())
    }

    fn from_docker_host(host: Option<&str>) -> Self {
        Self {
            client: connect(&host_target(host)),
        }
    }

    /// Containerns kumulativa (rx, tx) i byte, summerat över alla interface.
    /// `(None, None)` när daemonen inte kunde svara, containern saknas eller
    /// svaret inte bär några nätverksräknare.
    pub async fn container_net_io(&self, container: &str) -> (Option<u64>, Option<u64>) {
        // Namnet kommer från `container_name(server_id)` i `docker.rs`: bara
        // `craftpilot-` plus hex, så det kan stå i sökvägen utan encoding.
        let Some(docker) = &self.client else {
            return (None, None);
        };

        // `one-shot` (API ≥ 1.41) hoppar över den extra mätcykeln. En äldre
        // daemon kan ändå skicka två objekt efter varandra; då gäller det
        // sista, precis som i kontraktet.
        let options = StatsOptions {
            stream: false,
            one_shot: true,
        };

        let measurement = async {
            let mut stream = Box::pin(docker.stats(container, Some(options)));
            let mut last = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(stats) => last = Some(stats),
                    Err(err) => {
                        debug!(error = %err, "docker stats request failed");
                        return None;
                    }
                }
            }
            last.and_then(|stats| sum_networks(stats.networks))
        };

        // Även om daemonen ignorerar `stream=false` och håller svaret öppet
        // ska rapporten gå ut i tid.
        match tokio::time::timeout(REQUEST_TIMEOUT, measurement).await {
            Ok(Some((rx, tx))) => (Some(rx), Some(tx)),
            Ok(None) => {
                debug!("docker stats carried no per-interface counters");
                (None, None)
            }
            Err(_) => {
                debug!("reading docker stats timed out");
                (None, None)
            }
        }
    }
}

fn host_target(host: Option<&str>) -> HostTarget {
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return HostTarget::Unix(DEFAULT_SOCKET.to_string());
    };

    if let Some(path) = host.strip_prefix("unix://") {
        return HostTarget::Unix(path.to_string());
    }
    if let Some(address) = host.strip_prefix("tcp://") {
        return HostTarget::Http(format!("http://{address}"));
    }
    if host.starts_with("http://") {
        return HostTarget::Http(host.to_string());
    }
    // `https://` kräver TLS-konfiguration som agenten inte har; att skicka
    // klartext dit vore värre än att avstå.
    if host.starts_with('/') {
        return HostTarget::Unix(host.to_string());
    }

    debug!(docker_host = %host, "unsupported docker host; network counters stay empty");
    HostTarget::Unsupported
}

fn connect(target: &HostTarget) -> Option<Docker> {
    let client = match target {
        HostTarget::Unix(path) => connect_unix(path),
        HostTarget::Http(address) => {
            Docker::connect_with_http(address, REQUEST_TIMEOUT.as_secs(), API_DEFAULT_VERSION).ok()
        }
        HostTarget::Unsupported => None,
    };

    if client.is_none() {
        debug!(target = ?target, "the docker daemon is unreachable; network counters stay empty");
    }
    client
}

#[cfg(unix)]
fn connect_unix(path: &str) -> Option<Docker> {
    Docker::connect_with_unix(path, REQUEST_TIMEOUT.as_secs(), API_DEFAULT_VERSION).ok()
}

#[cfg(not(unix))]
fn connect_unix(_path: &str) -> Option<Docker> {
    // Socketar finns inte på den här plattformen; maskinen kör agenten i en
    // Linux-container, där grenen ovan används.
    None
}

/// Summerar `networks.*.rx_bytes`/`tx_bytes`. En server kan sitta på flera
/// nätverk (spelarnas och proxyns), så en enskild `eth0` underskattar
/// trafiken. Saknade räknare räknas som noll; saknas hela `networks`
/// (t.ex. `--network host`) finns inget att mäta och svaret är `None`.
fn sum_networks(networks: Option<HashMap<String, ContainerNetworkStats>>) -> Option<(u64, u64)> {
    let networks = networks?;
    if networks.is_empty() {
        return None;
    }

    let mut rx = 0u64;
    let mut tx = 0u64;
    for interface in networks.values() {
        rx = rx.saturating_add(interface.rx_bytes.unwrap_or(0));
        tx = tx.saturating_add(interface.tx_bytes.unwrap_or(0));
    }

    Some((rx, tx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn parse_stats(json: &str) -> bollard::models::ContainerStatsResponse {
        serde_json::from_str(json).expect("a docker stats object")
    }

    fn ok_body(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// En engångs-HTTP-server: svarar med `response` och lämnar tillbaka den
    /// råa förfrågan så testet kan granska sökvägen och query-parametrarna.
    async fn mock_daemon(response: Vec<u8>) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return String::new();
            };
            // Läs hela huvudblocket: en förfrågan kan komma i flera paket.
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let _ = stream.write_all(&response).await;
            String::from_utf8_lossy(&request).to_string()
        });
        (port, server)
    }

    fn api_for(port: u16) -> DockerApi {
        DockerApi::from_docker_host(Some(&format!("tcp://127.0.0.1:{port}")))
    }

    #[test]
    fn sums_every_interface() {
        let stats = parse_stats(
            r#"{"networks":{
                "eth0":{"rx_bytes":21084946999,"tx_bytes":933300257},
                "eth5":{"rx_bytes":4641,"tx_bytes":690}
            }}"#,
        );

        assert_eq!(
            sum_networks(stats.networks),
            Some((21_084_951_640, 933_300_947))
        );
    }

    #[test]
    fn a_missing_or_empty_network_map_yields_nothing() {
        assert_eq!(
            sum_networks(parse_stats(r#"{"read":"2026-11-18T14:00:00Z"}"#).networks),
            None
        );
        assert_eq!(
            sum_networks(parse_stats(r#"{"networks":{}}"#).networks),
            None
        );
        assert_eq!(sum_networks(None), None);
    }

    #[test]
    fn a_missing_counter_counts_as_zero() {
        let stats = parse_stats(r#"{"networks":{"eth0":{"rx_bytes":12},"eth5":{"tx_bytes":3}}}"#);

        assert_eq!(sum_networks(stats.networks), Some((12, 3)));
    }

    #[test]
    fn maps_the_docker_host_to_a_target() {
        assert_eq!(
            host_target(None),
            HostTarget::Unix(DEFAULT_SOCKET.to_string())
        );
        assert_eq!(
            host_target(Some("  ")),
            HostTarget::Unix(DEFAULT_SOCKET.to_string())
        );
        assert_eq!(
            host_target(Some("unix:///run/docker.sock")),
            HostTarget::Unix("/run/docker.sock".to_string())
        );
        assert_eq!(
            host_target(Some("/run/docker.sock")),
            HostTarget::Unix("/run/docker.sock".to_string())
        );
        assert_eq!(
            host_target(Some("tcp://127.0.0.1:2375")),
            HostTarget::Http("http://127.0.0.1:2375".to_string())
        );
        assert_eq!(
            host_target(Some("http://127.0.0.1:2375")),
            HostTarget::Http("http://127.0.0.1:2375".to_string())
        );
        assert_eq!(
            host_target(Some("https://docker.example")),
            HostTarget::Unsupported
        );
        assert_eq!(
            host_target(Some("npipe:////./pipe/docker_engine")),
            HostTarget::Unsupported
        );
    }

    #[test]
    fn a_missing_docker_socket_yields_no_client() {
        let api = DockerApi::from_docker_host(Some("unix:///nonexistent/craftpilot.sock"));

        assert!(api.client.is_none());
    }

    #[tokio::test]
    async fn reads_counters_from_the_daemon() {
        let body = r#"{"networks":{"eth0":{"rx_bytes":10,"tx_bytes":2},"eth5":{"rx_bytes":5,"tx_bytes":1}}}"#;
        let (port, server) = mock_daemon(ok_body(body)).await;

        assert_eq!(
            api_for(port).container_net_io("craftpilot-abc").await,
            (Some(15), Some(3))
        );

        let request = server.await.unwrap();
        assert!(
            request.starts_with("GET /containers/craftpilot-abc/stats?"),
            "request was: {request:?}"
        );
        assert!(request.contains("stream=false"));
        assert!(request.contains("one-shot=true"));
    }

    #[tokio::test]
    async fn takes_the_last_object_when_the_daemon_sends_two() {
        // Utan `one-shot` skickar en äldre daemon två samplingar, en per rad.
        let body = "{\"networks\":{\"eth0\":{\"rx_bytes\":1,\"tx_bytes\":1}}}\n\
                    {\"networks\":{\"eth0\":{\"rx_bytes\":9,\"tx_bytes\":4}}}\n";
        let (port, server) = mock_daemon(ok_body(body)).await;

        assert_eq!(
            api_for(port).container_net_io("craftpilot-abc").await,
            (Some(9), Some(4))
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_daemon_error_yields_no_counters() {
        let response =
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (port, server) = mock_daemon(response).await;

        assert_eq!(
            api_for(port).container_net_io("craftpilot-abc").await,
            (None, None)
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_daemon_yields_no_counters() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        assert_eq!(
            api_for(port).container_net_io("craftpilot-abc").await,
            (None, None)
        );
    }

    #[tokio::test]
    async fn a_hanging_daemon_yields_no_counters() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(stream);
        });

        assert_eq!(
            api_for(port).container_net_io("craftpilot-abc").await,
            (None, None)
        );

        server.abort();
    }
}
