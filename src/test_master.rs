//! En fejk-master för agenttesterna: bara enrollment-vägen.
//!
//! Testerna ska kunna se att agenten fil:ar en ny anmälan och skriver ut en ny
//! claim-kod när mastern inte längre känner igen noden eller det sparade
//! ärendet, utan att starta en riktig master. Servern svarar en request per
//! anslutning och stänger den (`Connection: close`), så reqwest behöver inget
//! keep-alive.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use url::Url;

pub const NEW_REQUEST_ID: &str = "new-enrollment-request";
const NEW_REQUEST_SECRET: &str = "new-enrollment-secret";
pub const NEW_CLAIM_CODE: &str = "CP-NEW01-ABCDE";
pub const NEW_CREDENTIAL: &str = "credential-from-the-new-node";

/// Vad ett nytt ärende får för svar på sin första poll. `Approved` låter
/// testet köra hela anslutningen; `Pending` stannar agenten i väntan på en
/// ägare, precis som i drift, så testet kan läsa den färska koden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    Approved,
    Pending,
}

/// En request som fejk-mastern tog emot, så testet kan se i vilken ordning
/// agenten försökte förnya, fil:a och polla.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenRequest {
    pub method: String,
    pub path: String,
}

pub struct FakeMaster {
    base: Url,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    task: JoinHandle<()>,
}

impl FakeMaster {
    pub async fn start() -> Self {
        Self::start_with(PollOutcome::Approved).await
    }

    pub async fn start_waiting() -> Self {
        Self::start_with(PollOutcome::Pending).await
    }

    async fn start_with(outcome: PollOutcome) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn(accept(listener, Arc::clone(&seen), outcome));
        Self {
            base: Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            seen,
            task,
        }
    }

    /// Masterns bas-URL. Testadressen är http på loopback: `Config::for_test`
    /// går förbi https-kravet som gäller för en riktig master.
    pub fn url(&self) -> Url {
        self.base.clone()
    }

    pub fn requests(&self) -> Vec<SeenRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for FakeMaster {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept(listener: TcpListener, seen: Arc<Mutex<Vec<SeenRequest>>>, outcome: PollOutcome) {
    loop {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let seen = Arc::clone(&seen);
        tokio::spawn(async move { serve(socket, seen, outcome).await });
    }
}

async fn serve(mut socket: TcpStream, seen: Arc<Mutex<Vec<SeenRequest>>>, outcome: PollOutcome) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];

    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    let headers: Vec<(&str, &str)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim(), value.trim()))
        .collect();
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let credential = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| value.strip_prefix("Bearer "))
        .map(str::to_string);

    // Kroppen kan ha kommit med i samma läsning som headern.
    let mut received = buffer.len() - header_end;
    while received < content_length {
        let read = match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        received += read;
    }

    seen.lock().unwrap().push(SeenRequest {
        method: method.clone(),
        path: path.clone(),
    });

    let (status, body) = response(&method, &path, credential.as_deref(), outcome);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

/// Mastern beter sig som en som tappat noden: den gamla credentialen och det
/// sparade ärendet ger 401, medan en ny anmälan får en kod. Den credential
/// som delas ut vid anslutningen accepteras, så en lyckad väg kan köras klart.
fn response(
    method: &str,
    path: &str,
    credential: Option<&str>,
    outcome: PollOutcome,
) -> (&'static str, String) {
    if path.ends_with("/heartbeat") {
        return if credential == Some(NEW_CREDENTIAL) {
            (
                "200 OK",
                json!({
                    "ok": true,
                    "heartbeat_interval_seconds": 30,
                    "agent_version": "test-agent",
                })
                .to_string(),
            )
        } else {
            unauthorized()
        };
    }

    if path.ends_with("/claim-code") {
        return unauthorized();
    }

    if method == "POST" && path == "/api/runtime/enrollments" {
        return (
            "201 Created",
            json!({
                "request_id": NEW_REQUEST_ID,
                "request_secret": NEW_REQUEST_SECRET,
                "claim_code": NEW_CLAIM_CODE,
                "claim_expires_at": "2026-11-17T20:00:00+00:00",
                "poll_interval_seconds": 15,
                "expires_at": "2026-11-18T20:00:00+00:00",
            })
            .to_string(),
        );
    }

    if path.ends_with("/poll") {
        if !path.contains(NEW_REQUEST_ID) {
            // Ett ärende mastern inte känner igen: samma 401 som en borttagen nod.
            return unauthorized();
        }

        return match outcome {
            PollOutcome::Approved => (
                "200 OK",
                json!({
                    "status": "approved",
                    "node_id": "8f14e45f-ceea-467f-a8b1-1a5f5f0c7a11",
                    "credential": NEW_CREDENTIAL,
                    "master_url": "https://craftpilot.test",
                })
                .to_string(),
            ),
            PollOutcome::Pending => ("200 OK", json!({"status": "pending"}).to_string()),
        };
    }

    if path.ends_with("/complete") {
        return ("200 OK", json!({"status": "completed"}).to_string());
    }

    ("404 Not Found", json!({"error": "NOT_FOUND"}).to_string())
}

fn unauthorized() -> (&'static str, String) {
    (
        "401 Unauthorized",
        json!({"error": "RUNTIME_NODE_UNAUTHENTICATED"}).to_string(),
    )
}
