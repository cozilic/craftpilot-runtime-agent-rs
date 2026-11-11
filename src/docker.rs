use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command as ProcessCommand;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::error::{AgentError, Result};

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

    pub async fn start(&self, payload: &Value) -> Result<StartOutcome> {
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

    /// Utan RCON skickar vi SIGTERM via `docker stop` (JVM:ens shutdown hook
    /// sparar världen) och faller tillbaka på SIGKILL först när `force` är
    /// satt eller masterns tidsgräns redan har passerat.
    pub async fn stop(&self, payload: &Value) -> Result<StopOutcome> {
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
