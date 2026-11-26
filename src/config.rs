use std::path::PathBuf;
use std::time::Duration;

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
    /// Hur ofta en väntande anmälan förnyar sin claim-kod. Koden lever 30 min
    /// hos mastern, så en anmälan som ligger kvar länge annars tappar koden och
    /// ägaren måste starta om agenten för att få en ny.
    pub claim_code_refresh_seconds: u64,
    /// Hur länge en avvisad anmälan inte får ersättas av en ny (24 h). Inkorgen
    /// ska inte fyllas med dubbletter medan sysadminen redan har sagt nej.
    pub enrollment_rejected_retry_seconds: u64,
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
    /// Nodens egen SFTP (docs/RUNTIME-NODE-SFTP.md).
    pub sftp: SftpSettings,
    /// Portfri anslutning (docs/RUNTIME-NODE-TUNNEL.md).
    pub tunnel: TunnelSettings,
}

/// Inställningar för nodens SFTPGo. Mastern äger *vad* som får finnas;
/// agenten äger *var* det finns och startar därför en egen SFTPGo per nod.
#[derive(Debug, Clone)]
pub struct SftpSettings {
    /// Av som default? Nej: en nod som uppgraderas ska börja erbjuda SFTP
    /// utan en extra flagga, och mastern visar bara slutpunkten när agenten
    /// faktiskt rapporterar `sftp`.
    pub enabled: bool,
    pub image: String,
    /// Porten som publiceras på nodens värd. Containerporten är alltid 2022.
    /// Är porten upptagen på värden publicerar agenten på nästa lediga och
    /// rapporterar den i stället (`sftp_bound_port`).
    pub port: u16,
    /// SFTPGo:s admin-API. Ligger på Docker-nätet och publiceras aldrig.
    pub admin_url: Url,
    pub container_name: String,
    /// Explicita ägare för filer SFTPGo skapar. `None` (default) läser
    /// serverkatalogens ägare, så filerna kan skrivas av spelets container
    /// även när noden kör en annan uid/gid än panelens images.
    pub file_uid: Option<u32>,
    pub file_gid: Option<u32>,
    pub admin_username: String,
}

pub const DEFAULT_SFTP_IMAGE: &str = "drakkan/sftpgo:v2.6.6";
pub const DEFAULT_SFTP_CONTAINER: &str = "craftpilot-sftpgo";
pub const DEFAULT_SFTP_ADMIN_USERNAME: &str = "craftpilot-agent";

/// Tunnelklienten (docs/RUNTIME-NODE-TUNNEL.md). Agenten startar frpc som en
/// barnprocess i sin egen container; mastern äger portarna och nekar allt som
/// inte är nodens eget.
#[derive(Debug, Clone)]
pub struct TunnelSettings {
    /// `CRAFTPILOT_TUNNEL`. Av som default? Nej: precis som SFTP ska en nod som
    /// uppgraderas kunna erbjuda tunneln utan en ny installation. Mastern
    /// svarar `enabled: false` tills ägaren slår på den i panelen.
    pub enabled: bool,
    /// frpc-binären. Dockerimagen installerar den pinnade versionen.
    pub frpc_bin: String,
    /// Adressen frpc når nodens publicerade portar på inifrån
    /// agentcontainern. Compose mappar `host.docker.internal` till
    /// värdens gateway; kör agenten direkt på värden fungerar 127.0.0.1.
    pub local_host: String,
    /// Privat katalog för `frpc.toml` och det pinnade CA:t.
    pub dir: PathBuf,
    /// Hur ofta masterns tunneläga hämtas. Kort nog att en avstängning
    /// släpper hubportarna snabbt, långt nog att inte allokera i onödan.
    pub refresh_interval: Duration,
}

impl Config {
    /// Minimal konfiguration för agenttesterna: bara masterns adress och
    /// state-katalogen varierar, resten är fasta värden.
    #[cfg(test)]
    pub fn for_test(master_url: Url, state_dir: PathBuf) -> Self {
        Self {
            master_url,
            node_id: None,
            registration_token: None,
            server_data_root: state_dir.join("servers"),
            agent_version: "test-agent".to_string(),
            hostname: "test-host".to_string(),
            reported_address: None,
            poll_seconds: 5,
            claim_code_refresh_seconds: 25 * 60,
            enrollment_rejected_retry_seconds: 24 * 60 * 60,
            docker_network: "craftpilot-servers".to_string(),
            allowed_images: Vec::new(),
            stop_grace_seconds: 1,
            docker_data_root: None,
            data_gid: 33,
            max_edit_bytes: 1024 * 1024,
            max_upload_bytes: 4 * 1024 * 1024,
            // Testerna ska inte starta varken SFTPGo eller frpc.
            tunnel: TunnelSettings {
                enabled: false,
                frpc_bin: "frpc".to_string(),
                local_host: "host.docker.internal".to_string(),
                dir: state_dir.join("tunnel"),
                refresh_interval: Duration::from_secs(10),
            },
            state_dir,
            // Testerna ska inte starta en riktig SFTPGo; modulerna testas
            // direkt mot en mockad admin-API.
            sftp: SftpSettings {
                enabled: false,
                image: DEFAULT_SFTP_IMAGE.to_string(),
                port: 2222,
                admin_url: Url::parse("http://craftpilot-sftpgo:8080/").unwrap(),
                container_name: DEFAULT_SFTP_CONTAINER.to_string(),
                file_uid: None,
                file_gid: None,
                admin_username: DEFAULT_SFTP_ADMIN_USERNAME.to_string(),
            },
        }
    }

    pub fn from_env() -> Result<Self> {
        let master_url = master_url_from(std::env::var("CRAFTPILOT_MASTER_URL").ok().as_deref())?;

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

        let sftp_enabled = parse_bool("CRAFTPILOT_SFTP_ENABLED", true)?;
        let sftp_image = env_or("CRAFTPILOT_SFTP_IMAGE", DEFAULT_SFTP_IMAGE);
        if sftp_image.is_empty()
            || sftp_image.len() > 255
            || sftp_image.chars().any(char::is_whitespace)
        {
            return Err(AgentError::Config(
                "CRAFTPILOT_SFTP_IMAGE is not a valid image reference".into(),
            ));
        }
        let sftp_port = parse_u64("CRAFTPILOT_SFTP_PORT", 2222)?;
        if !(1..=65535).contains(&sftp_port) {
            return Err(AgentError::Config(
                "CRAFTPILOT_SFTP_PORT must be 1-65535".into(),
            ));
        }
        let sftp_container_name = env_or("CRAFTPILOT_SFTP_CONTAINER", DEFAULT_SFTP_CONTAINER);
        if !is_docker_name(&sftp_container_name) {
            return Err(AgentError::Config(
                "CRAFTPILOT_SFTP_CONTAINER is not a valid Docker container name".into(),
            ));
        }
        let sftp_admin_url = parse_sftp_admin_url(&env_or(
            "CRAFTPILOT_SFTP_ADMIN_URL",
            "http://craftpilot-sftpgo:8080/",
        ))?;
        let data_gid = parse_u32("CRAFTPILOT_DATA_GID", 33)?;
        // Tomt/utelämnat = härleds per användare ur serverkatalogens ägare.
        let sftp_file_gid = optional_u32("CRAFTPILOT_SFTP_FILE_GID")?;
        let sftp_file_uid = optional_u32("CRAFTPILOT_SFTP_FILE_UID")?;

        let state_dir = PathBuf::from(env_or(
            "CRAFTPILOT_AGENT_STATE_DIR",
            "/var/lib/craftpilot-agent",
        ));
        let tunnel_enabled = parse_enabled_flag("CRAFTPILOT_TUNNEL", true)?;
        let frpc_bin = env_or("CRAFTPILOT_FRPC_BIN", "frpc");
        if frpc_bin.is_empty() || frpc_bin.chars().any(char::is_whitespace) {
            return Err(AgentError::Config(
                "CRAFTPILOT_FRPC_BIN must be a binary path without whitespace".into(),
            ));
        }
        let tunnel_local_host = match std::env::var("CRAFTPILOT_TUNNEL_LOCAL_HOST") {
            Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => default_tunnel_local_host(),
        };
        if !is_tunnel_host(&tunnel_local_host) {
            return Err(AgentError::Config(
                "CRAFTPILOT_TUNNEL_LOCAL_HOST is not a valid host".into(),
            ));
        }
        let tunnel_refresh_seconds = parse_u64("CRAFTPILOT_TUNNEL_REFRESH_SECONDS", 10)?;
        if !(5..=300).contains(&tunnel_refresh_seconds) {
            return Err(AgentError::Config(
                "CRAFTPILOT_TUNNEL_REFRESH_SECONDS must be between 5 and 300".into(),
            ));
        }

        Ok(Self {
            master_url,
            node_id: optional_env("CRAFTPILOT_NODE_ID"),
            registration_token: optional_env("CRAFTPILOT_REGISTRATION_TOKEN"),
            server_data_root,
            // Byggversionen (build.rs), inte Cargo-paketets platshållare.
            agent_version: crate::build_info::AGENT_VERSION.to_string(),
            hostname: detect_hostname(),
            reported_address: detect_host_address(),
            poll_seconds,
            claim_code_refresh_seconds: 25 * 60,
            enrollment_rejected_retry_seconds: 24 * 60 * 60,
            docker_network,
            allowed_images: parse_image_list("CRAFTPILOT_ALLOWED_IMAGES", DEFAULT_ALLOWED_IMAGES),
            stop_grace_seconds,
            docker_data_root: optional_env("CRAFTPILOT_DOCKER_SERVER_DATA_ROOT")
                .map(|root| root.trim_end_matches(['/', '\\']).to_string()),
            data_gid,
            max_edit_bytes: parse_u64("CRAFTPILOT_MAX_EDIT_BYTES", 4 * 1024 * 1024)?,
            max_upload_bytes: parse_u64("CRAFTPILOT_MAX_UPLOAD_BYTES", 2 * 1024 * 1024 * 1024)?,
            sftp: SftpSettings {
                enabled: sftp_enabled,
                image: sftp_image,
                port: sftp_port as u16,
                admin_url: sftp_admin_url,
                container_name: sftp_container_name,
                file_uid: sftp_file_uid,
                file_gid: sftp_file_gid,
                admin_username: DEFAULT_SFTP_ADMIN_USERNAME.to_string(),
            },
            state_dir: state_dir.clone(),
            tunnel: TunnelSettings {
                enabled: tunnel_enabled,
                frpc_bin,
                local_host: tunnel_local_host,
                dir: state_dir.join("tunnel"),
                refresh_interval: Duration::from_secs(tunnel_refresh_seconds),
            },
        })
    }
}

/// Masterns adress är obligatorisk. Förut föll en tom miljö tillbaka på
/// `https://craftpilot.se`, vilket gjorde att ett bare `docker run` kunde
/// anmäla sig mot produktion av misstag. Det finns ingen default.
fn master_url_from(raw: Option<&str>) -> Result<Url> {
    let raw = raw
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AgentError::Config(
                "CRAFTPILOT_MASTER_URL must be set (there is no default master)".into(),
            )
        })?;
    parse_https_origin(raw)
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

fn parse_bool(key: &str, default: bool) -> Result<bool> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(AgentError::Config(format!("{key} must be true or false"))),
        },
        _ => Ok(default),
    }
}

/// Admin-API:t är internt och talar klartext-HTTP på Docker-nätet. En
/// operator som pekar om det (agenten körs utanför nätet) får använda
/// https, men aldrig en path/query — URL:en byggs med `join`.
fn parse_sftp_admin_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw.trim())
        .map_err(|err| AgentError::Config(format!("CRAFTPILOT_SFTP_ADMIN_URL: {err}")))?;

    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AgentError::Config(
            "CRAFTPILOT_SFTP_ADMIN_URL must be an http(s) origin".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AgentError::Config(
            "CRAFTPILOT_SFTP_ADMIN_URL must not contain a query or fragment".into(),
        ));
    }
    if !url.path().is_empty() && url.path() != "/" {
        return Err(AgentError::Config(
            "CRAFTPILOT_SFTP_ADMIN_URL must not contain a path".into(),
        ));
    }

    let mut normalized = url.clone();
    normalized.set_path("/");
    Ok(normalized)
}

/// `CRAFTPILOT_TUNNEL` är en installationsflagga, inte en bool: setup.sh
/// skriver `enabled`/`disabled`. Booleska värden accepteras också så en
/// äldre eller egen `.env` fungerar.
fn parse_enabled_flag(key: &str, default: bool) -> Result<bool> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => enabled_flag_value(value.trim())
            .ok_or_else(|| AgentError::Config(format!("{key} must be enabled or disabled"))),
        _ => Ok(default),
    }
}

fn enabled_flag_value(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" => Some(true),
        "0" | "false" | "no" | "off" | "disabled" => Some(false),
        _ => None,
    }
}

fn is_tunnel_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':'))
}

/// Adressen frpc använder för att nå nodens publicerade portar. Compose mappar
/// `host.docker.internal` till värdens gateway, men en äldre compose-fil saknar
/// raden. Därför läses containerns standardgateway som reserv: den ligger på
/// samma värd och når samma publicerade portar. Utanför en container är
/// loopback rätt adress.
fn default_tunnel_local_host() -> String {
    if !std::path::Path::new("/.dockerenv").exists() {
        return "127.0.0.1".to_string();
    }

    if host_resolves("host.docker.internal") {
        return "host.docker.internal".to_string();
    }

    default_gateway().unwrap_or_else(|| "host.docker.internal".to_string())
}

fn host_resolves(host: &str) -> bool {
    use std::net::ToSocketAddrs;

    (host, 80)
        .to_socket_addrs()
        .map(|mut addresses| addresses.next().is_some())
        .unwrap_or(false)
}

fn default_gateway() -> Option<String> {
    let routes = std::fs::read_to_string("/proc/net/route").ok()?;
    gateway_from_routes(&routes)
}

/// `/proc/net/route` skriver gateway som little-endian hex på de arkitekturer
/// agenten kör på. `Destination` 00000000 är standardrutten och flaggan 0x2 är
/// RTF_GATEWAY.
fn gateway_from_routes(routes: &str) -> Option<String> {
    for line in routes.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 || fields[1] != "00000000" {
            continue;
        }
        let flags = u16::from_str_radix(fields[3], 16).ok()?;
        if flags & 0x2 == 0 {
            continue;
        }
        let raw = u32::from_str_radix(fields[2], 16).ok()?;
        return Some(std::net::Ipv4Addr::from(raw.to_le_bytes()).to_string());
    }
    None
}

fn optional_u32(key: &str) -> Result<Option<u32>> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u32>()
            .map(Some)
            .map_err(|_| AgentError::Config(format!("{key} must be an integer"))),
        _ => Ok(None),
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
    fn refuses_a_missing_or_blank_master_url() {
        // Ett bare `docker run` ska stanna i konfigurationen i stället för att
        // enrolla mot en inbyggd produktions-adress.
        for raw in [None, Some(""), Some("   ")] {
            let err = master_url_from(raw).unwrap_err();
            assert!(
                err.to_string()
                    .contains("CRAFTPILOT_MASTER_URL must be set"),
                "{raw:?} gav {err}"
            );
        }
    }

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
    fn tunnel_flag_accepts_the_setup_spelling() {
        assert_eq!(enabled_flag_value("enabled"), Some(true));
        assert_eq!(enabled_flag_value("disabled"), Some(false));
        assert_eq!(enabled_flag_value("TRUE"), Some(true));
        assert_eq!(enabled_flag_value("0"), Some(false));
        assert_eq!(enabled_flag_value("maybe"), None);
    }

    #[test]
    fn a_default_route_gives_the_gateway() {
        let routes = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\n\
eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\n";
        assert_eq!(gateway_from_routes(routes).as_deref(), Some("192.168.1.1"));
        assert_eq!(gateway_from_routes("Iface\tDestination\n"), None);
    }

    #[test]
    fn tunnel_local_host_is_a_host() {
        assert!(is_tunnel_host("host.docker.internal"));
        assert!(is_tunnel_host("127.0.0.1"));
        assert!(!is_tunnel_host(""));
        assert!(!is_tunnel_host("host docker"));
        assert!(!is_tunnel_host("host\nname"));
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
