use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command as ProcessCommand;
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::error::{AgentError, Result};
use crate::rcon::{self, RconSettings};

const CONTAINER_PREFIX: &str = "craftpilot-";
const MOUNT_PORT: u16 = 25565;
const MAX_CAPTURED_ERROR: usize = 220;
/// Samma konsol-FIFO som masterns `RuntimeSpec::CONSOLE_FILE`. Spelet äger
/// läsänden (`0<>` i startkommandot), agenten skriver en rad per kommando.
const CONSOLE_FILE: &str = "/tmp/.craftpilot-console";

/// Kör enbart fasta Docker-kommandon med argv — aldrig ett shell.
/// Motsvarar Python-agentens `DockerRuntime` för START/STOP.
pub struct Docker {
    data_root: PathBuf,
    docker_data_root: Option<PathBuf>,
    network: String,
    allowed_images: BTreeSet<String>,
    stop_grace_seconds: u64,
}

pub struct StartOutcome {
    pub already_running: bool,
    pub container_id: String,
}

pub struct StopOutcome {
    pub already_stopped: bool,
}

pub struct ContainerState {
    pub running: bool,
    pub exit_code: i64,
    /// Unix-tid för när containern startade (Dockers `State.StartedAt`).
    /// Används för uptime i telemetrin; None när tiden inte går att läsa.
    pub started_at_epoch: Option<i64>,
}

pub struct ContainerStats {
    pub cpu_percent: Option<f64>,
    pub memory_used_mb: Option<u64>,
    pub memory_limit_mb: Option<u64>,
}

impl Docker {
    pub fn new(
        data_root: PathBuf,
        docker_data_root: Option<PathBuf>,
        network: String,
        allowed_images: Vec<String>,
        stop_grace_seconds: u64,
    ) -> Self {
        Self {
            data_root,
            docker_data_root,
            network,
            allowed_images: allowed_images.into_iter().collect(),
            stop_grace_seconds,
        }
    }

    /// `rcon_password` sätts bara för Java-containrar: agenten slår på RCON
    /// i server.properties innan start så ett senare STOP kan be spelet
    /// stänga av sig självt.
    pub async fn start(
        &self,
        payload: &Value,
        rcon_password: Option<&str>,
    ) -> Result<StartOutcome> {
        let server_id = server_id(payload)?;
        let expected_name = container_name(&server_id);
        let spec = ContainerSpec::parse(payload, &expected_name)?;

        if !self.allowed_images.contains(&spec.image) {
            return Err(AgentError::Runtime(format!(
                "refusing to start the unapproved image {}",
                spec.image
            )));
        }
        // Mastern skickar sitt eget nätverksnamn. Agenten startar bara i
        // det nätverk den själv konfigurerats med — annars kunde en payload
        // placera spelcontainern på ett nät där agenten saknar insyn.
        if spec.network != self.network {
            return Err(AgentError::Runtime(format!(
                "container network {} does not match the configured network {}",
                spec.network, self.network
            )));
        }

        let (data_dir, created) = self.ensure_data_dir(&server_id)?;
        self.chown_new_directory(&data_dir, &spec.container_user, created);

        // Efter chown så filen får spelcontainerns ägare. Ett fel här får
        // inte stoppa starten — servern körs då utan RCON och STOP faller
        // tillbaka på SIGTERM.
        if let (true, Some(password)) = (spec.is_java(), rcon_password) {
            if let Err(err) = rcon::configure(&data_dir, password) {
                warn!(server_id = %server_id, error = %err, "could not enable rcon");
            }
        }
        let mount = self.host_data_path(&server_id, &data_dir);

        match self.inspect(&expected_name, "{{.State.Running}}").await? {
            Some(running) => {
                self.verify_managed(&server_id, &expected_name).await?;
                if running == "true" {
                    info!(server_id = %server_id, "server_start_noop reason=already_running");
                    return Ok(StartOutcome {
                        already_running: true,
                        container_id: self.container_id(&expected_name).await?,
                    });
                }

                info!(server_id = %server_id, "server_container_start_requested");
                self.run(&["start", &expected_name], 45).await?;
            }
            None => {
                info!(
                    server_id = %server_id,
                    image = %spec.image,
                    port = spec.port,
                    "server_container_create_started"
                );
                let args = self.run_args(&spec, &server_id, &mount);
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let output = self.run(&arg_refs, 45).await?;
                let candidate = output.lines().last().unwrap_or("").trim();
                if !candidate.is_empty() && !is_container_id(candidate) {
                    return Err(AgentError::Runtime(
                        "docker returned an invalid container id".into(),
                    ));
                }
            }
        }

        if self
            .inspect(&expected_name, "{{.State.Running}}")
            .await?
            .as_deref()
            != Some("true")
        {
            return Err(AgentError::Runtime(
                "container did not remain running after start".into(),
            ));
        }

        let container_id = self.container_id(&expected_name).await?;
        info!(server_id = %server_id, "server_started");
        Ok(StartOutcome {
            already_running: false,
            container_id,
        })
    }

    /// Först Minecrafts eget `stop` över RCON, så världen sparas och spelare
    /// kopplas ned städat. Saknas RCON, avvisas lösenordet eller stannar
    /// servern inte inom tidsgränsen går vi vidare till `docker stop`
    /// (SIGTERM), och `force` skickar SIGKILL direkt.
    pub async fn stop(
        &self,
        payload: &Value,
        rcon_settings: Option<&RconSettings>,
    ) -> Result<StopOutcome> {
        let server_id = server_id(payload)?;
        let name = container_name(&server_id);
        let force = payload
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let Some(running) = self.inspect(&name, "{{.State.Running}}").await? else {
            info!(server_id = %server_id, "server_stop_noop reason=container_missing");
            return Ok(StopOutcome {
                already_stopped: true,
            });
        };
        self.verify_managed(&server_id, &name).await?;

        if running == "false" {
            info!(server_id = %server_id, "server_stop_noop reason=already_stopped");
            return Ok(StopOutcome {
                already_stopped: true,
            });
        }
        if running != "true" {
            return Err(AgentError::Runtime(
                "docker returned an invalid running state".into(),
            ));
        }

        if force {
            warn!(server_id = %server_id, "server_force_stop_requested");
            self.run(&["kill", "--signal=KILL", &name], 15).await?;
        } else {
            if let Some(settings) = rcon_settings {
                info!(server_id = %server_id, "server_rcon_stop_requested");
                match rcon::send_stop(&name, settings.port, &settings.password).await {
                    Ok(()) => {
                        let limit = Duration::from_secs(self.stop_grace_seconds);
                        if self.wait_until_stopped(&name, limit).await? {
                            info!(server_id = %server_id, "server_stopped_via_rcon");
                            return Ok(StopOutcome {
                                already_stopped: false,
                            });
                        }
                        warn!(server_id = %server_id, "server_rcon_stop_did_not_exit");
                    }
                    Err(err) => {
                        warn!(server_id = %server_id, error = %err, "server_rcon_stop_failed");
                    }
                }
            }

            info!(server_id = %server_id, "server_graceful_stop_requested");
            let grace = self.stop_grace_seconds.to_string();
            self.run(
                &["stop", "--time", &grace, &name],
                self.stop_grace_seconds + 20,
            )
            .await?;
        }

        if self.inspect(&name, "{{.State.Running}}").await?.as_deref() == Some("true") {
            return Err(AgentError::Runtime(
                "container is still running after stop".into(),
            ));
        }

        info!(server_id = %server_id, "server_stopped");
        Ok(StopOutcome {
            already_stopped: false,
        })
    }

    /// Avbryter en start som hängt sig: SIGKILL direkt och aldrig RCON — en
    /// server som fastnat i STARTING svarar inte på `stop`. Mastern går till
    /// CANCELLED först när acken säger START_CANCELLED eller ALREADY_STOPPED.
    pub async fn cancel_start(&self, payload: &Value) -> Result<StopOutcome> {
        self.stop(&force_stop_payload(payload), None).await
    }

    /// Tar bort serverns container. En saknad container är ett lyckat resultat
    /// (kommandot är idempotent), men en container som inte är vår vägras innan
    /// den rörs.
    pub async fn remove_container(&self, payload: &Value) -> Result<()> {
        let server_id = server_id(payload)?;
        let name = container_name(&server_id);
        if self.inspect(&name, "{{.Id}}").await?.is_none() {
            info!(server_id = %server_id, "server_container_remove_noop reason=container_missing");
            return Ok(());
        }
        self.verify_managed(&server_id, &name).await?;
        self.run(&["rm", "--force", &name], 60).await?;
        info!(server_id = %server_id, "server_container_removed");
        Ok(())
    }

    /// Väntar på att containern lämnar `running`. RCON-stop är asynkront:
    /// spelet svarar innan det har hunnit spara klart och avsluta.
    async fn wait_until_stopped(&self, name: &str, limit: Duration) -> Result<bool> {
        let deadline = Instant::now() + limit;
        loop {
            if self.inspect(name, "{{.State.Running}}").await?.as_deref() != Some("true") {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            sleep(Duration::from_millis(500)).await;
        }
    }

    /// Skriver en rad till spelets konsol-FIFO — samma argv-form som masterns
    /// egen Docker-adapter: kommandot går som `$1`, aldrig interpolerat i
    /// skriptet. Containern måste vara vår och faktiskt köra.
    ///
    /// Returnerar den **exakta** payload-strängen, eftersom mastern verifierar
    /// `sha256(payload.command)` i acken; endast det som skrivs trimmas.
    pub async fn console(&self, payload: &Value) -> Result<String> {
        let server_id = server_id(payload)?;
        let raw = payload
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::Runtime("console command is missing".into()))?;
        let command = console_line(raw)?;
        let name = container_name(&server_id);

        let Some(running) = self.inspect(&name, "{{.State.Running}}").await? else {
            return Err(AgentError::Runtime(
                "the server container is missing".into(),
            ));
        };
        self.verify_managed(&server_id, &name).await?;
        if running != "true" {
            return Err(AgentError::Runtime("the server is not running".into()));
        }

        let script = format!("printf \"%s\\n\" \"$1\" > {CONSOLE_FILE}");
        self.run(&["exec", &name, "sh", "-c", &script, "sh", &command], 15)
            .await?;
        info!(server_id = %server_id, "server_console_command_written");
        Ok(raw.to_string())
    }

    /// Fast argv i samma form som Python-agenten använder.
    fn run_args(&self, spec: &ContainerSpec, server_id: &str, data_mount: &str) -> Vec<String> {
        let memory = format!("{}m", spec.memory_mb);
        let cpus = format!("{}", spec.cpus);
        let publish = format!("{}:{MOUNT_PORT}/tcp", spec.port);
        let volume = format!("{data_mount}:{}:rw", spec.mount_path);

        let mut args: Vec<String> = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            spec.name.clone(),
            "--label".into(),
            "craftpilot.managed=1".into(),
            "--label".into(),
            format!("craftpilot.server_id={server_id}"),
            "--network".into(),
            spec.network.clone(),
            "--memory".into(),
            memory.clone(),
            "--memory-swap".into(),
            memory,
            "--cpus".into(),
            cpus,
            "--restart".into(),
            "no".into(),
            "--user".into(),
            spec.container_user.clone(),
            "--volume".into(),
            volume,
            "--publish".into(),
            publish,
            spec.image.clone(),
        ];
        args.extend(spec.command.iter().cloned());
        args
    }

    /// Containerns tillstånd för telemetrin. Saknad container är ett
    /// giltigt svar (OFFLINE), inte ett fel.
    pub async fn state(&self, server_id: &str) -> Result<Option<ContainerState>> {
        let name = container_name(server_id);
        let Some(raw) = self.inspect(&name, "{{json .State}}").await? else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(&raw).map_err(|_| {
            AgentError::Runtime("docker returned an invalid container state".into())
        })?;

        Ok(Some(ContainerState {
            running: value.get("Running").and_then(Value::as_bool) == Some(true),
            exit_code: value.get("ExitCode").and_then(Value::as_i64).unwrap_or(0),
            started_at_epoch: value
                .get("StartedAt")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_epoch),
        }))
    }

    /// Ett `docker stats`-svar per container: CPU i procent och minne i MB.
    pub async fn stats(&self, server_id: &str) -> Result<Option<ContainerStats>> {
        let name = container_name(server_id);
        let output = self
            .run_raw(
                &["stats", "--no-stream", "--format", "{{json .}}", &name],
                15,
            )
            .await?;
        if !output.status.success() {
            let detail = normalized(&output.stderr);
            if is_missing_container_error(&detail) {
                return Ok(None);
            }
            return Err(AgentError::Runtime(format!(
                "docker stats failed: {}",
                truncate(&detail)
            )));
        }

        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if raw.is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&raw)
            .map_err(|_| AgentError::Runtime("docker returned invalid container stats".into()))?;
        let memory = value.get("MemUsage").and_then(Value::as_str);

        Ok(Some(ContainerStats {
            cpu_percent: value
                .get("CPUPerc")
                .and_then(Value::as_str)
                .and_then(percent),
            memory_used_mb: memory.and_then(|value| memory_pair(value).0),
            memory_limit_mb: memory.and_then(|value| memory_pair(value).1),
        }))
    }

    /// Nya konsolrader med Docker-tidsstämpel. En saknad container ger en
    /// tom lista — statusrapporten bär den informationen.
    pub async fn logs_since(
        &self,
        server_id: &str,
        since: Option<&str>,
    ) -> Result<Vec<(String, String)>> {
        let name = container_name(server_id);
        let mut args: Vec<&str> = vec!["logs", "--timestamps", "--tail", "400"];
        if let Some(since) = since {
            args.push("--since");
            args.push(since);
        }
        args.push(&name);

        let output = self.run_raw(&args, 20).await?;
        if !output.status.success() {
            let detail = normalized(&output.stderr);
            if is_missing_container_error(&detail) {
                return Ok(Vec::new());
            }
            return Err(AgentError::Runtime(format!(
                "docker logs failed: {}",
                truncate(&detail)
            )));
        }

        let mut lines = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some((ts, text)) = line.split_once(' ') else {
                continue;
            };
            if !ts.contains('T') || ts.len() > 40 {
                continue;
            }
            lines.push((ts.to_string(), text.to_string()));
        }
        Ok(lines)
    }

    /// Migrationer kräver att källcontainern bevisligen inte kör innan
    /// datan arkiveras. En saknad container räknas som stoppad.
    pub async fn is_running(&self, server_id: &str) -> Result<bool> {
        let name = container_name(server_id);
        Ok(self.inspect(&name, "{{.State.Running}}").await?.as_deref() == Some("true"))
    }

    async fn inspect(&self, name: &str, template: &str) -> Result<Option<String>> {
        let output = self
            .run_raw(&["inspect", "--format", template, name], 10)
            .await?;
        if output.status.success() {
            return Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            ));
        }

        let detail = normalized(&output.stderr);
        if is_missing_container_error(&detail) {
            return Ok(None);
        }
        Err(AgentError::Runtime(format!(
            "docker inspect failed: {}",
            truncate(&detail)
        )))
    }

    async fn verify_managed(&self, server_id: &str, name: &str) -> Result<()> {
        let raw = self
            .inspect(name, "{{json .Config.Labels}}")
            .await?
            .ok_or_else(|| {
                AgentError::Runtime("container disappeared while verifying ownership".into())
            })?;
        let labels: BTreeMap<String, String> = serde_json::from_str(&raw)
            .map_err(|_| AgentError::Runtime("docker returned invalid container labels".into()))?;

        if labels.get("craftpilot.managed").map(String::as_str) != Some("1")
            || labels.get("craftpilot.server_id").map(String::as_str) != Some(server_id)
        {
            return Err(AgentError::Runtime(
                "refusing to manage a container not owned by this server".into(),
            ));
        }
        Ok(())
    }

    async fn container_id(&self, name: &str) -> Result<String> {
        let value = self
            .inspect(name, "{{.Id}}")
            .await?
            .ok_or_else(|| AgentError::Runtime("container disappeared".into()))?;
        if !is_container_id(&value) {
            return Err(AgentError::Runtime(
                "docker returned an invalid container id".into(),
            ));
        }
        Ok(value)
    }

    /// Serverkatalogen måste ligga under data-roten, får inte vara en
    /// symlink och skapas med gruppskrivbar mode (spelcontainern kör som
    /// uid 1000/33-grupp).
    fn ensure_data_dir(&self, server_id: &str) -> Result<(PathBuf, bool)> {
        let path = self.data_root.join(server_id);
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                return Err(AgentError::Runtime(
                    "the server data directory is a symlink".into(),
                ));
            }
        }

        let created = !path.exists();
        std::fs::create_dir_all(&path)?;

        let root = std::fs::canonicalize(&self.data_root)?;
        let resolved = std::fs::canonicalize(&path)?;
        if !resolved.starts_with(&root) {
            return Err(AgentError::Runtime(
                "the server data directory escaped the configured root".into(),
            ));
        }

        set_mode(&resolved, 0o770)?;
        Ok((resolved, created))
    }

    fn host_data_path(&self, server_id: &str, resolved: &Path) -> String {
        match &self.docker_data_root {
            Some(root) => format!("{}/{server_id}", root.display()),
            None => resolved.display().to_string(),
        }
    }

    /// En ny katalog skapas av agenten (ofta root) medan spelcontainern kör
    /// som uid 1000 — utan chown kan Minecraft inte skriva i sin egen värld.
    /// Bara katalogen själv, aldrig rekursivt: en stor befintlig värld ska
    /// inte traverseras vid varje start.
    fn chown_new_directory(&self, path: &Path, container_user: &str, created: bool) {
        #[cfg(unix)]
        {
            if !created {
                return;
            }
            let Some((uid, gid)) = container_user.split_once(':') else {
                return;
            };
            if !uid.chars().all(|ch| ch.is_ascii_digit())
                || !gid.chars().all(|ch| ch.is_ascii_digit())
            {
                return;
            }
            let _ = std::process::Command::new("chown")
                .arg(format!("{uid}:{gid}"))
                .arg(path)
                .status();
        }
        #[cfg(not(unix))]
        {
            let _ = (path, container_user, created);
        }
    }

    async fn run(&self, args: &[&str], timeout_seconds: u64) -> Result<String> {
        let output = self.run_raw(args, timeout_seconds).await?;
        if !output.status.success() {
            let detail = normalized(&output.stderr);
            return Err(AgentError::Runtime(format!(
                "docker {} failed: {}",
                args.first().copied().unwrap_or(""),
                truncate(&detail)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn run_raw(&self, args: &[&str], timeout_seconds: u64) -> Result<Output> {
        let mut command = ProcessCommand::new("docker");
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        timeout(Duration::from_secs(timeout_seconds), command.output())
            .await
            .map_err(|_| AgentError::Runtime("docker operation timed out".into()))?
            .map_err(|err| AgentError::Runtime(format!("could not run the docker CLI: {err}")))
    }
}

#[derive(Debug)]
struct ContainerSpec {
    name: String,
    image: String,
    command: Vec<String>,
    port: u16,
    memory_mb: u64,
    cpus: f64,
    network: String,
    mount_path: String,
    container_user: String,
}

impl ContainerSpec {
    /// Bara Java-servern har `server.properties`; Pumpkin styrs via TOML och
    /// får behålla SIGTERM-vägen.
    fn is_java(&self) -> bool {
        // Mastern wrappar startkommandot i `sh -c "cd …; exec 'java' …"`, så
        // java och jar-namnet kan ligga inbäddade i ett enda argv-element.
        self.command
            .iter()
            .any(|arg| arg.contains("java") || arg.contains(".jar"))
    }

    fn parse(payload: &Value, expected_name: &str) -> Result<Self> {
        let container = payload
            .get("container")
            .and_then(Value::as_object)
            .ok_or_else(|| AgentError::Runtime("start command has no container spec".into()))?;

        let name = required_str(container, "name")?;
        if name != expected_name {
            return Err(AgentError::Runtime(
                "container specification does not match the server id".into(),
            ));
        }

        let image = required_str(container, "image")?;
        if image.is_empty() || image.len() > 255 || image.chars().any(char::is_whitespace) {
            return Err(AgentError::Runtime(
                "container specification has an invalid image".into(),
            ));
        }

        let command: Vec<String> = container
            .get("command")
            .and_then(Value::as_array)
            .ok_or_else(|| AgentError::Runtime("container specification has no command".into()))?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    AgentError::Runtime("container command must be an argv list".into())
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if command.is_empty() || command.len() > 128 {
            return Err(AgentError::Runtime(
                "container command has an invalid length".into(),
            ));
        }

        let port = required_u64(container, "port")?;
        if !(1..=65535).contains(&port) {
            return Err(AgentError::Runtime("container port is invalid".into()));
        }
        let memory_mb = required_u64(container, "memory_mb")?;
        if !(128..=65536).contains(&memory_mb) {
            return Err(AgentError::Runtime("container memory is invalid".into()));
        }
        let cpus = container
            .get("cpus")
            .and_then(Value::as_f64)
            .ok_or_else(|| AgentError::Runtime("container cpus is invalid".into()))?;
        if !(0.1..=32.0).contains(&cpus) {
            return Err(AgentError::Runtime("container cpus is invalid".into()));
        }

        let network = required_str(container, "network")?;
        if !is_docker_name(network) {
            return Err(AgentError::Runtime("container network is invalid".into()));
        }
        let mount_path = required_str(container, "mount_path")?;
        if !mount_path.starts_with('/') || mount_path.contains("..") || mount_path.contains(':') {
            return Err(AgentError::Runtime(
                "container mount path is invalid".into(),
            ));
        }
        let container_user = required_str(container, "container_user")?;
        if container_user.is_empty()
            || container_user.len() > 64
            || !container_user
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-' | '.'))
        {
            return Err(AgentError::Runtime("container user is invalid".into()));
        }

        Ok(Self {
            name: name.to_string(),
            image: image.to_string(),
            command,
            port: port as u16,
            memory_mb,
            cpus,
            network: network.to_string(),
            mount_path: mount_path.to_string(),
            container_user: container_user.to_string(),
        })
    }
}

/// `craftpilot-<första 8 hex>` — samma namn som masterns `containerName`
/// (`Str::lower(substr($id, 0, 8))`).
fn container_name(server_id: &str) -> String {
    let compact: String = server_id
        .chars()
        .filter(|ch| *ch != '-')
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("{CONTAINER_PREFIX}{compact}")
}

/// `cancel_start` stoppar hårt oavsett vad payloaden säger: mastern skickar
/// `force: false` för alla kommandon utom STOP.
fn force_stop_payload(payload: &Value) -> Value {
    let mut forced = payload.clone();
    if let Value::Object(map) = &mut forced {
        map.insert("force".to_string(), Value::Bool(true));
    }
    forced
}

pub fn server_id(payload: &Value) -> Result<String> {
    let value = payload
        .get("server_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Runtime("command has an invalid server id".into()))?;
    if !is_uuid(value) {
        return Err(AgentError::Runtime(
            "command has an invalid server id".into(),
        ));
    }
    Ok(value.to_string())
}

/// Masterns `DockerRuntimeAdapter::execute` trimmar raden och accepterar
/// högst 1000 tecken utan kontrolltecken. Samma regel här, så att agenten
/// aldrig skriver något panelen inte skulle ha skickat.
fn console_line(raw: &str) -> Result<String> {
    let command = raw.trim();
    if command.is_empty() || command.chars().count() > 1000 || command.chars().any(char::is_control)
    {
        return Err(AgentError::Runtime("console command is invalid".into()));
    }
    Ok(command.to_string())
}

pub fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        })
}

fn percent(value: &str) -> Option<f64> {
    value
        .trim()
        .trim_end_matches('%')
        .parse::<f64>()
        .ok()
        .filter(|value| (0.0..=1000.0).contains(value))
}

/// `123.4MiB / 1GiB` → (123, 1024) i MB.
fn memory_pair(value: &str) -> (Option<u64>, Option<u64>) {
    let mut parts = value.split('/');
    let used = parts.next().and_then(megabytes);
    let limit = parts.next().and_then(megabytes);
    (used, limit)
}

fn megabytes(value: &str) -> Option<u64> {
    let value = value.trim();
    let split = value.find(|ch: char| !ch.is_ascii_digit() && ch != '.')?;
    let amount = value[..split].parse::<f64>().ok()?;
    let unit = value[split..].trim().to_ascii_lowercase();
    let factor = match unit.as_str() {
        "b" => 1.0 / (1024.0 * 1024.0),
        "kib" | "kb" => 1.0 / 1024.0,
        "mib" | "mb" => 1.0,
        "gib" | "gb" => 1024.0,
        "tib" | "tb" => 1024.0 * 1024.0,
        _ => return None,
    };

    Some((amount * factor).round() as u64)
}

fn is_container_id(value: &str) -> bool {
    (12..=64).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn is_docker_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

fn required_str<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::Runtime(format!("container specification is missing {key}")))
}

fn required_u64(map: &serde_json::Map<String, Value>, key: &str) -> Result<u64> {
    map.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| AgentError::Runtime(format!("container specification is missing {key}")))
}

/// Docker Desktop svarar `Error: No such object: <namn>` — med versaler —
/// medan daemonen på Linux kan svara `No such container`. Båda betyder
/// "containern finns inte ännu" och ska leda till `docker run`, inte till
/// ett RUNTIME_ERROR.
fn is_missing_container_error(stderr: &str) -> bool {
    let lowered = stderr.to_ascii_lowercase();
    lowered.contains("no such object") || lowered.contains("no such container")
}

fn normalized(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_CAPTURED_ERROR).collect()
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(mode);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

/// RFC3339 till Unix-sekunder. Docker rapporterar alltid UTC (`Z`) för
/// `State.StartedAt`, med valfri bråkdel som ignoreras. En egen parser håller
/// agenten fri från en datumdependency för ett enda fält.
fn parse_rfc3339_epoch(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.len() < 20
        || !raw.ends_with('Z')
        || raw.as_bytes().get(4) != Some(&b'-')
        || raw.as_bytes().get(7) != Some(&b'-')
        || raw.as_bytes().get(10) != Some(&b'T')
        || raw.as_bytes().get(13) != Some(&b':')
        || raw.as_bytes().get(16) != Some(&b':')
    {
        return None;
    }

    let year: i64 = raw.get(0..4)?.parse().ok()?;
    let month: u32 = raw.get(5..7)?.parse().ok()?;
    let day: u32 = raw.get(8..10)?.parse().ok()?;
    let hour: i64 = raw.get(11..13)?.parse().ok()?;
    let minute: i64 = raw.get(14..16)?.parse().ok()?;
    let second: i64 = raw.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Dagar sedan 1970-01-01 enligt Howard Hinnants civil-algoritm.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SERVER_ID: &str = "096d46aa-4221-4e49-acd9-01624347f131";

    fn payload() -> Value {
        json!({
            "server_id": SERVER_ID,
            "force": false,
            "container": {
                "name": "craftpilot-096d46aa",
                "image": "eclipse-temurin:21-jre",
                "command": ["java", "-Xmx1024M", "-jar", "server.jar", "nogui"],
                "port": 25599,
                "memory_mb": 1024,
                "cpus": 1,
                "network": "craftpilot-servers",
                "mount_path": "/data",
                "container_user": "1000:1000"
            }
        })
    }

    fn docker() -> Docker {
        Docker::new(
            PathBuf::from("/tmp/craftpilot-test"),
            Some(PathBuf::from("/host/servers")),
            "craftpilot-servers".to_string(),
            vec!["eclipse-temurin:21-jre".to_string()],
            30,
        )
    }

    #[test]
    fn parses_docker_started_at_as_epoch_seconds() {
        assert_eq!(parse_rfc3339_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_epoch("2000-01-01T00:00:00Z"),
            Some(946_684_800)
        );
        assert_eq!(
            parse_rfc3339_epoch("2026-11-12T13:00:00.123456789Z"),
            Some(1_794_488_400)
        );
        assert_eq!(parse_rfc3339_epoch("not-a-date"), None);
        assert_eq!(parse_rfc3339_epoch("2026-13-12T13:00:00Z"), None);
    }

    #[test]
    fn recognizes_docker_desktop_missing_container_errors() {
        assert!(is_missing_container_error(
            "Error: No such object: craftpilot-4f2b2d3a"
        ));
        assert!(is_missing_container_error(
            "Error response from daemon: No such container: craftpilot-4f2b2d3a"
        ));
        assert!(!is_missing_container_error(
            "Error response from daemon: permission denied"
        ));
    }

    #[test]
    fn container_name_matches_the_master() {
        assert_eq!(container_name(SERVER_ID), "craftpilot-096d46aa");
        assert_eq!(
            container_name("134EB9B3-314F-4454-B53E-24097E0F1A07"),
            "craftpilot-134eb9b3"
        );
    }

    #[test]
    fn cancel_start_forces_a_hard_stop() {
        let original = payload();
        let forced = force_stop_payload(&original);

        assert_eq!(forced["force"].as_bool(), Some(true));
        assert_eq!(forced["server_id"], original["server_id"]);
        // Originalet lämnas orört: samma payload kan läses av flera steg.
        assert_eq!(original["force"].as_bool(), Some(false));
    }

    #[test]
    fn validates_uuids() {
        assert!(is_uuid(SERVER_ID));
        assert!(!is_uuid("096d46aa"));
        assert!(!is_uuid("096d46aa-4221-4e49-acd9-01624347f13z"));
    }

    #[test]
    fn builds_the_fixed_docker_run_argv() {
        let spec = ContainerSpec::parse(&payload(), "craftpilot-096d46aa").unwrap();
        let args = docker().run_args(&spec, SERVER_ID, "/host/servers/096d46aa");
        let joined = args.join(" ");

        assert_eq!(args[0], "run");
        assert!(joined.contains("--label craftpilot.managed=1"));
        assert!(joined.contains(&format!("--label craftpilot.server_id={SERVER_ID}")));
        assert!(joined.contains("--network craftpilot-servers"));
        assert!(joined.contains("--memory 1024m --memory-swap 1024m"));
        assert!(joined.contains("--publish 25599:25565/tcp"));
        assert!(joined.contains("--volume /host/servers/096d46aa:/data:rw"));
        assert!(args.ends_with(&[
            "eclipse-temurin:21-jre".to_string(),
            "java".to_string(),
            "-Xmx1024M".to_string(),
            "-jar".to_string(),
            "server.jar".to_string(),
            "nogui".to_string()
        ]));
    }

    #[test]
    fn rejects_a_container_name_for_another_server() {
        let error = ContainerSpec::parse(&payload(), "craftpilot-deadbeef").unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }

    #[test]
    fn rejects_a_command_that_is_not_an_argv_list() {
        let mut value = payload();
        value["container"]["command"] = json!("java -jar server.jar");
        assert!(ContainerSpec::parse(&value, "craftpilot-096d46aa").is_err());
    }

    #[test]
    fn rejects_an_out_of_range_port_and_memory() {
        let mut value = payload();
        value["container"]["port"] = json!(70000);
        assert!(ContainerSpec::parse(&value, "craftpilot-096d46aa").is_err());

        let mut value = payload();
        value["container"]["memory_mb"] = json!(16);
        assert!(ContainerSpec::parse(&value, "craftpilot-096d46aa").is_err());
    }

    #[test]
    fn parses_docker_stats_values() {
        assert_eq!(percent("12.50%"), Some(12.5));
        assert_eq!(percent("0.00%"), Some(0.0));
        assert_eq!(percent("not-a-number"), None);
        assert_eq!(percent("5000%"), None);

        assert_eq!(memory_pair("123.4MiB / 1GiB"), (Some(123), Some(1024)));
        assert_eq!(memory_pair("512KiB / 2GiB"), (Some(1), Some(2048)));
        assert_eq!(memory_pair("weird"), (None, None));
    }

    #[test]
    fn console_accepts_a_trimmed_single_line() {
        assert_eq!(console_line("  say hello  ").unwrap(), "say hello");
        assert_eq!(
            console_line("tellraw @a \"hej\"").unwrap(),
            "tellraw @a \"hej\""
        );
    }

    #[test]
    fn console_rejects_empty_control_and_oversized_input() {
        assert!(console_line("   ").is_err());
        assert!(console_line("say\u{0}hi").is_err());
        assert!(console_line("say\nhello").is_err());
        assert!(console_line(&"a".repeat(1001)).is_err());
        assert!(console_line(&"a".repeat(1000)).is_ok());
    }

    #[test]
    fn rejects_a_traversing_mount_path() {
        let mut value = payload();
        value["container"]["mount_path"] = json!("/data/../etc");
        assert!(ContainerSpec::parse(&value, "craftpilot-096d46aa").is_err());
    }

    #[test]
    fn stop_requires_a_boolean_force_flag() {
        let mut value = json!({"server_id": SERVER_ID, "force": "yes"});
        // En sträng ska inte tolkas som true; den ignoreras som false.
        assert_eq!(value["force"].as_bool(), None);
        value["force"] = json!(true);
        assert_eq!(value["force"].as_bool(), Some(true));
    }
}
