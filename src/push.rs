//! Nodens push-kanal över Reverb (2026-11-25).
//!
//! Mastern publicerar `runtime.node.wake` på nodens egen privata kanal när
//! ett kommando köas. Agenten prenumererar med sin credential och väcker
//! kommandoloopet direkt, så en konsolrad eller regelgrindens prompt slipper
//! vänta ut poll-intervallet.
//!
//! Pushen är bara en signal. Kommandon hämtas alltid med ett vanligt claim
//! över HTTP, leasen och acken är oförändrade, och en tappad anslutning
//! betyder bara att agenten pollar som förut — därför startar loopen aldrig
//! om agenten, den loggar och backar av.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use crate::api::MasterClient;
use crate::model::RealtimeConfig;

/// Eventnamnet mastern publicerar när nodkön fått en ny rad.
const WAKE_EVENT: &str = "runtime.node.wake";
const CONNECT_TIMEOUT_SECONDS: u64 = 15;
const MAX_BACKOFF_SECONDS: u64 = 60;

/// Vad en inkommande ram betyder för agenten.
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    Wake,
    Ping,
    Ignore,
}

/// Håller prenumerationen öppen och väcker kommandoloopet på varje
/// `runtime.node.wake`. Kör tills processen avslutas.
pub async fn run(cfg: RealtimeConfig, node_id: String, api: MasterClient, wake: Arc<Notify>) {
    let mut backoff = 1u64;

    loop {
        match session(&cfg, &node_id, &api, &wake).await {
            Ok(()) => backoff = 1,
            Err(err) => warn!(
                error = %err,
                retry_in_seconds = backoff,
                "push connection failed; the command poll keeps running"
            ),
        }

        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF_SECONDS);
    }
}

/// En anslutning: handskakning, signering av kanalen, prenumeration, och
/// sedan ramar tills anslutningen stängs. `Ok` betyder att servern stängde
/// snyggt (ingen varning), `Err` att något gick sönder.
async fn session(
    cfg: &RealtimeConfig,
    node_id: &str,
    api: &MasterClient,
    wake: &Notify,
) -> Result<(), String> {
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECONDS),
        connect_async(cfg.url.as_str()),
    )
    .await
    .map_err(|_| "the push connection timed out".to_string())?
    .map_err(|err| err.to_string())?;

    let mut subscribed = false;

    while let Some(message) = socket.next().await {
        let message = message.map_err(|err| err.to_string())?;
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Message::Ping(payload) => {
                socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|err| err.to_string())?;
                continue;
            }
            Message::Close(_) => return Ok(()),
            _ => continue,
        };

        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        // Pushers handskakning bär socket-id:t som signaturen kräver.
        if !subscribed {
            if let Some(id) = socket_id(&value) {
                let auth = api
                    .broadcast_auth(node_id, &cfg.channel, &id)
                    .await
                    .map_err(|err| format!("channel signature failed: {err}"))?;
                socket
                    .send(Message::Text(subscribe_frame(&cfg.channel, &auth)))
                    .await
                    .map_err(|err| err.to_string())?;
                subscribed = true;
                info!(channel = %cfg.channel, "subscribed to the runtime node push channel");
                continue;
            }
        }

        match classify(&value) {
            Frame::Wake => {
                debug!("runtime node wake received");
                wake.notify_one();
            }
            Frame::Ping => {
                socket
                    .send(Message::Text(pong_frame()))
                    .await
                    .map_err(|err| err.to_string())?;
            }
            Frame::Ignore => {}
        }
    }

    Ok(())
}

/// Bara ping och väckningen har en åtgärd. Prenumerationskvittensen och
/// framtida masternyheter ignoreras, så en ny händelse på kanalen aldrig
/// får agenten att tro att något gick sönder.
fn classify(value: &Value) -> Frame {
    match value.get("event").and_then(Value::as_str) {
        Some(WAKE_EVENT) => Frame::Wake,
        Some("pusher:ping") => Frame::Ping,
        _ => Frame::Ignore,
    }
}

/// Pushers `pusher:connection_established` bär socket-id:t som en JSON-sträng
/// i `data` (inte som objekt), men båda formerna läses.
fn socket_id(value: &Value) -> Option<String> {
    let data = value.get("data")?;
    let parsed: Value = match data {
        Value::String(raw) => serde_json::from_str(raw).ok()?,
        other => other.clone(),
    };

    parsed
        .get("socket_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn subscribe_frame(channel: &str, auth: &str) -> String {
    json!({
        "event": "pusher:subscribe",
        "data": { "auth": auth, "channel": channel },
    })
    .to_string()
}

fn pong_frame() -> String {
    json!({ "event": "pusher:pong", "data": {} }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::accept_async;
    use url::Url;

    #[test]
    fn a_wake_frame_is_a_wake() {
        assert_eq!(
            classify(&json!({"event": "runtime.node.wake", "data": {"node_id": "n"}})),
            Frame::Wake
        );
    }

    #[test]
    fn a_ping_is_answered_and_everything_else_is_ignored() {
        assert_eq!(classify(&json!({"event": "pusher:ping"})), Frame::Ping);
        assert_eq!(
            classify(&json!({"event": "pusher_internal:subscription_succeeded"})),
            Frame::Ignore
        );
        assert_eq!(
            classify(&json!({"event": "runtime.node.other"})),
            Frame::Ignore
        );
        assert_eq!(classify(&json!({})), Frame::Ignore);
    }

    #[test]
    fn the_socket_id_is_read_from_both_handshake_shapes() {
        let as_string = json!({
            "event": "pusher:connection_established",
            "data": "{\"socket_id\":\"1234.5678\",\"activity_timeout\":30}",
        });
        let as_object = json!({
            "event": "pusher:connection_established",
            "data": {"socket_id": "9.10"},
        });

        assert_eq!(socket_id(&as_string), Some("1234.5678".to_string()));
        assert_eq!(socket_id(&as_object), Some("9.10".to_string()));
        assert_eq!(socket_id(&json!({"event": "x"})), None);
    }

    #[test]
    fn the_subscribe_frame_carries_the_signed_channel() {
        let frame: Value =
            serde_json::from_str(&subscribe_frame("private-runtime-node.n1", "key:sig")).unwrap();

        assert_eq!(frame["event"], "pusher:subscribe");
        assert_eq!(frame["data"]["channel"], "private-runtime-node.n1");
        assert_eq!(frame["data"]["auth"], "key:sig");
    }

    /// Hela vägen: handskakning → signering mot mastern → prenumeration →
    /// en väckning som når `Notify`. Reverb och mastern är lokala mockar.
    #[tokio::test]
    async fn a_wake_from_the_channel_notifies_the_command_loop() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());

        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(socket).await.unwrap();

            socket
                .send(Message::Text(
                    json!({
                        "event": "pusher:connection_established",
                        "data": "{\"socket_id\":\"111.222\"}",
                    })
                    .to_string(),
                ))
                .await
                .unwrap();

            let subscribe = socket.next().await.unwrap().unwrap();
            let Message::Text(subscribe) = subscribe else {
                panic!("expected a text frame");
            };
            let subscribe: Value = serde_json::from_str(&subscribe).unwrap();
            assert_eq!(subscribe["event"], "pusher:subscribe");
            assert_eq!(subscribe["data"]["auth"], "test-key:test-signature");

            socket
                .send(Message::Text(
                    json!({"event": "runtime.node.wake", "channel": "private-runtime-node.n1"})
                        .to_string(),
                ))
                .await
                .unwrap();

            // Håll anslutningen öppen tills klienten stänger den.
            let _ = socket.next().await;
        });

        let (auth_base, auth_task) = auth_master().await;
        let mut api = MasterClient::new(auth_base).unwrap();
        api.set_credential(Some(
            "credential-0123456789abcdefghijklmnopqrst".to_string(),
        ));

        let wake = Arc::new(Notify::new());
        let config = RealtimeConfig {
            url,
            channel: "private-runtime-node.n1".to_string(),
        };
        let client = tokio::spawn({
            let wake = Arc::clone(&wake);
            async move { session(&config, "n1", &api, &wake).await }
        });

        tokio::time::timeout(Duration::from_secs(5), wake.notified())
            .await
            .expect("the wake never reached the command loop");

        auth_task.abort();
        server.abort();
        client.abort();
    }

    /// Minimal master som bara svarar på kanalsigneringen. Signaturvärdet
    /// spelar ingen roll här — agenten skickar vidare det den får.
    async fn auth_master() -> (Url, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve_auth(socket));
            }
        });

        (
            Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            task,
        )
    }

    async fn serve_auth(mut socket: TcpStream) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
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

        let body = r#"{"auth":"test-key:test-signature"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = header_end;
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    }
}
