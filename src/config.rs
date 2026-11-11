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
    pub poll_seconds: u64,
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
            poll_seconds,
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

fn parse_u64(key: &str, default: u64) -> Result<u64> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .map_err(|_| AgentError::Config(format!("{key} must be an integer"))),
        _ => Ok(default),
    }
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
    fn hostname_matches_the_master_regex() {
        let hostname = detect_hostname();
        let first = hostname.chars().next().unwrap_or(' ');
        assert!(first.is_ascii_alphanumeric());
        assert!(hostname
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')));
    }
}
