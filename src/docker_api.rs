//! Docker Engine API för den data CLI:t inte kan ge (2026-11-20).
//!
//! Telemetrins nätverksräknare måste vara råa byte. `docker stats`-CLI:t
//! formaterar `NetIO` till tre värdesiffror ("353MB / 247MB"), så ett delta på
//! 40 kB blir antingen 0 eller ett hopp på ~100 kB. Räknarna läses därför ur
//! `/containers/{id}/stats`, samma endpoint som masterns kontrakt beskriver
//! (`docs/RUNTIME-NODE-NETWORK-TELEMETRY.md` i CraftPilot-repot).
//!
//! Ett fel här får aldrig stoppa en rapport: alla vägar som inte ger ett
//! mätvärde lämnar `(None, None)` och servern rapporteras som vanligt.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Deserializer, Value};
use tracing::debug;

/// Golvnivå som alla daemoner ≥ Docker 20.10 accepterar.
const API_VERSION: &str = "v1.41";
/// Kort tidsgräns: hellre tomma räknare i rapporten än en försenad rapport.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Ett statssvar är några kilobyte. Taket hindrar en trasig daemon från att
/// fylla agentens minne.
const MAX_BODY_BYTES: usize = 256 * 1024;
const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// Klient för de få Docker-anrop som behöver rådata. CLI:t används fortfarande
/// för allt annat (start, stopp, inspect, loggar).
pub struct DockerApi {
    target: Target,
}

enum Target {
    /// Docker-socketen (`DOCKER_HOST=unix://…` eller standardvägen).
    Socket(PathBuf),
    /// `DOCKER_HOST=tcp://…`: daemonen nås över HTTP i stället.
    Http {
        base: String,
        client: reqwest::Client,
    },
    /// `npipe://` och okända scheme: ingen läsning.
    Unsupported,
}

impl DockerApi {
    pub fn from_env() -> Self {
        Self::from_docker_host(std::env::var("DOCKER_HOST").ok())
    }

    fn from_docker_host(value: Option<String>) -> Self {
        let Some(host) = value
            .as_deref()
            .map(str::trim)
            .filter(|host| !host.is_empty())
        else {
            return Self {
                target: Target::Socket(PathBuf::from(DEFAULT_SOCKET)),
            };
        };

        if let Some(path) = host.strip_prefix("unix://") {
            return Self {
                target: Target::Socket(PathBuf::from(path)),
            };
        }
        if let Some(address) = host.strip_prefix("tcp://") {
            return Self {
                target: Target::http(format!("http://{address}")),
            };
        }
        if host.starts_with("http://") || host.starts_with("https://") {
            return Self {
                target: Target::http(host.to_string()),
            };
        }
        if host.starts_with('/') {
            return Self {
                target: Target::Socket(PathBuf::from(host)),
            };
        }

        debug!(docker_host = %host, "unsupported docker host; network counters stay empty");
        Self {
            target: Target::Unsupported,
        }
    }

    /// Containerns kumulativa (rx, tx) i byte, summerat över alla interface.
    /// `(None, None)` när daemonen inte kunde svara, containern saknas eller
    /// svaret inte bär några nätverksräknare.
    pub async fn container_net_io(&self, container: &str) -> (Option<u64>, Option<u64>) {
        // Namnet kommer från `container_name(server_id)` i `docker.rs`: bara
        // `craftpilot-` plus hex, så det kan stå i sökvägen utan encoding.
        let path = format!("{API_VERSION}/containers/{container}/stats?stream=false&one-shot=true");

        let body = match &self.target {
            Target::Socket(socket) => socket_get(socket, &format!("/{path}")).await,
            Target::Http { base, client } => {
                let url = format!("{}/{path}", base.trim_end_matches('/'));
                match client.get(url).timeout(REQUEST_TIMEOUT).send().await {
                    Ok(response) if response.status().is_success() => {
                        response.bytes().await.ok().map(|body| body.to_vec())
                    }
                    Ok(response) => {
                        debug!(status = %response.status(), "docker stats request was refused");
                        None
                    }
                    Err(err) => {
                        debug!(error = %err, "docker stats request failed");
                        None
                    }
                }
            }
            Target::Unsupported => None,
        };

        let Some(body) = body else {
            return (None, None);
        };

        match network_counters(&body) {
            Some((rx, tx)) => (Some(rx), Some(tx)),
            None => {
                debug!("docker stats carried no per-interface counters");
                (None, None)
            }
        }
    }
}

impl Target {
    fn http(base: String) -> Self {
        match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => Target::Http { base, client },
            Err(_) => Target::Unsupported,
        }
    }
}

#[cfg(unix)]
async fn socket_get(path: &Path, request_path: &str) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let request = format!(
        "GET {request_path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );

    let mut stream = match tokio::time::timeout(REQUEST_TIMEOUT, UnixStream::connect(path)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            debug!(error = %err, "could not connect to the docker socket");
            return None;
        }
        Err(_) => {
            debug!("connecting to the docker socket timed out");
            return None;
        }
    };

    tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .ok()?
        .ok()?;

    let mut raw = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = match tokio::time::timeout(REQUEST_TIMEOUT, stream.read(&mut buffer)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(read)) => read,
            Ok(Err(err)) => {
                debug!(error = %err, "reading docker stats failed");
                return None;
            }
            Err(_) => {
                debug!("reading docker stats timed out");
                return None;
            }
        };
        if raw.len() + read > MAX_BODY_BYTES {
            debug!("docker stats response was too large");
            return None;
        }
        raw.extend_from_slice(&buffer[..read]);
    }

    response_body(&raw)
}

#[cfg(not(unix))]
async fn socket_get(_path: &Path, _request_path: &str) -> Option<Vec<u8>> {
    // Socketar finns inte på den här plattformen; maskinen kör agenten i en
    // Linux-container, där grenen ovan används.
    None
}

/// Kroppen ur ett HTTP/1.1-svar, eller `None` när statusen inte är 200.
/// Docker kan svara med `Content-Length`, chunked eller en kropp som tar slut
/// när anslutningen stängs — alla tre hanteras.
fn response_body(raw: &[u8]) -> Option<Vec<u8>> {
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;

    let mut lines = head.split("\r\n");
    let mut status = lines.next()?.split_whitespace();
    status.next()?;
    if status.next()? != "200" {
        return None;
    }

    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
        if name == "content-length" {
            length = value.parse::<usize>().ok();
        }
    }

    let body = &raw[split + 4..];
    if chunked {
        return decode_chunked(body);
    }

    match length {
        Some(length) if body.len() >= length => Some(body[..length].to_vec()),
        Some(_) => None,
        None => Some(body.to_vec()),
    }
}

/// Chunked-kodning enligt RFC 7230: storlek i hex, `\r\n`, data, `\r\n` — och
/// en nollstorlek som avslutar.
fn decode_chunked(body: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::new();
    let mut rest = body;

    loop {
        let line_end = rest.windows(2).position(|window| window == b"\r\n")?;
        let size_line = std::str::from_utf8(&rest[..line_end]).ok()?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        rest = &rest[line_end + 2..];

        if size == 0 {
            return Some(decoded);
        }
        if rest.len() < size + 2 {
            return None;
        }
        decoded.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// Räknarna ur ett statssvar. `one-shot` ger ett objekt, men en äldre daemon
/// kan skicka två efter varandra — då gäller det sista.
fn network_counters(body: &[u8]) -> Option<(u64, u64)> {
    let stats = Deserializer::from_slice(body)
        .into_iter::<Value>()
        .filter_map(|entry| entry.ok())
        .last()?;
    sum_networks(&stats)
}

/// Summerar `networks.*.rx_bytes`/`tx_bytes`. En server kan sitta på flera
/// nätverk (spelarnas och proxyns), så en enskild `eth0` underskattar
/// trafiken. Saknade räknare räknas som noll; saknas hela `networks`
/// (t.ex. `--network host`) finns inget att mäta och svaret är `None`.
fn sum_networks(stats: &Value) -> Option<(u64, u64)> {
    let networks = stats.get("networks")?.as_object()?;
    if networks.is_empty() {
        return None;
    }

    let mut rx = 0u64;
    let mut tx = 0u64;
    for interface in networks.values() {
        rx = rx.saturating_add(
            interface
                .get("rx_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        tx = tx.saturating_add(
            interface
                .get("tx_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
    }

    Some((rx, tx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_body(json: &str) -> Vec<u8> {
        json.as_bytes().to_vec()
    }

    #[test]
    fn sums_every_interface() {
        let body = stats_body(
            r#"{"networks":{
                "eth0":{"rx_bytes":21084946999,"tx_bytes":933300257},
                "eth5":{"rx_bytes":4641,"tx_bytes":690}
            }}"#,
        );

        assert_eq!(network_counters(&body), Some((21_084_951_640, 933_300_947)));
    }

    #[test]
    fn a_missing_or_empty_network_map_yields_nothing() {
        assert_eq!(
            network_counters(&stats_body(r#"{"read":"2026-11-18T14:00:00Z"}"#)),
            None
        );
        assert_eq!(network_counters(&stats_body(r#"{"networks":{}}"#)), None);
        assert_eq!(network_counters(b""), None);
    }

    #[test]
    fn a_missing_counter_counts_as_zero() {
        let body = stats_body(r#"{"networks":{"eth0":{"rx_bytes":12},"eth5":{"tx_bytes":3}}}"#);

        assert_eq!(network_counters(&body), Some((12, 3)));
    }

    #[test]
    fn takes_the_last_object_when_the_daemon_sends_two() {
        let body = stats_body(
            r#"{"networks":{"eth0":{"rx_bytes":1,"tx_bytes":1}}}
               {"networks":{"eth0":{"rx_bytes":9,"tx_bytes":4}}}"#,
        );

        assert_eq!(network_counters(&body), Some((9, 4)));
    }

    #[test]
    fn decodes_a_chunked_response() {
        let body = r#"{"networks":{"eth0":{"rx_bytes":7,"tx_bytes":2}}}"#;
        let (first, second) = body.split_at(5);
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
            first.len(),
            second.len()
        );

        let decoded = response_body(raw.as_bytes()).expect("chunked body");
        assert_eq!(network_counters(&decoded), Some((7, 2)));
    }

    #[test]
    fn reads_a_content_length_body_exactly() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}trailing";

        assert_eq!(response_body(raw), Some(b"{}".to_vec()));
    }

    #[test]
    fn a_refused_request_has_no_body() {
        assert_eq!(
            response_body(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"),
            None
        );
        assert_eq!(
            response_body(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n"),
            None
        );
    }

    #[test]
    fn maps_the_docker_host_to_a_target() {
        let default = DockerApi::from_docker_host(None);
        assert!(
            matches!(&default.target, Target::Socket(path) if path == Path::new(DEFAULT_SOCKET))
        );

        let unix = DockerApi::from_docker_host(Some("unix:///run/docker.sock".into()));
        assert!(
            matches!(&unix.target, Target::Socket(path) if path == Path::new("/run/docker.sock"))
        );

        let tcp = DockerApi::from_docker_host(Some("tcp://127.0.0.1:2375".into()));
        assert!(
            matches!(&tcp.target, Target::Http { base, .. } if base == "http://127.0.0.1:2375")
        );

        let named_pipe = DockerApi::from_docker_host(Some("npipe:////./pipe/docker_engine".into()));
        assert!(matches!(named_pipe.target, Target::Unsupported));
    }

    #[cfg(unix)]
    fn socket_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-docker-api-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Lyssnaren binds före trådstarten: annars kan klienten koppla upp innan
    /// servern hunnit börja lyssna, och då väntar testet på en anslutning som
    /// aldrig kommer.
    #[cfg(unix)]
    fn serve(listener: tokio::net::UnixListener, response: Vec<u8>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0u8; 4096];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(&response).await;
        })
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reads_counters_from_the_docker_socket() {
        let dir = socket_dir("ok");
        let socket_path = dir.join("docker.sock");
        let body = r#"{"networks":{"eth0":{"rx_bytes":10,"tx_bytes":2},"eth5":{"rx_bytes":5,"tx_bytes":1}}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );

        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = serve(listener, response.into_bytes());

        let api = DockerApi::from_docker_host(Some(format!("unix://{}", socket_path.display())));
        assert_eq!(
            api.container_net_io("craftpilot-abc").await,
            (Some(15), Some(3))
        );

        server.await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_daemon_error_yields_no_counters() {
        let dir = socket_dir("error");
        let socket_path = dir.join("docker.sock");

        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = serve(
            listener,
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
        );

        let api = DockerApi::from_docker_host(Some(format!("unix://{}", socket_path.display())));
        assert_eq!(api.container_net_io("craftpilot-abc").await, (None, None));

        server.await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreachable_socket_yields_no_counters() {
        let dir = socket_dir("missing");
        let api = DockerApi::from_docker_host(Some(format!(
            "unix://{}",
            dir.join("absent.sock").display()
        )));

        assert_eq!(api.container_net_io("craftpilot-abc").await, (None, None));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
