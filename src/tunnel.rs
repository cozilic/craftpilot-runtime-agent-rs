use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use crate::api::MasterClient;
use crate::config::TunnelSettings;
use crate::error::{AgentError, Result};
use crate::state::StateStore;

/// Agentförmågan för tunnelklienten. `tunnel` säger att klienten finns och är
/// påslagen — mastern äger fortfarande `up`, så panelen visar "ansluter"
/// medan hubben bekräftar proxyn. `tunnel-disabled` skiljer en avstängd
/// klient från en agent som inte har någon alls.
pub const FEATURE_TUNNEL: &str = "tunnel";
pub const FEATURE_TUNNEL_DISABLED: &str = "tunnel-disabled";

pub fn capability_features(enabled: bool) -> Vec<String> {
    vec![if enabled {
        FEATURE_TUNNEL.to_string()
    } else {
        FEATURE_TUNNEL_DISABLED.to_string()
    }]
}

/// Sant när mastern vill ha en tunnel men agenten inte har någon token att
/// använda — det enda läget som rapporteras som `tunnel_token_missing`.
///
/// En avstängd tunnel, en token som mastern just skickat i svaret och en
/// sparad token i state är alla friska lägen: ingen av dem ska få mastern att
/// rotera. Utan den här gränsen skulle en ny token utfärdas varje poll i ett
/// läge där felet ligger någon annanstans (t.ex. CA:t).
pub fn missing_token(state: &TunnelState, stored_token: Option<&str>) -> bool {
    state.enabled && state.token.is_none() && stored_token.is_none()
}

/// Masterns svar från `GET /api/runtime/nodes/{node}/tunnel`
/// (docs/RUNTIME-NODE-TUNNEL.md). `token` är bara satt när mastern just
/// utfärdat en ny token; annars ligger den kvar i agentens state.
#[derive(Debug, Clone, Deserialize)]
pub struct TunnelState {
    pub enabled: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub hub: Option<TunnelHub>,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub ca_pem: Option<String>,
    #[serde(default)]
    pub ca_sha256: Option<String>,
    /// Nodens bandbreddstak i KB/s (2026-11-28). Mastern äger värdet; noll
    /// eller tomt betyder inget tak. Taket skrivs som `transport.bandwidthLimit`
    /// på varje proxy, så en nod aldrig kan ta mer än sin andel av masterns
    /// upplänk.
    #[serde(default)]
    pub bandwidth_limit_kbps: Option<u32>,
    #[serde(default)]
    pub proxies: Vec<TunnelProxy>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TunnelHub {
    pub host: String,
    pub port: u16,
    pub tls_name: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TunnelProxy {
    pub name: String,
    #[serde(default)]
    pub kind: String,
    pub local_port: u16,
    pub remote_port: u16,
}

impl TunnelSettings {
    pub fn config_path(&self) -> PathBuf {
        self.dir.join("frpc.toml")
    }

    pub fn ca_path(&self) -> PathBuf {
        self.dir.join("ca.crt")
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.join("frpc.log")
    }
}

/// En färdig frpc-konfiguration. `signature` ändras när hubben, token, CA:t
/// eller portlistan ändras — då måste frpc startas om.
#[derive(Debug)]
struct PreparedTunnel {
    token: String,
    new_token: bool,
    ca_pem: String,
    config: String,
    signature: String,
}

/// Agentens tunnelklient.
///
/// frpc körs som en barnprocess i agentcontainern. Det gör att den dör med
/// agenten, och att en avstängning stänger hubbförbindelsen direkt i stället
/// för att lämna kvar en process som håller nodens portar. Portarna som
/// frpc registrerar är nodens egna: mastern nekar allt annat i sitt plugin.
///
/// `tunnel`-förmågan säger att klienten finns och är påslagen, inte att
/// hubben redan accepterat den: mastern äger `up`, och panelen behöver kunna
/// skilja "ansluter" från "har ingen klient". En död eller nekad frpc syns i
/// `tunnel_state`/`tunnel_error` i stället för att agenten gissar.
pub struct TunnelService {
    settings: TunnelSettings,
    child: Option<Child>,
    /// Signatur för konfigurationen som barnprocessen startades med.
    applied: Option<String>,
    last_refresh: Option<Instant>,
    /// Mastern vill ha en tunnel men agenten har ingen token att använda
    /// (state-katalogen är borta, eller token sparades aldrig). Rapporteras i
    /// heartbeat/claim så mastern kan rotera i stället för att noden väntar
    /// ut TTL:en.
    token_missing: bool,
}

impl TunnelService {
    pub fn new(settings: TunnelSettings) -> Self {
        Self {
            settings,
            child: None,
            applied: None,
            last_refresh: None,
            token_missing: false,
        }
    }

    /// Mastern vill ha en tunnel men agenten har ingen token. Läggs i
    /// förmågorna av `Agent::capabilities()`, så hjärtat/claimen kan be
    /// mastern utfärda en ny.
    pub fn token_missing(&self) -> bool {
        self.token_missing
    }

    /// Hämtar nodens tunneläga och ser till att frpc speglar det.
    ///
    /// Nätverksanropet görs först: faller det stannar ingen förbindelse kvar
    /// som ser fungerande ut — `tunnel_state` ägs av mastern. När mastern säger
    /// `enabled: false` stoppas frpc och token/konfiguration tas bort, så
    /// nodens portar lämnas tillbaka till hubben.
    pub async fn refresh(
        &mut self,
        node_id: &str,
        api: &MasterClient,
        store: &mut StateStore,
    ) -> Result<()> {
        if !self.settings.enabled {
            self.shutdown().await;
            remove_files(&self.settings);
            clear_token(store)?;
            self.token_missing = false;
            return Ok(());
        }

        if self
            .last_refresh
            .is_some_and(|last| last.elapsed() < self.settings.refresh_interval)
        {
            return Ok(());
        }
        self.last_refresh = Some(Instant::now());

        if self.child.is_some() && !self.child_alive() {
            self.applied = None;
        }
        if self.child.is_none() {
            self.applied = None;
        }

        let state = match api.tunnel_state(node_id).await {
            Ok(state) => state,
            Err(err) => {
                // En master utan hubben (503), en gammal master (404) eller
                // en nätverksmiss ska inte lämna kvar en förbindelse som
                // håller portar hubben redan har släppt.
                self.shutdown().await;
                self.applied = None;
                debug!(error = %err, "tunnel state is unavailable");
                return Err(err);
            }
        };

        if !state.enabled {
            let had_secrets = self.applied.is_some() || store.state().tunnel_token.is_some();
            self.shutdown().await;
            self.applied = None;
            remove_files(&self.settings);
            clear_token(store)?;
            if had_secrets {
                info!("tunnel is off: stopped frpc and removed its config");
            }
            // Mastern vill ingen tunnel: ingen token behövs, alltså inget att
            // rapportera.
            self.token_missing = false;
            return Ok(());
        }

        let prepared = match prepare(
            &state,
            &self.settings,
            store.state().tunnel_token.as_deref(),
        ) {
            Ok(prepared) => {
                // Klienten kan bygga sin konfiguration: ingen token saknas.
                self.token_missing = false;
                prepared
            }
            Err(err) => {
                // Bara när mastern vill ha en tunnel och agenten varken fick
                // en ny token i svaret eller har en sparad: det är läget
                // mastern kan lösa genom att rotera (2026-11-27).
                self.token_missing = missing_token(&state, store.state().tunnel_token.as_deref());
                self.shutdown().await;
                self.applied = None;
                return Err(err);
            }
        };

        if prepared.new_token {
            store.state_mut().tunnel_token = Some(prepared.token.clone());
            store.save()?;
        }

        if self.applied.as_deref() == Some(prepared.signature.as_str())
            && self.child.is_some()
            && self.child_alive()
        {
            // frpc har redan rätt konfiguration; hubben äger `up`.
            return Ok(());
        }

        self.shutdown().await;
        self.applied = None;

        ensure_private_dir(&self.settings.dir)?;
        write_private(&self.settings.ca_path(), prepared.ca_pem.as_bytes())?;
        write_private(&self.settings.config_path(), prepared.config.as_bytes())?;
        self.spawn()?;
        self.applied = Some(prepared.signature);

        debug!(
            hub_state = %state.state,
            proxies = state.proxies.len(),
            "frpc started; waiting for the hub to confirm the proxies"
        );
        Ok(())
    }

    /// Stoppar frpc utan att röra konfigurationen. Anropas vid avstängning
    /// av agenten; själva portfrigöringen ägs av mastern.
    pub async fn shutdown(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => debug!(error = %err, "waiting for frpc failed"),
            Err(_) => warn!("frpc did not exit within 5 s"),
        }
        self.applied = None;
    }

    fn child_alive(&mut self) -> bool {
        let status = match self.child.as_mut() {
            Some(child) => child.try_wait(),
            None => return false,
        };

        match status {
            Ok(None) => true,
            Ok(Some(status)) => {
                warn!(code = status.code(), "frpc exited");
                self.child = None;
                self.applied = None;
                false
            }
            Err(err) => {
                warn!(error = %err, "checking frpc failed");
                self.child = None;
                self.applied = None;
                false
            }
        }
    }

    fn spawn(&mut self) -> Result<()> {
        let log = open_private(&self.settings.log_path())?;
        let stderr = log.try_clone()?;

        let child = Command::new(&self.settings.frpc_bin)
            .arg("-c")
            .arg(self.settings.config_path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| {
                AgentError::Runtime(format!(
                    "could not start {}: {err} (is frpc installed?)",
                    self.settings.frpc_bin
                ))
            })?;

        if let Some(pid) = child.id() {
            info!(pid, bin = %self.settings.frpc_bin, "frpc started");
        }
        self.child = Some(child);
        Ok(())
    }
}

/// Tar bort frp-konfigurationen (som innehåller tunnel-token) när tunneln
/// stängs av i miljön. CA:t och loggen tas också bort; katalogen lämnas.
pub fn remove_leftovers(settings: &TunnelSettings, store: &mut StateStore) -> Result<()> {
    remove_files(settings);
    clear_token(store)
}

fn clear_token(store: &mut StateStore) -> Result<()> {
    if store.state().tunnel_token.is_some() {
        store.state_mut().tunnel_token = None;
        store.save()?;
    }
    Ok(())
}

fn remove_files(settings: &TunnelSettings) {
    for path in [
        settings.config_path(),
        settings.ca_path(),
        settings.log_path(),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(path = %path.display(), error = %err, "could not remove a tunnel file")
            }
        }
    }
}

fn prepare(
    state: &TunnelState,
    settings: &TunnelSettings,
    stored_token: Option<&str>,
) -> Result<PreparedTunnel> {
    if state.proxies.is_empty() {
        return Err(AgentError::Runtime(
            "the master sent no tunnel proxies".into(),
        ));
    }
    if state.proxies.len() > 256 {
        return Err(AgentError::Runtime(
            "the master sent too many tunnel proxies".into(),
        ));
    }

    let hub = state
        .hub
        .as_ref()
        .ok_or_else(|| AgentError::Runtime("the master sent no tunnel hub".into()))?;
    let ca_pem = state
        .ca_pem
        .as_deref()
        .ok_or_else(|| AgentError::Runtime("the master sent no tunnel CA".into()))?;
    let ca_sha256 = state
        .ca_sha256
        .as_deref()
        .ok_or_else(|| AgentError::Runtime("the master sent no tunnel CA digest".into()))?;

    verify_ca(ca_pem, ca_sha256)?;

    let (token, new_token) = match state.token.as_deref() {
        Some(token) => (token.to_string(), Some(token) != stored_token),
        // En giltig token lämnas ut en gång. Utan den sparade kopian får
        // agenten vänta på att mastern roterar den, i stället för att
        // starta frpc med en tom token.
        None => (
            stored_token
                .ok_or_else(|| {
                    AgentError::Runtime(
                        "the tunnel token is missing from this agent; waiting for the master to reissue it"
                            .into(),
                    )
                })?
                .to_string(),
            false,
        ),
    };

    validate_token(&token)?;
    validate_user(&state.user)?;
    validate_bandwidth_limit(state.bandwidth_limit_kbps)?;

    let config = render_config(
        hub,
        &state.user,
        &token,
        &state.proxies,
        state.bandwidth_limit_kbps,
        &settings.local_host,
        &settings.ca_path(),
    )?;

    let mut hasher = Sha256::new();
    hasher.update(config.as_bytes());
    hasher.update(b"\0");
    hasher.update(ca_sha256.trim().to_ascii_lowercase().as_bytes());
    let signature = to_hex(&hasher.finalize());

    Ok(PreparedTunnel {
        token,
        new_token,
        ca_pem: ca_pem.to_string(),
        config,
        signature,
    })
}

fn verify_ca(ca_pem: &str, expected_sha256: &str) -> Result<()> {
    let expected = expected_sha256.trim();
    if expected.len() != 64 || !expected.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(AgentError::Runtime(
            "the master sent an invalid tunnel CA digest".into(),
        ));
    }
    if ca_pem.len() > 64 * 1024
        || !ca_pem.contains("BEGIN CERTIFICATE")
        || !ca_pem.contains("END CERTIFICATE")
    {
        return Err(AgentError::Runtime(
            "the master sent an invalid tunnel CA".into(),
        ));
    }

    let actual = to_hex(&Sha256::digest(ca_pem.as_bytes()));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(AgentError::Runtime(
            "the tunnel CA does not match the pinned digest".into(),
        ));
    }
    Ok(())
}

fn render_config(
    hub: &TunnelHub,
    user: &str,
    token: &str,
    proxies: &[TunnelProxy],
    bandwidth_limit_kbps: Option<u32>,
    local_host: &str,
    ca_path: &Path,
) -> Result<String> {
    validate_host_value("tunnel hub host", &hub.host)?;
    validate_tls_name(&hub.tls_name)?;
    validate_local_host(local_host)?;
    if hub.port == 0 {
        return Err(AgentError::Runtime(
            "the tunnel hub port must be 1-65535".into(),
        ));
    }

    let mut config = String::new();
    config.push_str("# Generated by craftpilot-runtime-agent. Do not edit.\n");
    config.push_str(&format!("serverAddr = {}\n", toml_string(&hub.host)));
    config.push_str(&format!("serverPort = {}\n", hub.port));
    config.push_str(&format!("user = {}\n", toml_string(user)));
    config.push_str(&format!(
        "metadatas.tunnel_token = {}\n\n",
        toml_string(token)
    ));
    config.push_str("transport.tls.enable = true\n");
    config.push_str(&format!(
        "transport.tls.trustedCaFile = {}\n",
        toml_string(&ca_path.display().to_string())
    ));
    config.push_str(&format!(
        "transport.tls.serverName = {}\n\n",
        toml_string(&hub.tls_name)
    ));

    let mut names = std::collections::BTreeSet::new();
    let mut remote_ports = std::collections::BTreeSet::new();

    for proxy in proxies {
        validate_proxy_name(&proxy.name)?;
        if !matches!(proxy.kind.as_str(), "server" | "sftp") {
            return Err(AgentError::Runtime(format!(
                "the master sent an unknown tunnel proxy kind: {}",
                proxy.kind
            )));
        }
        if proxy.local_port == 0 || proxy.remote_port == 0 {
            return Err(AgentError::Runtime(format!(
                "tunnel proxy {} has an invalid port",
                proxy.name
            )));
        }
        if !names.insert(proxy.name.as_str()) {
            return Err(AgentError::Runtime(format!(
                "the master sent a duplicate tunnel proxy name: {}",
                proxy.name
            )));
        }
        if !remote_ports.insert(proxy.remote_port) {
            return Err(AgentError::Runtime(format!(
                "the master sent a duplicate remote port: {}",
                proxy.remote_port
            )));
        }

        config.push_str("[[proxies]]\n");
        config.push_str(&format!("name = {}\n", toml_string(&proxy.name)));
        config.push_str("type = \"tcp\"\n");
        config.push_str(&format!("localIP = {}\n", toml_string(local_host)));
        config.push_str(&format!("localPort = {}\n", proxy.local_port));
        config.push_str(&format!("remotePort = {}\n", proxy.remote_port));

        // Bandbreddstaket (2026-11-28) ligger på varje proxy i frp — det finns
        // ingen klientövergripande gräns — så samma tak skrivs på nodens alla
        // proxies. frp:s enhet är KB/s, vilket är det mastern räknar i.
        if let Some(kbps) = bandwidth_limit_kbps {
            config.push_str(&format!(
                "transport.bandwidthLimit = {}\n",
                toml_string(&format!("{kbps}KB"))
            ));
        }

        config.push('\n');
    }

    Ok(config)
}

fn validate_token(token: &str) -> Result<()> {
    if token.len() < 32 || token.len() > 128 || !token.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(AgentError::Runtime(
            "the master sent an invalid tunnel token".into(),
        ));
    }
    Ok(())
}

/// Taket mastern skickar (KB/s). Noll betyder "inget tak" och utelämnas
/// redan av mastern; ett noll eller orimligt värde här är ett fel i
/// kontraktet, inte något att gissa runt.
fn validate_bandwidth_limit(kbps: Option<u32>) -> Result<()> {
    if let Some(kbps) = kbps {
        if kbps == 0 || kbps > 1_048_576 {
            return Err(AgentError::Runtime(
                "the master sent an invalid tunnel bandwidth limit".into(),
            ));
        }
    }
    Ok(())
}

fn validate_user(user: &str) -> Result<()> {
    if user.is_empty()
        || user.len() > 64
        || !user
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(AgentError::Runtime(
            "the master sent an invalid tunnel user".into(),
        ));
    }
    Ok(())
}

fn validate_proxy_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(AgentError::Runtime(
            "the master sent an invalid tunnel proxy name".into(),
        ));
    }
    Ok(())
}

fn validate_host_value(what: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 253
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':'))
    {
        return Err(AgentError::Runtime(format!("{what} is not a host")));
    }
    Ok(())
}

fn validate_tls_name(value: &str) -> Result<()> {
    validate_host_value("the tunnel TLS name", value)?;
    if value.contains(':') {
        return Err(AgentError::Runtime(
            "the tunnel TLS name must not contain a port".into(),
        ));
    }
    Ok(())
}

fn validate_local_host(value: &str) -> Result<()> {
    validate_host_value("the tunnel local host", value)
}

fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = open_private(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(dir: PathBuf) -> TunnelSettings {
        TunnelSettings {
            enabled: true,
            frpc_bin: "frpc".to_string(),
            local_host: "host.docker.internal".to_string(),
            dir,
            refresh_interval: Duration::from_secs(10),
        }
    }

    fn state() -> TunnelState {
        let parsed: TunnelState = serde_json::from_value(json!({
            "enabled": true,
            "state": "pending",
            "hub": {"host": "tunnel.example", "port": 7000, "tls_name": "tunnel.example"},
            "user": "node-1234",
            "token": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ca_pem": "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
            "ca_sha256": "aa",
            "proxies": [
                {"name": "server-abc", "kind": "server", "local_port": 25568, "remote_port": 30001},
                {"name": "sftp", "kind": "sftp", "local_port": 2222, "remote_port": 30002}
            ]
        }))
        .unwrap();
        parsed.with_valid_ca()
    }

    impl TunnelState {
        fn with_valid_ca(mut self) -> Self {
            let pem = self.ca_pem.clone().unwrap();
            self.ca_sha256 = Some(to_hex(&Sha256::digest(pem.as_bytes())));
            self
        }
    }

    #[test]
    fn parses_the_master_contract() {
        let parsed = state();
        assert!(parsed.enabled);
        assert_eq!(parsed.hub.as_ref().unwrap().port, 7000);
        assert_eq!(parsed.proxies[1].kind, "sftp");
        assert_eq!(parsed.proxies[1].remote_port, 30002);
    }

    #[test]
    fn renders_tls_token_and_proxies() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-render");
        let prepared = prepare(&state(), &settings(dir.clone()), None).unwrap();
        let config = prepared.config;

        assert!(config.contains("metadatas.tunnel_token = \"aaaaaaaa"));
        assert!(config.contains("transport.tls.enable = true"));
        assert!(config.contains("transport.tls.serverName = \"tunnel.example\""));
        assert!(config.contains("localIP = \"host.docker.internal\""));
        assert!(config.contains("remotePort = 30001"));
        assert!(config.contains("[[proxies]]\nname = \"sftp\""));
    }

    #[test]
    fn renders_the_bandwidth_limit_on_every_proxy() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-bandwidth");
        let mut limited = state();
        limited.bandwidth_limit_kbps = Some(1250);

        let config = prepare(&limited, &settings(dir), None).unwrap().config;

        assert_eq!(
            config.matches("transport.bandwidthLimit = \"1250KB\"").count(),
            2,
            "taket ska gälla nodens båda proxies: {config}"
        );
    }

    #[test]
    fn a_tunnel_without_a_limit_has_no_bandwidth_key() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-no-bandwidth");
        let config = prepare(&state(), &settings(dir), None).unwrap().config;

        assert!(!config.contains("bandwidthLimit"));
    }

    #[test]
    fn rejects_an_invalid_bandwidth_limit() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-bad-bandwidth");

        for kbps in [0, 1_048_577] {
            let mut broken = state();
            broken.bandwidth_limit_kbps = Some(kbps);
            assert!(prepare(&broken, &settings(dir.clone()), None).is_err());
        }
    }

    #[test]
    fn a_changed_bandwidth_limit_changes_the_signature() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-bandwidth-signature");
        let first = prepare(&state(), &settings(dir.clone()), None).unwrap();

        let mut limited = state();
        limited.bandwidth_limit_kbps = Some(2000);
        let second = prepare(&limited, &settings(dir), None).unwrap();

        assert_ne!(first.signature, second.signature);
    }

    #[test]
    fn a_changed_port_changes_the_signature() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-signature");
        let first = prepare(&state(), &settings(dir.clone()), None).unwrap();

        let mut changed = state();
        changed.proxies[0].remote_port = 30003;
        let second = prepare(&changed, &settings(dir), None).unwrap();

        assert_ne!(first.signature, second.signature);
    }

    #[test]
    fn rejects_a_ca_that_does_not_match_the_digest() {
        let mut broken = state();
        broken.ca_sha256 = Some("0".repeat(64));

        let err = prepare(
            &broken,
            &settings(std::env::temp_dir().join("craftpilot-agent-test-tunnel-ca")),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("pinned digest"), "{err}");
    }

    #[test]
    fn rejects_unsafe_or_duplicate_values() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-invalid");

        let mut unsafe_host = state();
        unsafe_host.hub.as_mut().unwrap().host = "evil\"\nhost".into();
        assert!(prepare(&unsafe_host, &settings(dir.clone()), None).is_err());

        let mut bad_token = state();
        bad_token.token = Some("not-hex".into());
        assert!(prepare(&bad_token, &settings(dir.clone()), None).is_err());

        let mut duplicate = state();
        duplicate.proxies[0].name = "sftp".into();
        assert!(prepare(&duplicate, &settings(dir.clone()), None).is_err());

        let mut unknown_kind = state();
        unknown_kind.proxies[0].kind = "udp".into();
        assert!(prepare(&unknown_kind, &settings(dir), None).is_err());
    }

    #[test]
    fn a_stored_token_survives_a_null_response() {
        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-token");
        let stored = "b".repeat(64);

        let mut state = state();
        state.token = None;

        let prepared = prepare(&state, &settings(dir.clone()), Some(&stored)).unwrap();
        assert_eq!(prepared.token, stored);
        assert!(!prepared.new_token);

        assert!(prepare(&state, &settings(dir), None).is_err());
    }

    #[test]
    fn the_client_is_reported_before_the_hub_is_up() {
        // `up` ägs av mastern. Förmågan säger bara att klienten finns och är
        // påslagen, så panelen kan skilja den från en agent utan klient.
        assert_eq!(capability_features(true), vec![FEATURE_TUNNEL]);
        assert_eq!(capability_features(false), vec![FEATURE_TUNNEL_DISABLED]);
    }

    #[test]
    fn a_client_without_a_token_asks_the_master_for_one() {
        let stored = "b".repeat(64);

        // Ingen token i svaret och ingen sparad: bara mastern kan lösa det.
        let mut without_token = state();
        without_token.token = None;
        assert!(missing_token(&without_token, None));

        // En sparad token duger — den återanvänds när mastern svarar `null`.
        assert!(!missing_token(&without_token, Some(&stored)));

        // En ny token i svaret är alltid nog, och en avstängd tunnel behöver
        // ingen token alls.
        assert!(!missing_token(&state(), None));
        let mut disabled = state();
        disabled.enabled = false;
        disabled.token = None;
        assert!(!missing_token(&disabled, None));
    }

    #[test]
    fn a_stopped_client_does_not_report_a_missing_token() {
        let service = TunnelService::new(settings(
            std::env::temp_dir().join("craftpilot-agent-test-tunnel-missing"),
        ));
        assert!(!service.token_missing());
    }

    #[cfg(unix)]
    #[test]
    fn config_and_ca_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join("craftpilot-agent-test-tunnel-perms");
        let _ = fs::remove_dir_all(&dir);
        let settings = settings(dir.clone());
        let prepared = prepare(&state(), &settings, None).unwrap();

        ensure_private_dir(&settings.dir).unwrap();
        write_private(&settings.ca_path(), prepared.ca_pem.as_bytes()).unwrap();
        write_private(&settings.config_path(), prepared.config.as_bytes()).unwrap();

        for path in [&settings.dir, &settings.ca_path(), &settings.config_path()] {
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            let expected = if path == &settings.dir { 0o700 } else { 0o600 };
            assert_eq!(mode, expected, "{}", path.display());
        }

        fs::remove_dir_all(&dir).unwrap();
    }
}
