use std::path::PathBuf;

use url::Url;

use crate::error::{AgentError, Result};

/// Miljövariablerna har samma namn som Python-agentens, så en värd kan
/// byta binär utan att ändra sin `.env`.
#[derive(Debug, Clone)]
pub struct Config {
    /// HTTPS-origin utan path, query eller credentials.
    pub master_url: Url,
    /// Nodens UUID. Sätts bara när en redan skapad nod ska anslutas med
    /// en engångstoken; normal väg är själv-enrollment.
    pub node_id: Option<String>,
    pub registration_token: Option<String>,
    pub state_dir: PathBuf,
    pub server_data_root: PathBuf,
    pub agent_version: String,
    pub hostname: String,
    /// Agentens egen adress (2026-11-16): den IP panelen/proxyn når maskinen
    /// på. Rapporteras vid anmälan så claim-formuläret kan föreslå rätt värde
    /// i stället för att gissa `host.docker.internal`. `None` när adressen
    /// inte gick att avgöra — då utelämnas fältet (äldre beteende).
    pub reported_address: Option<String>,
    pub poll_seconds: u64,
    /// Docker-nätverket som spelcontainrarna ansluts till.
    pub docker_network: String,
    /// Exakta image-namn agenten får starta (masterns Java-lista + Pumpkin).
    pub allowed_images: Vec<String>,
    /// Hur länge `docker stop` får vänta på en frivillig avstängning.
    pub stop_grace_seconds: u64,
    /// Värdsökvägen som Docker daemon ser för serverdata (Docker Desktop
    /// behöver t.ex. `C:/Projects/...` i stället för containerns mount).
    pub docker_data_root: Option<String>,
    /// GID som spelcontainerns filer delas med (33 = Minecraft i panelens
    /// images). Används när en snapshot installeras.
    pub data_gid: u32,
    /// Samma gräns som masterns `server.files.max_edit_bytes`.
    pub max_edit_bytes: u64,
    /// Samma gräns som masterns `server.files.max_upload_bytes`.
    pub max_upload_bytes: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let master_url =
            parse_https_origin(&env_or("CRAFTPILOT_MASTER_URL", "https://craftpilot.se"))?;

        let poll_seconds = parse_u64("CRAFTPILOT_POLL_SECONDS", 5)?;
        if !(5..=120).contains(&poll_seconds) {
            return Err(AgentError::Config(
                "CRAFTPILOT_POLL_SECONDS must be between 5 and 120".into(),
            ));
        }

        let server_data_root = PathBuf::from(env_or(
            "CRAFTPILOT_SERVER_DATA_ROOT",
            "/srv/craftpilot/servers",
        ));
        if !server_data_root.is_absolute() {
            return Err(AgentError::Config(
                "CRAFTPILOT_SERVER_DATA_ROOT must be an absolute path".into(),
            ));
        }

        let docker_network = env_or("CRAFTPILOT_DOCKER_NETWORK", "craftpilot-servers");
        if !is_docker_name(&docker_network) {
            return Err(AgentError::Config(
                "CRAFTPILOT_DOCKER_NETWORK is not a valid Docker network name".into(),
            ));
        }

        let stop_grace_seconds = parse_u64("CRAFTPILOT_STOP_GRACE_SECONDS", 30)?;
        if stop_grace_seconds > 45 {
            return Err(AgentError::Config(
                "CRAFTPILOT_STOP_GRACE_SECONDS must be 0-45".into(),
            ));
        }

        Ok(Self {
            master_url,
            node_id: optional_env("CRAFTPILOT_NODE_ID"),
            registration_token: optional_env("CRAFTPILOT_REGISTRATION_TOKEN"),
            state_dir: PathBuf::from(env_or(
                "CRAFTPILOT_AGENT_STATE_DIR",
                "/var/lib/craftpilot-agent",
            )),
            server_data_root,
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: detect_hostname(),
            reported_address: detect_host_address(),
            poll_seconds,
            docker_network,
            allowed_images: parse_image_list("CRAFTPILOT_ALLOWED_IMAGES", DEFAULT_ALLOWED_IMAGES),
            stop_grace_seconds,
            docker_data_root: optional_env("CRAFTPILOT_DOCKER_SERVER_DATA_ROOT")
                .map(|root| root.trim_end_matches(['/', '\\']).to_string()),
            data_gid: parse_u32("CRAFTPILOT_DATA_GID", 33)?,
            max_edit_bytes: parse_u64("CRAFTPILOT_MAX_EDIT_BYTES", 4 * 1024 * 1024)?,
            max_upload_bytes: parse_u64("CRAFTPILOT_MAX_UPLOAD_BYTES", 2 * 1024 * 1024 * 1024)?,
        })
    }
}

/// Mastern kräver samma form som `ApproveRuntimeNodeEnrollmentAction`:
/// `https`, en host, tom path och inga credentials/query/fragment.
pub fn parse_https_origin(raw: &str) -> Result<Url> {
    let trimmed = raw.trim().trim_end_matches('/');
    let url = Url::parse(trimmed)
        .map_err(|err| AgentError::Config(format!("CRAFTPILOT_MASTER_URL: {err}")))?;

    if url.scheme() != "https" {
        return Err(AgentError::Config(
            "CRAFTPILOT_MASTER_URL must use https".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(AgentError::Config(
            "CRAFTPILOT_MASTER_URL must have a host".into(),
        ));
    }
    if !url.path().is_empty() && url.path() != "/" {
        return Err(AgentError::Config(
            "CRAFTPILOT_MASTER_URL must not contain a path".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AgentError::Config(
            "CRAFTPILOT_MASTER_URL must not contain a query or fragment".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AgentError::Config(
            "CRAFTPILOT_MASTER_URL must not embed credentials".into(),
        ));
    }

    Ok(url)
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => default.to_string(),
    }
}

fn optional_env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}

/// Samma bilder som masterns `server.java_images` plus den egna
/// Pumpkin-bilden (nativ binär utan JVM). En image utanför listan startas
/// aldrig, oavsett vad en kommandopayload säger.
pub const DEFAULT_ALLOWED_IMAGES: &[&str] = &[
    "eclipse-temurin:8-jre",
    "eclipse-temurin:11-jre",
    "eclipse-temurin:17-jre",
    "eclipse-temurin:21-jre",
    "eclipse-temurin:25-jre",
    "craftpilot-pumpkin:latest",
];

fn parse_image_list(key: &str, default: &[&str]) -> Vec<String> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value
            .split(',')
            .map(str::trim)
            .filter(|image| !image.is_empty())
            .map(str::to_string)
            .collect(),
        _ => default.iter().map(|image| image.to_string()).collect(),
    }
}

/// Docker tillåter punkt, bindestreck och understreck i nätverksnamn.
fn is_docker_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

fn parse_u32(key: &str, default: u32) -> Result<u32> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u32>()
            .map_err(|_| AgentError::Config(format!("{key} must be an integer"))),
        _ => Ok(default),
    }
}

fn parse_u64(key: &str, default: u64) -> Result<u64> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .map_err(|_| AgentError::Config(format!("{key} must be an integer"))),
        _ => Ok(default),
    }
}

/// Agentens egen adress (2026-11-16). `CRAFTPILOT_HOST_ADDRESS` vinner — den
/// finns för värden med flera nätkort eller en publik IP som inte är den
/// utgående routen. Annars läses IP:n på det interface kerneln väljer för
/// utgående trafik: en UDP-socket som "ansluts" till en publik adress skickar
/// inga paket men tvingar fram en rutt, vilket ger maskinens LAN-IP.
///
/// Bara riktiga IP-adresser godtas, och loopback/länklokala avvisas — panelen
/// validerar värdet som adress, och ett felaktigt förslag är sämre än inget.
fn detect_host_address() -> Option<String> {
    if let Ok(raw) = std::env::var("CRAFTPILOT_HOST_ADDRESS") {
        if let Some(address) = sanitize_host_address(&raw) {
            return Some(address);
        }
    }

    // I en container är "den egna adressen" containerns brygga (172.x), som
    // varken panelen eller spelare når — sämre än panelens default. Kör
    // agenten direkt på värden (desktop-installationen) gäller auto-detekten;
    // i container får `CRAFTPILOT_HOST_ADDRESS` styra om något ska rapporteras.
    if std::path::Path::new("/.dockerenv").exists() {
        return None;
    }

    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    let address = socket.local_addr().ok()?.ip();

    sanitize_host_address(&address.to_string())
}

/// Normaliserar och godkänner en adresskandidat. `None` för allt som inte är
/// en användbar mottagaradress: loopback, `0.0.0.0`, broadcast, länklokal
/// (`169.254.0.0/16`, `fe80::/10`) och värdnamn (t.ex. `host.docker.internal`).
fn sanitize_host_address(raw: &str) -> Option<String> {
    let address: std::net::IpAddr = raw.trim().parse().ok()?;

    if address.is_loopback() || address.is_unspecified() {
        return None;
    }

    match address {
        std::net::IpAddr::V4(ipv4) => {
            if ipv4.is_link_local() || ipv4.is_broadcast() {
                return None;
            }
        }
        std::net::IpAddr::V6(ipv6) => {
            // fe80::/10 är länklokal och bara meningsfull på en specifik länk.
            if (ipv6.segments()[0] & 0xffc0) == 0xfe80 {
                return None;
            }
        }
    }

    Some(address.to_string())
}

/// `/etc/hostname` först (containern), sedan HOSTNAME/COMPUTERNAME.
/// Mastern validerar `^[A-Za-z0-9][A-Za-z0-9._-]*$`, så allt annat tas bort
/// och ett tomt värde faller tillbaka på ett neutralt namn.
fn detect_hostname() -> String {
    let raw = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_default();

    let sanitized: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        .take(255)
        .collect();

    match sanitized.chars().next() {
        Some(first) if first.is_ascii_alphanumeric() => sanitized,
        _ => "craftpilot-node".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_https_origin() {
        let url = parse_https_origin("https://craftpilot.se").unwrap();
        assert_eq!(url.as_str(), "https://craftpilot.se/");
    }

    #[test]
    fn accepts_a_trailing_slash_and_port() {
        let url = parse_https_origin("https://localhost:8443/").unwrap();
        assert_eq!(url.port(), Some(8443));
    }

    #[test]
    fn rejects_http() {
        assert!(parse_https_origin("http://craftpilot.se").is_err());
    }

    #[test]
    fn rejects_a_path() {
        assert!(parse_https_origin("https://craftpilot.se/api").is_err());
    }

    #[test]
    fn rejects_embedded_credentials() {
        assert!(parse_https_origin("https://user:pass@craftpilot.se").is_err());
    }

    #[test]
    fn rejects_an_invalid_docker_network() {
        assert!(is_docker_name("craftpilot-servers"));
        assert!(is_docker_name("cp_net.1"));
        assert!(!is_docker_name("craftpilot servers"));
        assert!(!is_docker_name(""));
    }

    #[test]
    fn reported_address_accepts_lan_and_public_ips() {
        // Både LAN- och publika adresser är användbara för panelen; det är
        // claim-formuläret som föreslår dem och ägaren som bekräftar.
        assert_eq!(
            sanitize_host_address(" 192.168.1.42 ").as_deref(),
            Some("192.168.1.42")
        );
        assert_eq!(
            sanitize_host_address("10.0.0.5").as_deref(),
            Some("10.0.0.5")
        );
        assert_eq!(
            sanitize_host_address("203.0.113.7").as_deref(),
            Some("203.0.113.7")
        );
        assert_eq!(
            sanitize_host_address("2001:db8::1").as_deref(),
            Some("2001:db8::1")
        );
    }

    #[test]
    fn reported_address_rejects_unusable_candidates() {
        // Loopback, ospecificerad, broadcast och länklokal når ingen annan
        // maskin; ett värdnamn (t.ex. host.docker.internal) är ingen IP alls.
        for raw in [
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "255.255.255.255",
            "169.254.10.1",
            "fe80::1",
            "host.docker.internal",
            "",
            "192.168.1.300",
        ] {
            assert_eq!(sanitize_host_address(raw), None, "{raw} ska avvisas");
        }
    }

    #[test]
    fn hostname_matches_the_master_regex() {
        let hostname = detect_hostname();
        let first = hostname.chars().next().unwrap_or(' ');
        assert!(first.is_ascii_alphanumeric());
        assert!(hostname
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')));
    }
}
