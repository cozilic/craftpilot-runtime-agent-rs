//! Docker Engine API via bollard för telemetrins CPU, minne och
//! nätverksräknare (2026-11-22, utökat 2026-11-25).
//!
//! `docker stats`-CLI:t formaterar både `NetIO` och minnet till tre
//! värdesiffror ("353MB / 247MB"), så ett delta på 40 kB blir antingen 0
//! eller ett hopp på ~100 kB, och varje anrop kostade ~2 s per ONLINE-server.
//! Allt läses därför ur `/containers/{id}/stats` i EN bollard-läsning, samma
//! endpoint som masterns kontrakt beskriver
//! (`docs/RUNTIME-NODE-NETWORK-TELEMETRY.md` i CraftPilot-repot).
//!
//! Ett fel här får aldrig stoppa en rapport: alla vägar som inte ger ett
//! mätvärde lämnar `(None, None)` och servern rapporteras som vanligt.
//!
//! bollard 0.21 skickar sökvägen utan versionsprefix, så daemonen svarar med
//! sin egen API-version. Det är avsiktligt ofarligt: äldre daemoner saknar
//! `one-shot` och skickar då två samplingar, och läsaren tar den sista.

use std::time::Duration;

use bollard::query_parameters::StatsOptions;
use bollard::{Docker, API_DEFAULT_VERSION};
use futures_util::StreamExt;
use tracing::debug;

/// Kort tidsgräns: hellre tomma räknare i rapporten än en försenad rapport.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const MEGABYTE: u64 = 1024 * 1024;

/// Ett mätvärde ur `/containers/{id}/stats`: CPU-räknare, minne och nätverk i
/// samma läsning. Fält som daemonen inte gav är `None`; rapporten hoppar då
/// över dem.
///
/// CPU är råa kumulativa räknare, inte procent: `one-shot` lämnar
/// `precpu_stats` med nollor, så deltat måste räknas mot agentens FÖREGÅENDE
/// rapport (samma princip som nätverks-bps:en i mastern).
#[derive(Debug, Default, PartialEq)]
pub struct ContainerSample {
    pub cpu_total_usage: Option<u64>,
    pub system_cpu_usage: Option<u64>,
    pub online_cpus: Option<u64>,
    pub memory_used_mb: Option<u64>,
    pub memory_limit_mb: Option<u64>,
    pub network_rx_bytes: Option<u64>,
    pub network_tx_bytes: Option<u64>,
}
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

    /// CPU, minne och nätverksräknare i EN läsning av
    /// `/containers/{id}/stats` (2026-11-25). `docker stats --no-stream` tog
    /// ~2 s per ONLINE-server på Docker Desktop; samma endpoint svarar på
    /// millisekunder, och CPU/minne räknas fram med Docker Engine API:s egna
    /// formler (desamma som CLI:t använder).
    ///
    /// `None` betyder att daemonen inte svarade alls. Ett enskilt fält som
    /// saknas i svaret blir `None` i `ContainerSample`, så rapporten hoppar
    /// över just det i stället för att skicka en nolla som ser ut som en
    /// mätning.
    pub async fn container_sample(&self, container: &str) -> Option<ContainerSample> {
        // Namnet kommer från `container_name(server_id)` i `docker.rs`: bara
        // `craftpilot-` plus hex, så det kan stå i sökvägen utan encoding.
        let docker = self.client.as_ref()?;

        // `one-shot` (API ≥ 1.41) hoppar över den extra mätcykeln. En äldre
        // daemon kan ändå skicka två objekt efter varandra; då gäller det
        // sista, precis som i kontraktet.
        let options = StatsOptions {
            stream: false,
            one_shot: true,
        };

        let measurement = async {
            let mut stream = Box::pin(docker.stats(container, Some(options)));
            let mut sample = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(stats) => {
                        // CPU: råa kumulativa räknare. Procenten räknas i
                        // `telemetry.rs` mot förra rapporten, eftersom
                        // `one-shot` lämnar `precpu_stats` med nollor.
                        let (cpu_total_usage, system_cpu_usage, online_cpus) =
                            match stats.cpu_stats.as_ref() {
                                Some(cpu) => {
                                    let cpus = cpu.online_cpus.map(u64::from).or_else(|| {
                                        cpu.cpu_usage
                                            .as_ref()
                                            .and_then(|usage| usage.percpu_usage.as_ref())
                                            .map(|cores| cores.len() as u64)
                                    });
                                    (
                                        cpu.cpu_usage.as_ref().and_then(|usage| usage.total_usage),
                                        cpu.system_cpu_usage,
                                        cpus,
                                    )
                                }
                                None => (None, None, None),
                            };

                        // Minne: samma avdrag för sidcache som CLI:t gör
                        // (cgroups v2 `inactive_file`, v1 `total_inactive_file`).
                        let (memory_used_mb, memory_limit_mb) = match stats.memory_stats.as_ref() {
                            Some(memory) => {
                                let cache = memory
                                    .stats
                                    .as_ref()
                                    .and_then(|values| {
                                        values
                                            .get("inactive_file")
                                            .or_else(|| values.get("total_inactive_file"))
                                            .or_else(|| values.get("cache"))
                                            .copied()
                                    })
                                    .unwrap_or(0);
                                (
                                    memory
                                        .usage
                                        .map(|used| used.saturating_sub(cache) / MEGABYTE),
                                    memory.limit.map(|limit| limit / MEGABYTE),
                                )
                            }
                            None => (None, None),
                        };

                        // Nätverk: kumulativa räknare summerade över alla interface.
                        let (mut rx, mut tx) = (0u64, 0u64);
                        let mut has_network = false;
                        if let Some(networks) = stats.networks.as_ref() {
                            for interface in networks.values() {
                                has_network = true;
                                rx = rx.saturating_add(interface.rx_bytes.unwrap_or(0));
                                tx = tx.saturating_add(interface.tx_bytes.unwrap_or(0));
                            }
                        }

                        sample = Some(ContainerSample {
                            cpu_total_usage,
                            system_cpu_usage,
                            online_cpus,
                            memory_used_mb,
                            memory_limit_mb,
                            network_rx_bytes: has_network.then_some(rx),
                            network_tx_bytes: has_network.then_some(tx),
                        });
                    }
                    Err(err) => {
                        debug!(error = %err, "docker stats request failed");
                        return None;
                    }
                }
            }
            sample
        };

        // Även om daemonen ignorerar `stream=false` och håller svaret öppet
        // ska rapporten gå ut i tid.
        match tokio::time::timeout(REQUEST_TIMEOUT, measurement).await {
            Ok(sample) => sample,
            Err(_) => {
                debug!("reading docker stats timed out");
                None
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

/// Ett mätvärde täcker CPU, minne och nätverk; testerna matar en mockad
/// daemon med svar i den form `/containers/{id}/stats` ger.
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// Ett svar räcker: CPU enligt Engine API-formeln (delta/delta × kärnor),
    /// minne efter avdraget för `inactive_file`, och nätet summerat över alla
    /// interface. Sökvägen och query-parametrarna är desamma som förut, så
    /// detta är fortfarande EN läsning per server och varv.
    #[tokio::test]
    async fn reads_cpu_memory_and_networks_in_one_request() {
        // En rad: bollard läser statssvaret som en ström av JSON-objekt, ett
        // per rad, så ett flerradigt objekt tolkas som flera trasiga.
        let body = concat!(
            r#"{"cpu_stats":{"cpu_usage":{"total_usage":2000000000},"system_cpu_usage":20000000000,"online_cpus":2},"#,
            r#""precpu_stats":{"cpu_usage":{"total_usage":1000000000},"system_cpu_usage":10000000000},"#,
            r#""memory_stats":{"usage":314572800,"limit":1073741824,"stats":{"inactive_file":104857600}},"#,
            r#""networks":{"eth0":{"rx_bytes":10,"tx_bytes":2},"eth5":{"rx_bytes":5,"tx_bytes":1}}}"#
        );
        let (port, server) = mock_daemon(ok_body(body)).await;

        let sample = api_for(port)
            .container_sample("craftpilot-abc")
            .await
            .expect("a sample");

        // Råa räknare: procenten räknas mot föregående rapport i telemetrin.
        assert_eq!(sample.cpu_total_usage, Some(2_000_000_000));
        assert_eq!(sample.system_cpu_usage, Some(20_000_000_000));
        assert_eq!(sample.online_cpus, Some(2));
        assert_eq!(sample.memory_used_mb, Some(200));
        assert_eq!(sample.memory_limit_mb, Some(1024));
        assert_eq!(sample.network_rx_bytes, Some(15));
        assert_eq!(sample.network_tx_bytes, Some(3));

        let request = server.await.unwrap();
        assert!(
            request.starts_with("GET /containers/craftpilot-abc/stats?"),
            "request was: {request:?}"
        );
        assert!(request.contains("stream=false"));
        assert!(request.contains("one-shot=true"));
    }

    #[tokio::test]
    async fn missing_parts_are_left_empty_instead_of_zero() {
        // Ingen `precpu_stats` → ingen CPU-siffra; ingen minnesstatistik → inga
        // minnesfält; ingen `networks` (t.ex. `--network host`) → inga räknare.
        let body = r#"{"read":"2026-11-18T14:00:00Z"}"#;
        let (port, server) = mock_daemon(ok_body(body)).await;

        assert_eq!(
            api_for(port).container_sample("craftpilot-abc").await,
            Some(ContainerSample::default())
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn takes_the_last_object_when_the_daemon_sends_two() {
        // Utan `one-shot` skickar en äldre daemon två samplingar, en per rad.
        let body = "{\"networks\":{\"eth0\":{\"rx_bytes\":1,\"tx_bytes\":1}}}\n\
                    {\"networks\":{\"eth0\":{\"rx_bytes\":9,\"tx_bytes\":4}}}\n";
        let (port, server) = mock_daemon(ok_body(body)).await;

        let sample = api_for(port)
            .container_sample("craftpilot-abc")
            .await
            .expect("a sample");
        assert_eq!(
            (sample.network_rx_bytes, sample.network_tx_bytes),
            (Some(9), Some(4))
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_daemon_error_yields_no_sample() {
        let response =
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (port, server) = mock_daemon(response).await;

        assert_eq!(api_for(port).container_sample("craftpilot-abc").await, None);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_daemon_yields_no_sample() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        assert_eq!(api_for(port).container_sample("craftpilot-abc").await, None);
    }

    #[tokio::test]
    async fn a_hanging_daemon_yields_no_sample() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(stream);
        });

        assert_eq!(api_for(port).container_sample("craftpilot-abc").await, None);

        server.abort();
    }
}
