//! Nodens SFTPGo-instans: livscykel för containern och den återkommande
//! synken mot masterns state (docs/RUNTIME-NODE-SFTP.md).
//!
//! Containern ägs av agenten, inte av mastern: mastern vet bara att noden
//! rapporterar `sftp` och var värdnyckeln finns. Därför är alla volymer
//! namngivna Docker-volymer (agenten kan själv köra i en container och då
//! fungerar inte containerns egna sökvägar som bind mounts), medan
//! serverdatan mountas från den värdväg agenten redan känner
//! (`CRAFTPILOT_DOCKER_SERVER_DATA_ROOT` på Docker Desktop).
//!
//! Admin-API:t publiceras aldrig på värden: agenten når det över samma
//! Docker-nät som spelcontainrarna. Porten som publiceras är SFTP-porten.

use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use tokio::process::Command as ProcessCommand;
use tokio::time::timeout;
use tracing::info;

use crate::api::MasterClient;
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::model::NodeCapabilities;
use crate::sftpgo::{host_key_fingerprint, ApplyConfig, SftpGoClient, CONTAINER_SERVERS_ROOT};

/// Hur ofta containerns tillstånd kontrolleras när den redan är frisk.
/// Varje poll behöver inte kosta en `docker inspect`.
const CONTAINER_CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// En idempotent om-applicering då och då läker en SFTPGo som tappat sin
/// databas (återställd volym) utan att revisionen ändrats.
const APPLY_REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Första `docker run` kan behöva pulla imagen.
const DOCKER_PULL_TIMEOUT: Duration = Duration::from_secs(300);
/// Ett misslyckat bakgrundspull (nätet nere, fel tag) får inte försöka i
/// evighet; agenten ska fortsätta polla och försöka igen senare.
const IMAGE_PULL_RETRY: Duration = Duration::from_secs(5 * 60);
const SFTPGO_SFTP_PORT: u16 = 2022;
/// Hur många portar agenten provar ovanför den föredragna innan den ger upp.
const PORT_CANDIDATES: u16 = 32;
const SFTPGO_ADMIN_PORT: u16 = 8080;
/// SFTPGo får bara servera den här värdnyckeln. Panelen visar agentens
/// rapporterade fingeravtryck, och en klient som annars förhandlat fram
/// RSA/ECDSA hade då fått ett annat värde än panelen.
const SFTPGO_HOST_KEY_PATH: &str = "/var/lib/sftpgo/id_ed25519";
/// Höjs när containerns miljö/mounts ändras på ett sätt som kräver att en
/// befintlig container återskapas. En uppgraderad agent ska inte behöva
/// vänta på att någon tar bort containern manuellt.
const CONTAINER_CONFIG_VERSION: &str = "2";

pub struct SftpService {
    image: String,
    /// Porten operatören föredrar. Är den upptagen på värden publicerar
    /// agenten SFTPGo på nästa lediga i stället.
    preferred_port: u16,
    /// Porten SFTPGo faktiskt publicerades på — den agenten rapporterar.
    /// `None` tills containern skapats eller inspekterats.
    bound_port: Option<u16>,
    container_name: String,
    network: String,
    server_data_root: PathBuf,
    /// Värdvägen Docker daemon ser för serverdatan.
    host_data_root: PathBuf,
    /// `None` = härled ur serverkatalogens ägare vid varje apply.
    file_uid: Option<u32>,
    file_gid: Option<u32>,
    config_volume: String,
    data_volume: String,
    admin_username: String,
    admin_password: String,
    client: SftpGoClient,
    container_checked_at: Option<Instant>,
    container_healthy: bool,
    image_pull_started_at: Option<Instant>,
    applied_revision: Option<String>,
    applied_at: Option<Instant>,
    fingerprint: Option<String>,
    capabilities: NodeCapabilities,
}

impl SftpService {
    pub fn new(cfg: &Config, admin_password: String) -> Result<Self> {
        let client = SftpGoClient::new(
            cfg.sftp.admin_url.clone(),
            cfg.sftp.admin_username.clone(),
            admin_password.clone(),
        )?;

        // Samma värdväg som spelcontainrarna använder. Utan den pekar
        // mounten på agentens egen container-sökväg, som daemonen inte kan
        // binda (Docker Desktop).
        let host_data_root = cfg
            .docker_data_root
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| cfg.server_data_root.clone());

        Ok(Self {
            image: cfg.sftp.image.clone(),
            preferred_port: cfg.sftp.port,
            bound_port: None,
            container_name: cfg.sftp.container_name.clone(),
            network: cfg.docker_network.clone(),
            server_data_root: cfg.server_data_root.clone(),
            host_data_root,
            file_uid: cfg.sftp.file_uid,
            file_gid: cfg.sftp.file_gid,
            config_volume: format!("{}-config", cfg.sftp.container_name),
            data_volume: format!("{}-data", cfg.sftp.container_name),
            admin_username: cfg.sftp.admin_username.clone(),
            admin_password,
            client,
            container_checked_at: None,
            container_healthy: false,
            image_pull_started_at: None,
            applied_revision: None,
            applied_at: None,
            fingerprint: None,
            capabilities: NodeCapabilities::default(),
        })
    }

    /// Det agenten får rapportera i heartbeat/claim. `features` är tom när
    /// SFTPGo inte svarar — då slutar panelen lova en slutpunkt i stället
    /// för att visa en död värd.
    pub fn capabilities(&self) -> NodeCapabilities {
        self.capabilities.clone()
    }

    /// SFTPGo svarar inte (eller state kunde inte appliceras): sluta
    /// rapportera `sftp`. Värdnyckeln lämnas orörd hos mastern — den är
    /// publik och blir rätt igen så snart SFTPGo svarar.
    pub fn unavailable(&mut self) {
        self.capabilities = NodeCapabilities::default();
        // Porten rapporteras bara medan SFTPGo svarar; nästa lyckade
        // containerkontroll läser av den igen.
        self.bound_port = None;
        self.container_healthy = false;
        self.container_checked_at = None;
        self.applied_revision = None;
        self.applied_at = None;
    }

    /// Ser till att containern kör, hämtar masterns state och applicerar det
    /// när revisionen ändrats (eller SFTPGo precis startats om).
    pub async fn refresh(&mut self, node_id: &str, api: &MasterClient) -> Result<()> {
        let restarted = self.ensure_container().await?;

        let status = self.client.status().await?;
        self.fingerprint = host_key_fingerprint(&status);

        let state = api.sftp_state(node_id).await?;
        let stale = self
            .applied_at
            .is_none_or(|at| at.elapsed() >= APPLY_REFRESH_INTERVAL);
        if restarted || stale || self.applied_revision.as_deref() != Some(state.revision.as_str()) {
            let outcome = self
                .client
                .apply(
                    &state,
                    &ApplyConfig {
                        data_root: &self.server_data_root,
                        file_uid: self.file_uid,
                        file_gid: self.file_gid,
                    },
                )
                .await?;
            // Bara antal loggas: användarnamn, nycklar och state är inte
            // något som hör hemma i en publik logg (docs/RUNTIME-NODE-SFTP).
            info!(
                revision = %state.revision,
                users = outcome.users,
                folders = outcome.folders,
                skipped_folders = outcome.skipped_folders,
                "sftp state applied"
            );
            self.applied_revision = Some(state.revision.clone());
            self.applied_at = Some(Instant::now());
        }

        self.capabilities = NodeCapabilities {
            features: vec!["sftp".to_string()],
            fingerprint: self.fingerprint.clone(),
            bound_port: self.bound_port,
        };
        Ok(())
    }

    /// `true` när containern skapades eller startades om — då måste state
    /// appliceras även om revisionen är densamma (en ny SFTPGo-databas är
    /// tom).
    async fn ensure_container(&mut self) -> Result<bool> {
        if self.container_healthy
            && self
                .container_checked_at
                .is_some_and(|at| at.elapsed() < CONTAINER_CHECK_INTERVAL)
        {
            return Ok(false);
        }

        // Imagen kan behöva pullas första gången. Det får inte blockera
        // poll-loopen (då tystnar noden i flera minuter), så pullen körs i
        // bakgrunden och nästa poll skapar containern när den finns lokalt.
        if !self.image_present().await {
            self.start_image_pull();
            return Err(AgentError::Runtime(
                "the sftpgo image is not available locally yet; pulling it in the background"
                    .into(),
            ));
        }

        let inspected = self
            .docker(&[
                "inspect",
                "--format",
                "{{.State.Running}}|{{.Config.Image}}|{{index .Config.Labels \"craftpilot.sftpgo\"}}|{{index .Config.Labels \"craftpilot.sftpgo.config\"}}|{{json .HostConfig.PortBindings}}",
                &self.container_name,
            ])
            .await;

        let output = match inspected {
            Ok(output) => output,
            Err(err) => {
                self.container_healthy = false;
                return Err(err);
            }
        };

        if !output.status.success() {
            if !is_missing_container(&output) {
                self.container_healthy = false;
                return Err(AgentError::Runtime(format!(
                    "docker inspect failed: {}",
                    normalized(&output.stderr)
                )));
            }

            self.create_container().await?;
            self.mark_healthy();
            return Ok(true);
        }

        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let parts: Vec<&str> = raw.splitn(5, '|').collect();
        if parts.len() != 5 {
            self.container_healthy = false;
            return Err(AgentError::Runtime(
                "docker returned an invalid sftpgo container state".into(),
            ));
        }
        let running = parts[0] == "true";
        let image = parts[1];
        let label = parts[2];
        let config_version = parts[3];
        let ports = parts[4];

        if label != "1" {
            self.container_healthy = false;
            return Err(AgentError::Runtime(format!(
                "refusing to manage container {}: it was not created by this agent",
                self.container_name
            )));
        }

        // Containern äger sanningen om porten: agenten väljer nästa lediga
        // när den föredragna är upptagen, och en container som redan kör på
        // en annan port ska behållas — inte bytas vid varje poll.
        if let Some(port) = published_port(ports) {
            self.bound_port = Some(port);
        }

        // En uppgradering av agenten kan byta image eller miljö. Då
        // återskapas containern; SFTPGo:s databas, hemm och värdnyckel ligger
        // kvar i volymerna.
        if !self.settings_match(image, config_version, ports) {
            info!(
                image = %image,
                "sftpgo container settings changed; recreating it"
            );
            self.remove_container().await?;
            self.create_container().await?;
            self.mark_healthy();
            return Ok(true);
        }

        if !running {
            info!("starting the sftpgo container");
            let output = self.docker(&["start", &self.container_name]).await?;
            if !output.status.success() {
                self.container_healthy = false;
                return Err(AgentError::Runtime(format!(
                    "docker start failed: {}",
                    normalized(&output.stderr)
                )));
            }
            self.mark_healthy();
            return Ok(true);
        }

        self.mark_healthy();
        Ok(false)
    }

    async fn image_present(&self) -> bool {
        match self.docker(&["image", "inspect", &self.image]).await {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    fn start_image_pull(&mut self) {
        if self
            .image_pull_started_at
            .is_some_and(|at| at.elapsed() < IMAGE_PULL_RETRY)
        {
            return;
        }
        self.image_pull_started_at = Some(Instant::now());
        let image = self.image.clone();
        info!(image = %image, "pulling the sftpgo image in the background");
        tokio::spawn(async move {
            let _ = ProcessCommand::new("docker")
                .args(["pull", &image])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        });
    }

    fn mark_healthy(&mut self) {
        self.container_healthy = true;
        self.container_checked_at = Some(Instant::now());
    }

    /// Image, konfigurationsversion och en läsbar publicerad SFTP-port måste
    /// stämma. Ett inställningsbyte (t.ex. Ed25519-only-värdnyckeln) kräver en
    /// ny container eftersom Docker inte ändrar miljön på en befintlig.
    ///
    /// Porten jämförs inte mot den föredragna: agenten publicerar på nästa
    /// lediga när den föredragna är upptagen, och den containern ska behållas.
    fn settings_match(&self, image: &str, config_version: &str, ports: &str) -> bool {
        image == self.image
            && config_version == CONTAINER_CONFIG_VERSION
            && published_port(ports).is_some()
    }

    async fn create_container(&mut self) -> Result<()> {
        for volume in [&self.config_volume, &self.data_volume] {
            let output = self.docker(&["volume", "create", volume]).await?;
            if !output.status.success() {
                return Err(AgentError::Runtime(format!(
                    "docker volume create failed: {}",
                    normalized(&output.stderr)
                )));
            }
        }

        // Den föredragna porten först; är den upptagen på värden tar agenten
        // nästa lediga. En nod kan dela maskin med en panel som redan har
        // 2222, och då får SFTPGo inte plats på defaultporten. Docker är den
        // enda pålitliga domaren: agenten kör ofta i en egen container, så en
        // egen bind-test säger inget om värdens portar.
        for port in candidate_ports(self.preferred_port) {
            let args = self.container_args(port);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let output = self.docker_timeout(&refs, DOCKER_PULL_TIMEOUT).await?;
            if output.status.success() {
                self.bound_port = Some(port);
                info!(
                    image = %self.image,
                    port,
                    preferred = self.preferred_port,
                    "sftpgo container created"
                );
                return Ok(());
            }

            let stderr = normalized(&output.stderr);
            if !port_unavailable(&stderr) {
                return Err(AgentError::Runtime(format!(
                    "docker run for sftpgo failed: {stderr}"
                )));
            }

            // `docker run` kan ha hunnit skapa containern innan bindningen
            // föll; namnet måste vara fritt innan nästa port provas.
            self.remove_container().await?;
            info!(
                port,
                preferred = self.preferred_port,
                "host port is taken, trying the next one"
            );
        }

        Err(AgentError::Runtime(format!(
            "no free host port for sftpgo near {}",
            self.preferred_port
        )))
    }

    async fn remove_container(&mut self) -> Result<()> {
        let output = self
            .docker(&["rm", "--force", &self.container_name])
            .await?;
        if !output.status.success() && !is_missing_container(&output) {
            return Err(AgentError::Runtime(format!(
                "docker rm for sftpgo failed: {}",
                normalized(&output.stderr)
            )));
        }
        Ok(())
    }

    /// Fast argv — aldrig ett shell. Lösenordet går som env till SFTPGo
    /// (samma väg som `docker run -e`), inte via kommandoraden i en shell.
    fn container_args(&self, port: u16) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            self.container_name.clone(),
            "--label".into(),
            "craftpilot.managed=1".into(),
            "--label".into(),
            "craftpilot.sftpgo=1".into(),
            "--label".into(),
            format!("craftpilot.sftpgo.config={CONTAINER_CONFIG_VERSION}"),
            "--restart".into(),
            "unless-stopped".into(),
            "--network".into(),
            self.network.clone(),
            // SFTPGo:s image kör uid 1000 som default. Nodens serverfiler
            // ägs av spelcontainrarnas användare, så SFTPGo måste kunna
            // skriva dem; skapade filer får uid/gid från användarobjektet
            // (härlett ur serverkatalogen om inget override är satt).
            "--user".into(),
            "0:0".into(),
            "--volume".into(),
            format!("{}:/var/lib/sftpgo:rw", self.config_volume),
            "--volume".into(),
            format!("{}:/srv/sftpgo/data:rw", self.data_volume),
            "--volume".into(),
            format!(
                "{}:{CONTAINER_SERVERS_ROOT}:rw",
                self.host_data_root.display()
            ),
            "--publish".into(),
            format!("0.0.0.0:{port}:{SFTPGO_SFTP_PORT}"),
            "--log-driver".into(),
            "json-file".into(),
            "--log-opt".into(),
            "max-size=10m".into(),
            "--log-opt".into(),
            "max-file=3".into(),
        ];

        for (key, value) in [
            (
                "SFTPGO_DATA_PROVIDER__CREATE_DEFAULT_ADMIN",
                "true".to_string(),
            ),
            ("SFTPGO_DEFAULT_ADMIN_USERNAME", self.admin_username.clone()),
            ("SFTPGO_DEFAULT_ADMIN_PASSWORD", self.admin_password.clone()),
            // Admin-API:t måste vara nåbart över Docker-nätet; webadmin och
            // webclient är avstängda, bara REST-API:t behövs.
            ("SFTPGO_HTTPD__BINDINGS__0__ADDRESS", "0.0.0.0".to_string()),
            (
                "SFTPGO_HTTPD__BINDINGS__0__PORT",
                SFTPGO_ADMIN_PORT.to_string(),
            ),
            (
                "SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN",
                "false".to_string(),
            ),
            (
                "SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_CLIENT",
                "false".to_string(),
            ),
            ("SFTPGO_SFTPD__BINDINGS__0__ADDRESS", "0.0.0.0".to_string()),
            (
                "SFTPGO_SFTPD__BINDINGS__0__PORT",
                SFTPGO_SFTP_PORT.to_string(),
            ),
            // Bara Ed25519: panelens fingeravtryck ska vara det enda en
            // klient kan få, oavsett vilken algoritm den föredrar.
            ("SFTPGO_SFTPD__HOST_KEYS", SFTPGO_HOST_KEY_PATH.to_string()),
            // Skapade filer ska vara gruppskrivbara: spelcontainern och
            // SFTP delar ofta grupp men inte användare.
            ("SFTPGO_COMMON__UMASK", "0002".to_string()),
        ] {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }

        args.push(self.image.clone());
        args
    }

    async fn docker(&self, args: &[&str]) -> Result<Output> {
        self.docker_timeout(args, Duration::from_secs(60)).await
    }

    async fn docker_timeout(&self, args: &[&str], limit: Duration) -> Result<Output> {
        let mut command = ProcessCommand::new("docker");
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        timeout(limit, command.output())
            .await
            .map_err(|_| AgentError::Runtime("docker operation timed out".into()))?
            .map_err(|err| AgentError::Runtime(format!("could not run the docker CLI: {err}")))
    }
}

fn is_missing_container(output: &Output) -> bool {
    normalized(&output.stderr).contains("No such container")
        || normalized(&output.stderr).contains("No such object")
}

fn normalized(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

/// Portarna agenten får publicera SFTPGo på: den föredragna först, sedan
/// nästa lediga uppåt. Aldrig utanför 1–65535.
fn candidate_ports(preferred: u16) -> Vec<u16> {
    // u16:s saturating_add kapar redan vid 65535.
    let last = preferred.saturating_add(PORT_CANDIDATES);
    (preferred..=last).collect()
}

/// Värdporten SFTPGo-containern publicerar, läs ur `HostConfig.PortBindings`.
fn published_port(raw: &str) -> Option<u16> {
    let bindings = serde_json::from_str::<serde_json::Value>(raw.trim()).ok()?;

    bindings
        .get(format!("{SFTPGO_SFTP_PORT}/tcp"))
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find_map(|entry| {
            entry
                .get("HostPort")
                .and_then(serde_json::Value::as_str)
                .and_then(|port| port.parse::<u16>().ok())
                .filter(|port| *port >= 1)
        })
}

/// Docker:s svar när värdporten redan är tagen. Formuleringen skiljer mellan
/// Linux och Docker Desktop, så båda tas med — allt annat är ett riktigt fel.
fn port_unavailable(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();

    stderr.contains("port is already allocated")
        || stderr.contains("address already in use")
        || stderr.contains("ports are not available")
}

/// Ett lösenord som bara agenten känner, sparat i den privata state-filen.
/// SFTPGo kräver minst 8 tecken; rcon-generatorn ger 32 hextecken.
pub fn generate_admin_password() -> String {
    crate::rcon::generate_password()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use url::Url;

    use super::*;
    use crate::config::{Config, SftpSettings, DEFAULT_SFTP_IMAGE};

    fn service() -> SftpService {
        let mut cfg = Config::for_test(
            Url::parse("https://master.example/").unwrap(),
            PathBuf::from("/var/lib/craftpilot-agent"),
        );
        cfg.sftp = SftpSettings {
            enabled: true,
            image: DEFAULT_SFTP_IMAGE.to_string(),
            port: 2222,
            admin_url: Url::parse("http://craftpilot-sftpgo:8080/").unwrap(),
            container_name: "craftpilot-sftpgo".to_string(),
            file_uid: None,
            file_gid: None,
            admin_username: "craftpilot-agent".to_string(),
        };
        SftpService::new(&cfg, "test-admin-password".to_string()).unwrap()
    }

    #[test]
    fn the_admin_password_is_never_empty() {
        let password = generate_admin_password();
        assert!(password.len() >= 8);
        assert!(password.chars().all(|ch| ch.is_ascii_alphanumeric()));
    }

    /// Porten som publiceras är SFTP-porten (2022 i containern), aldrig
    /// admin-API:t. Admin nås bara över Docker-nätet.
    #[test]
    fn container_args_publish_only_the_sftp_port() {
        let args = service().container_args(2222);
        let joined = args.join(" ");

        assert!(joined.contains("--publish 0.0.0.0:2222:2022"), "{joined}");
        assert!(!joined.contains("8080:"), "{joined}");
        assert!(
            joined.contains("craftpilot-sftpgo-config:/var/lib/sftpgo"),
            "{joined}"
        );
        assert!(
            joined.contains("craftpilot-sftpgo-data:/srv/sftpgo/data"),
            "{joined}"
        );
        assert!(joined.contains(":/srv/craftpilot/servers:rw"), "{joined}");
        assert!(
            joined.contains("SFTPGO_DATA_PROVIDER__CREATE_DEFAULT_ADMIN=true"),
            "{joined}"
        );
        assert!(
            joined.contains("SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN=false"),
            "{joined}"
        );
        assert!(
            joined.contains("SFTPGO_SFTPD__HOST_KEYS=/var/lib/sftpgo/id_ed25519"),
            "{joined}"
        );
        assert!(joined.contains("craftpilot.sftpgo.config=2"), "{joined}");
        assert!(joined.ends_with(DEFAULT_SFTP_IMAGE), "{joined}");
    }

    #[test]
    fn the_host_data_root_wins_over_the_container_path() {
        let mut cfg = Config::for_test(
            Url::parse("https://master.example/").unwrap(),
            PathBuf::from("/var/lib/craftpilot-agent"),
        );
        cfg.docker_data_root = Some("C:/craftpilot/servers".to_string());
        cfg.sftp.enabled = true;

        let service = SftpService::new(&cfg, "test-admin-password".to_string()).unwrap();
        let joined = service.container_args(2222).join(" ");
        assert!(
            joined.contains("C:/craftpilot/servers:/srv/craftpilot/servers:rw"),
            "{joined}"
        );
    }

    #[test]
    fn capabilities_are_only_advertised_when_the_service_is_ready() {
        let mut service = service();
        assert!(service.capabilities().features.is_empty());

        service.capabilities = NodeCapabilities {
            features: vec!["sftp".to_string()],
            fingerprint: Some("SHA256:test".to_string()),
            bound_port: Some(2223),
        };
        assert_eq!(service.capabilities().features, vec!["sftp"]);
        assert_eq!(service.capabilities().bound_port, Some(2223));

        service.unavailable();
        assert!(service.capabilities().features.is_empty());
        assert!(service.capabilities().fingerprint.is_none());
        assert!(service.capabilities().bound_port.is_none());
    }

    #[test]
    fn a_changed_container_configuration_forces_a_recreate() {
        let service = service();
        let ports = r#"{"2022/tcp":[{"HostIp":"0.0.0.0","HostPort":"2222"}]}"#;

        assert!(service.settings_match(DEFAULT_SFTP_IMAGE, "2", ports));
        // En container som redan publicerar en annan port behålls: agenten
        // valde den för att den föredragna var upptagen.
        assert!(service.settings_match(
            DEFAULT_SFTP_IMAGE,
            "2",
            r#"{"2022/tcp":[{"HostIp":"0.0.0.0","HostPort":"2223"}]}"#
        ));
        // En gammal container (utan config-label eller med en tidigare
        // version) ska bytas ut så Ed25519-only slår igenom.
        assert!(!service.settings_match(DEFAULT_SFTP_IMAGE, "<no value>", ports));
        assert!(!service.settings_match(DEFAULT_SFTP_IMAGE, "1", ports));
        assert!(!service.settings_match("drakkan/sftpgo:v2.6.5", "2", ports));
        assert!(!service.settings_match(DEFAULT_SFTP_IMAGE, "2", "{}"));
    }

    #[test]
    fn the_published_port_is_read_from_the_container_bindings() {
        assert_eq!(
            Some(2222),
            published_port(r#"{"2022/tcp":[{"HostIp":"0.0.0.0","HostPort":"2222"}]}"#)
        );
        assert_eq!(
            Some(2223),
            published_port(r#"{"2022/tcp":[{"HostIp":"0.0.0.0","HostPort":"2223"}]}"#)
        );
        assert_eq!(None, published_port("{}"));
        assert_eq!(None, published_port("not json"));
    }

    #[test]
    fn the_configured_port_is_tried_first_and_the_next_ones_after_it() {
        let ports = candidate_ports(2222);
        assert_eq!(ports.first(), Some(&2222));
        assert_eq!(ports.get(1), Some(&2223));
        assert_eq!(ports.len(), usize::from(PORT_CANDIDATES) + 1);
    }

    #[test]
    fn candidate_ports_never_leave_the_valid_range() {
        assert_eq!(candidate_ports(65535), vec![65535]);
        assert_eq!(candidate_ports(65534), vec![65534, 65535]);
    }

    #[test]
    fn only_a_taken_host_port_moves_on_to_the_next_candidate() {
        assert!(port_unavailable(
            "Error response from daemon: driver failed programming external connectivity: Bind for 0.0.0.0:2222 failed: port is already allocated"
        ));
        assert!(port_unavailable(
            "Error starting userland proxy: listen tcp4 0.0.0.0:2222: bind: address already in use"
        ));
        // Ett riktigt fel (saknad image, trasig volym) ska inte tyst byta port.
        assert!(!port_unavailable(
            "Unable to find image 'drakkan/sftpgo:v2.6.6' locally"
        ));
    }
}
