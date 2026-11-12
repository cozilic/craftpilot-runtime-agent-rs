use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::error::{AgentError, Result};

/// Minecrafts standardport för RCON. Porten publiceras aldrig — agenten och
/// spelcontainern delar det isolerade Docker-nätverket.
pub const RCON_PORT: u16 = 25575;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PACKET: i32 = 64 * 1024;
const MAX_PROPERTIES: usize = 1024 * 1024;
const PROPERTY_ENABLE: &str = "enable-rcon";
const PROPERTY_PORT: &str = "rcon.port";
const PROPERTY_PASSWORD: &str = "rcon.password";

/// Det agenten behöver för att nå en servers RCON: porten från
/// `server.properties` och lösenordet från state-filen.
#[derive(Debug, Clone)]
pub struct RconSettings {
    pub port: u16,
    pub password: String,
}

/// Lösenordet genereras en gång per server och sparas i agentens state
/// (0600), precis som Python-agenten gör. Det loggas aldrig.
pub fn generate_password() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // Utan slumpkälla vill vi hellre ha ett svagt lösenord än ingen
        // RCON alls: porten är ändå bara nåbar från det privata nätet.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        return format!("{now:032x}");
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Servern är Java om startkommandot kör `java` eller en `.jar`. Pumpkin har
/// ingen `server.properties` och får behålla SIGTERM-vägen.
pub fn payload_is_java(payload: &Value) -> bool {
    payload
        .get("container")
        .and_then(|container| container.get("command"))
        .and_then(Value::as_array)
        .is_some_and(|command| {
            // Mastern skickar ofta `sh -c "… exec 'java' … server.jar"`, så
            // java kan ligga inbäddat i skriptet i stället för som eget argv.
            command
                .iter()
                .filter_map(Value::as_str)
                .any(|arg| arg.contains("java") || arg.contains(".jar"))
        })
}

/// Läser RCON-porten ur `server.properties`. `None` betyder att RCON är av
/// eller att filen saknas, och då hoppar STOP direkt till SIGTERM.
pub fn read_port(dir: &Path) -> Option<u16> {
    let text = std::fs::read(dir.join("server.properties")).ok()?;
    if text.len() > MAX_PROPERTIES {
        return None;
    }
    let text = String::from_utf8_lossy(&text);

    let mut enabled = false;
    let mut port = RCON_PORT;
    for line in text.lines() {
        let Some((key, value)) = property(line) else {
            continue;
        };
        match key {
            PROPERTY_ENABLE => enabled = value.eq_ignore_ascii_case("true"),
            PROPERTY_PORT => {
                if let Ok(parsed) = value.parse::<u16>() {
                    if parsed > 0 {
                        port = parsed;
                    }
                }
            }
            _ => {}
        }
    }

    enabled.then_some(port)
}

/// Slår på RCON i `server.properties` och sätter det sparade lösenordet.
/// Andra nycklar, kommentarer och radbrytningstyp bevaras; filen skrivs via
/// temp + rename och behåller ägare och mode så spelcontainern kan läsa den.
///
/// Returnerar porten som servern kommer att lyssna på.
pub fn configure(dir: &Path, password: &str) -> Result<u16> {
    let path = dir.join("server.properties");
    let existing = match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() > MAX_PROPERTIES {
                return Err(AgentError::Runtime(
                    "server.properties is too large to configure RCON".into(),
                ));
            }
            Some(bytes)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };

    let original = existing
        .as_deref()
        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
        .unwrap_or_default();
    let newline = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut lines: Vec<String> = original
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    set_property(&mut lines, PROPERTY_ENABLE, "true");
    set_property(&mut lines, PROPERTY_PORT, &RCON_PORT.to_string());
    set_property(&mut lines, PROPERTY_PASSWORD, password);

    let mut text = lines.join(newline);
    text.push_str(newline);

    let temporary = dir.join("server.properties.craftpilot.tmp");
    {
        use std::io::Write;

        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    copy_metadata(&path, &temporary)?;
    std::fs::rename(&temporary, &path)?;

    Ok(RCON_PORT)
}

/// Skickar `stop` över RCON. Autentiseringsfel och tidsgränser blir ett fel
/// så att anroparen kan falla tillbaka på `docker stop` (SIGTERM).
pub async fn send_stop(host: &str, port: u16, password: &str) -> Result<()> {
    let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| AgentError::Runtime("the rcon connection timed out".into()))?
        .map_err(|err| AgentError::Runtime(format!("could not reach rcon: {err}")))?;

    timeout(CONNECT_TIMEOUT, stream.write_all(&packet(1, 3, password)))
        .await
        .map_err(|_| AgentError::Runtime("the rcon handshake timed out".into()))??;

    let (id, _kind, _body) = timeout(CONNECT_TIMEOUT, read_packet(&mut stream))
        .await
        .map_err(|_| AgentError::Runtime("the rcon handshake timed out".into()))??;
    if id == -1 {
        return Err(AgentError::Runtime("the rcon password was rejected".into()));
    }

    timeout(COMMAND_TIMEOUT, stream.write_all(&packet(2, 2, "stop")))
        .await
        .map_err(|_| AgentError::Runtime("sending the rcon stop timed out".into()))??;

    // Servern kan stänga anslutningen innan svaret hinner skrivas; kommandot
    // är levererat och STOP verifierar ändå att containern faktiskt stannar.
    let _ = timeout(COMMAND_TIMEOUT, read_packet(&mut stream)).await;

    Ok(())
}

fn property(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

fn set_property(lines: &mut Vec<String>, key: &str, value: &str) {
    let replacement = format!("{key}={value}");
    for line in lines.iter_mut() {
        if property(line).is_some_and(|(existing, _)| existing == key) {
            *line = replacement;
            return;
        }
    }
    lines.push(replacement);
}

fn packet(id: i32, kind: i32, body: &str) -> Vec<u8> {
    // längd = id(4) + typ(4) + body + två avslutande NUL
    let length = 10 + body.len() as i32;
    let mut out = Vec::with_capacity(4 + length as usize);
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

async fn read_packet(stream: &mut TcpStream) -> Result<(i32, i32, String)> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    let length = i32::from_le_bytes(header);
    if !(10..=MAX_PACKET).contains(&length) {
        return Err(AgentError::Runtime(
            "rcon returned an invalid packet length".into(),
        ));
    }

    let mut body = vec![0u8; length as usize];
    stream.read_exact(&mut body).await?;
    let id = i32::from_le_bytes(body[0..4].try_into().expect("four bytes"));
    let kind = i32::from_le_bytes(body[4..8].try_into().expect("four bytes"));
    let text = String::from_utf8_lossy(&body[8..length as usize - 2]).to_string();
    Ok((id, kind, text))
}

/// Nya filen skapas av agenten (ofta root) medan spelcontainern kör som uid
/// 1000 — utan ägare/mode från originalet kan Minecraft inte läsa den.
fn copy_metadata(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{chown, MetadataExt, PermissionsExt};

        if let Ok(meta) = std::fs::metadata(source) {
            let mut permissions = std::fs::metadata(target)?.permissions();
            permissions.set_mode(meta.permissions().mode() & 0o777);
            std::fs::set_permissions(target, permissions)?;
            let _ = chown(target, Some(meta.uid()), Some(meta.gid()));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn temporary_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-rcon-test-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn configures_a_missing_properties_file() {
        let dir = temporary_dir("missing");
        assert_eq!(configure(&dir, "secret").unwrap(), RCON_PORT);

        let text = std::fs::read_to_string(dir.join("server.properties")).unwrap();
        assert!(text.contains("enable-rcon=true"));
        assert!(text.contains("rcon.port=25575"));
        assert!(text.contains("rcon.password=secret"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_other_keys_and_comments() {
        let dir = temporary_dir("keep");
        std::fs::write(
            dir.join("server.properties"),
            "motd=Hej\n# enable-rcon=false\nenable-rcon=false\nrcon.port=25576\n",
        )
        .unwrap();

        configure(&dir, "secret").unwrap();
        let text = std::fs::read_to_string(dir.join("server.properties")).unwrap();

        assert!(text.contains("motd=Hej"));
        assert!(text.contains("# enable-rcon=false"));
        assert!(text.contains("enable-rcon=true"));
        // Kommentaren bevaras, men ingen aktiv nyckel får vara kvar på false.
        assert!(!text.lines().any(|line| line.trim() == "enable-rcon=false"));
        assert!(text.contains("rcon.port=25575"));
        assert!(text.contains("rcon.password=secret"));
        assert_eq!(read_port(&dir), Some(RCON_PORT));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_port_requires_rcon_to_be_enabled() {
        let dir = temporary_dir("enabled");
        std::fs::write(dir.join("server.properties"), "enable-rcon=false\n").unwrap();
        assert_eq!(read_port(&dir), None);

        std::fs::write(
            dir.join("server.properties"),
            "enable-rcon=true\nrcon.port=25599\n",
        )
        .unwrap();
        assert_eq!(read_port(&dir), Some(25599));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_java_containers() {
        assert!(payload_is_java(&json!({
            "container": {"command": ["java", "-jar", "server.jar", "nogui"]}
        })));
        assert!(payload_is_java(&json!({
            "container": {"command": ["/opt/java/bin/java", "-jar", "server.jar"]}
        })));
        assert!(payload_is_java(&json!({
            "container": {"command": ["sh", "-c", "cd '/data' || exit 1; exec 'java' '-jar' 'server.jar' 'nogui'"]}
        })));
        assert!(!payload_is_java(&json!({
            "container": {"command": ["/pumpkin", "--port", "25565"]}
        })));
        assert!(!payload_is_java(&json!({})));
    }

    #[tokio::test]
    async fn sends_stop_after_authenticating() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            let (id, kind, body) = read_packet(&mut socket).await.unwrap();
            assert_eq!((id, kind, body.as_str()), (1, 3, "secret"));
            socket.write_all(&packet(1, 2, "")).await.unwrap();

            let (id, kind, body) = read_packet(&mut socket).await.unwrap();
            assert_eq!((id, kind, body.as_str()), (2, 2, "stop"));
            socket
                .write_all(&packet(2, 0, "Stopping the server"))
                .await
                .unwrap();
        });

        send_stop("127.0.0.1", address.port(), "secret")
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reports_a_rejected_password() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_packet(&mut socket).await.unwrap();
            socket.write_all(&packet(-1, 2, "")).await.unwrap();
        });

        let error = send_stop("127.0.0.1", address.port(), "wrong")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("rejected"));
        server.await.unwrap();
    }
}
