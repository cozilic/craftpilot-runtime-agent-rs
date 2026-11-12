use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::sleep;
use tracing::{info, warn};

use crate::api::MasterClient;
use crate::config::Config;
use crate::docker::Docker;
use crate::error::{AgentError, Result};
use crate::files::Files;
use crate::migrations::Migrations;
use crate::model::{AckRequest, Command};
use crate::rcon;
use crate::snapshot::Snapshotter;
use crate::state::StateStore;
use crate::telemetry::{Artifact, Reporter};

/// Etapp 1 av Rust-agenten: enrollment, heartbeat, claim och ack.
///
/// Kommandon som ännu inte är porterade ackas med `success=false` och
/// `RUNTIME_ERROR` så masterns kö inte fastnar på en lease som aldrig
/// besvaras. Det är avsiktligt synligt i panelen — ingen operation låtsas
/// lyckas.
pub struct Agent {
    cfg: Config,
    store: StateStore,
    api: MasterClient,
    docker: Docker,
    snapshot: Snapshotter,
    files: Files,
    migrations: Migrations,
    reporter: Reporter,
    last_report: Option<Instant>,
}

impl Agent {
    pub fn new(cfg: Config) -> Result<Self> {
        let store = StateStore::open(&cfg.state_dir)?;
        let api = MasterClient::new(cfg.master_url.clone())?;
        let docker = Docker::new(
            cfg.server_data_root.clone(),
            cfg.docker_data_root.clone().map(std::path::PathBuf::from),
            cfg.docker_network.clone(),
            cfg.allowed_images.clone(),
            cfg.stop_grace_seconds,
        );
        let snapshot = Snapshotter::new(
            cfg.server_data_root.clone(),
            cfg.master_url.clone(),
            cfg.data_gid,
        );
        let files = Files::new(
            cfg.server_data_root.clone(),
            cfg.max_edit_bytes,
            cfg.max_upload_bytes,
        );
        let migrations = Migrations::new(
            cfg.server_data_root.clone(),
            cfg.state_dir.clone(),
            cfg.data_gid,
        );
        Ok(Self {
            cfg,
            store,
            api,
            docker,
            snapshot,
            files,
            migrations,
            reporter: Reporter::new(),
            last_report: None,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        self.ensure_credentialed().await?;
        self.handshake().await?;
        self.main_loop().await
    }

    /// Sparad credential vinner; annars engångstoken; annars själv-enrollment.
    async fn ensure_credentialed(&mut self) -> Result<()> {
        if let (Some(node_id), Some(credential)) = (
            self.store.state().node_id.clone(),
            self.store.state().credential.clone(),
        ) {
            self.api.set_credential(Some(credential));
            info!(node_id = %node_id, "using the saved node credential");
            return Ok(());
        }

        if let (Some(node_id), Some(token)) = (
            self.cfg.node_id.clone(),
            self.cfg.registration_token.clone(),
        ) {
            let registered = self.api.register(&node_id, &token).await?;
            self.api.set_credential(Some(registered.credential.clone()));
            {
                let state = self.store.state_mut();
                state.node_id = Some(registered.node_id.clone());
                state.credential = Some(registered.credential);
                state.enrollment = None;
            }
            self.store.save()?;
            info!(node_id = %registered.node_id, "registered with the configured registration token");
            return Ok(());
        }

        self.self_enroll().await
    }

    /// Ett heartbeat direkt efter enrollment bevisar att credentialen
    /// fungerar innan första claim, och ger masterns intervall tillbaka.
    async fn handshake(&mut self) -> Result<()> {
        let node_id = self
            .store
            .state()
            .node_id
            .clone()
            .ok_or(AgentError::Unauthorized)?;
        let sequence = self.store.next_sequence()?;
        let heartbeat = self.api.heartbeat(&node_id, sequence).await?;
        info!(
            agent_version = %heartbeat.agent_version,
            heartbeat_interval_seconds = heartbeat.heartbeat_interval_seconds,
            server_data_root = %self.cfg.server_data_root.display(),
            "heartbeat accepted"
        );
        Ok(())
    }

    /// Skriver identity.conf och skriver ut koden så den syns i terminalen.
    async fn announce_claim_code(
        &mut self,
        claim_code: &str,
        request_id: &str,
        expires_at: &str,
    ) -> Result<()> {
        self.store
            .write_identity_file(claim_code, request_id, expires_at)?;
        info!(
            claim_code = %claim_code,
            claim_expires_at = %expires_at,
            identity_file = %self.store.identity_file_path().display(),
            "enter this claim code under My node in CraftPilot (also saved in identity.conf)"
        );
        Ok(())
    }

    async fn self_enroll(&mut self) -> Result<()> {
        let mut enrollment = match self.store.state().enrollment.clone() {
            Some(saved) => {
                // Ny start med ett sparat ärende: förnya koden så den som
                // ligger i terminalen/identity.conf alltid är giltig.
                match self
                    .api
                    .refresh_claim_code(&saved.request_id, &saved.request_secret)
                    .await
                {
                    Ok(refreshed) => {
                        self.announce_claim_code(
                            &refreshed.claim_code,
                            &saved.request_id,
                            refreshed.claim_expires_at.as_deref().unwrap_or(""),
                        )
                        .await?;
                    }
                    Err(err) => warn!(
                        error = %err,
                        request_id = %saved.request_id,
                        "could not refresh the claim code; keeping the saved enrollment"
                    ),
                }
                saved
            }
            None => {
                let created = self
                    .api
                    .create_enrollment(&self.cfg.hostname, &self.cfg.agent_version)
                    .await?;
                info!(
                    request_id = %created.request_id,
                    expires_at = %created.expires_at,
                    "filed an enrollment request — link it under My node in CraftPilot"
                );
                if let Some(code) = created.claim_code.clone() {
                    self.announce_claim_code(
                        &code,
                        &created.request_id,
                        created.claim_expires_at.as_deref().unwrap_or(""),
                    )
                    .await?;
                }
                let enrollment = created.into_enrollment();
                self.store.state_mut().enrollment = Some(enrollment.clone());
                self.store.save()?;
                enrollment
            }
        };

        loop {
            let poll = self
                .api
                .poll_enrollment(&enrollment.request_id, &enrollment.request_secret)
                .await?;

            match poll.status.as_str() {
                "approved" => {
                    let (Some(node_id), Some(credential)) =
                        (poll.node_id.clone(), poll.credential.clone())
                    else {
                        warn!(
                            request_id = %enrollment.request_id,
                            "approved without a credential yet — polling again"
                        );
                        sleep(Duration::from_secs(5)).await;
                        continue;
                    };

                    self.api.set_credential(Some(credential.clone()));
                    {
                        let state = self.store.state_mut();
                        state.node_id = Some(node_id.clone());
                        state.credential = Some(credential);
                    }
                    self.store.save()?;

                    self.api
                        .complete_enrollment(&enrollment.request_id, &enrollment.request_secret)
                        .await?;
                    self.store.state_mut().enrollment = None;
                    self.store.save()?;
                    // Koden är förbrukad: ta bort den från disk.
                    if let Err(err) = self.store.clear_identity_file() {
                        warn!(error = %err, "could not remove the used identity.conf");
                    }

                    info!(node_id = %node_id, "enrollment completed");
                    return Ok(());
                }
                "pending" => sleep(Duration::from_secs(15)).await,
                "rejected" => {
                    warn!(request_id = %enrollment.request_id, "the enrollment request was rejected");
                    return Err(AgentError::Rejected {
                        status: 403,
                        body: "the enrollment request was rejected".to_string(),
                    });
                }
                "expired" | "invalid" | "completed" => {
                    // Ett borta/gammalt ärende får aldrig retras för evigt
                    // (Python-buggen `master_request_failed status=401`):
                    // släpp det sparade ärendet och fil:a ett nytt.
                    warn!(
                        request_id = %enrollment.request_id,
                        status = %poll.status,
                        "the enrollment request is gone — filing a new one"
                    );
                    self.store.state_mut().enrollment = None;
                    self.store.save()?;

                    let created = self
                        .api
                        .create_enrollment(&self.cfg.hostname, &self.cfg.agent_version)
                        .await?;
                    info!(request_id = %created.request_id, "filed a replacement enrollment request");
                    if let Some(code) = created.claim_code.clone() {
                        self.announce_claim_code(
                            &code,
                            &created.request_id,
                            created.claim_expires_at.as_deref().unwrap_or(""),
                        )
                        .await?;
                    }
                    enrollment = created.into_enrollment();
                    self.store.state_mut().enrollment = Some(enrollment.clone());
                    self.store.save()?;
                }
                other => {
                    return Err(AgentError::Config(format!(
                        "unexpected enrollment status: {other}"
                    )))
                }
            }
        }
    }

    async fn main_loop(&mut self) -> Result<()> {
        let poll = Duration::from_secs(self.cfg.poll_seconds);

        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    result?;
                    info!("shutdown requested");
                    return Ok(());
                }
                _ = sleep(poll) => {
                    if let Err(err) = self.tick().await {
                        if matches!(err, AgentError::Unauthorized) {
                            warn!("the master rejected the credential — filing a replacement enrollment request");
                            self.forget_credential()?;
                            if let Err(enroll_err) = self.self_enroll().await {
                                warn!(error = %enroll_err, "re-enrollment failed; retrying on the next poll");
                            }
                        } else {
                            warn!(error = %err, "poll failed");
                        }
                    }
                }
            }
        }
    }

    async fn tick(&mut self) -> Result<()> {
        let node_id = self
            .store
            .state()
            .node_id
            .clone()
            .ok_or(AgentError::Unauthorized)?;
        let sequence = self.store.next_sequence()?;
        let claim = self.api.claim(&node_id, sequence).await?;

        // Telemetrin hör inte till kommandokön: den måste skickas även när
        // claim är tomt, annars stannar panelens status så snart inget körs.
        let report_due = self
            .last_report
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(30));
        if report_due {
            self.report_servers().await;
        }

        let Some(command) = claim.command else {
            return Ok(());
        };

        info!(
            command_id = %command.id,
            command_type = %command.command_type,
            "claimed a command"
        );

        let ack = self.execute(&command).await;
        let result = self.api.acknowledge(&node_id, &command.id, &ack).await;
        let urgent = matches!(
            command.command_type.as_str(),
            "start" | "stop" | "migration_stop"
        );
        if urgent && !report_due {
            self.report_servers().await;
        }
        match result {
            Ok(_) => Ok(()),
            Err(AgentError::Conflict(body)) => {
                warn!(command_id = %command.id, error = %body, "lease expired before the acknowledge");
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// STOP går först över RCON (Minecrafts eget `stop`), med `docker stop`
    /// som fallback. Migrationer använder samma väg så en källserver alltid
    /// stängs av städat innan datan arkiveras.
    async fn stop_server(&self, payload: &Value) -> Result<crate::docker::StopOutcome> {
        let settings = self.rcon_settings(payload);
        self.docker.stop(payload, settings.as_ref()).await
    }

    /// Lösenordet skapas en gång per Java-server och sparas i state-filen
    /// (0600). Ett state-fel får inte stoppa en start — då kör servern utan
    /// RCON och STOP använder SIGTERM som vanligt.
    fn ensure_rcon_password(&mut self, payload: &Value) -> Option<String> {
        let server_id = crate::docker::server_id(payload).ok()?;
        if !rcon::payload_is_java(payload) {
            return None;
        }
        if let Some(password) = self.store.state().rcon_passwords.get(&server_id) {
            return Some(password.clone());
        }

        let password = rcon::generate_password();
        self.store
            .state_mut()
            .rcon_passwords
            .insert(server_id.clone(), password.clone());
        if let Err(err) = self.store.save() {
            warn!(server_id = %server_id, error = %err, "could not persist the rcon password");
            return None;
        }
        info!(server_id = %server_id, "generated an rcon password");
        Some(password)
    }

    fn rcon_settings(&self, payload: &Value) -> Option<rcon::RconSettings> {
        let server_id = crate::docker::server_id(payload).ok()?;
        let password = self.store.state().rcon_passwords.get(&server_id)?.clone();
        let port =
            rcon::read_port(&self.cfg.server_data_root.join(&server_id)).unwrap_or(rcon::RCON_PORT);
        Some(rcon::RconSettings { port, password })
    }

    /// Kör START/STOP via Docker. Övriga operationer ackas ärligt som
    /// RUNTIME_ERROR så att masterns kö aldrig fastnar på en död lease.
    async fn execute(&mut self, command: &Command) -> AckRequest {
        let (success, result_code, message, container_id) = match command.command_type.as_str() {
            "start" => {
                let password = self.ensure_rcon_password(&command.payload);
                match self
                    .docker
                    .start(&command.payload, password.as_deref())
                    .await
                {
                    Ok(outcome) => (
                        true,
                        if outcome.already_running {
                            "ALREADY_RUNNING"
                        } else {
                            "STARTED"
                        }
                        .to_string(),
                        None,
                        Some(outcome.container_id),
                    ),
                    Err(err) => {
                        warn!(command_id = %command.id, error = %err, "start failed");
                        (
                            false,
                            "RUNTIME_ERROR".to_string(),
                            Some(ack_message(&err)),
                            None,
                        )
                    }
                }
            }
            "file_list" | "file_read" | "file_mutate" => {
                return self.file_command(command).await;
            }
            "file_download" => return self.download_command(command).await,
            "migration_prepare"
            | "migration_export"
            | "migration_import"
            | "migration_commit"
            | "migration_delete_source" => {
                return self.migration_command(command).await;
            }
            "migration_stop" => match self.stop_server(&command.payload).await {
                Ok(_) => (true, "MIGRATION_STOPPED".to_string(), None, None),
                Err(err) => {
                    warn!(command_id = %command.id, error = %err, "migration stop failed");
                    (
                        false,
                        "RUNTIME_ERROR".to_string(),
                        Some(ack_message(&err)),
                        None,
                    )
                }
            },
            "console" => match self.docker.console(&command.payload).await {
                Ok(raw) => (
                    true,
                    "CONSOLE_EXECUTED".to_string(),
                    Some(sha256_hex(&raw)),
                    None,
                ),
                Err(err) => {
                    warn!(command_id = %command.id, error = %err, "console failed");
                    (
                        false,
                        "RUNTIME_ERROR".to_string(),
                        Some(ack_message(&err)),
                        None,
                    )
                }
            },
            "snapshot" => {
                let node_id = self.store.state().node_id.clone();
                match node_id {
                    Some(node_id) => {
                        match self.snapshot.stage(&node_id, command, &self.api).await {
                            Ok(outcome) => (
                                true,
                                "SNAPSHOT_STAGED".to_string(),
                                Some(outcome.sha256),
                                Some(outcome.bytes.to_string()),
                            ),
                            Err(err) => {
                                warn!(command_id = %command.id, error = %err, "snapshot failed");
                                (
                                    false,
                                    "RUNTIME_ERROR".to_string(),
                                    Some(ack_message(&err)),
                                    None,
                                )
                            }
                        }
                    }
                    None => (
                        false,
                        "RUNTIME_ERROR".to_string(),
                        Some("the agent has no node id".to_string()),
                        None,
                    ),
                }
            }
            "stop" => match self.stop_server(&command.payload).await {
                Ok(outcome) => (
                    true,
                    if outcome.already_stopped {
                        "ALREADY_STOPPED"
                    } else {
                        "STOPPED"
                    }
                    .to_string(),
                    None,
                    None,
                ),
                Err(err) => {
                    warn!(command_id = %command.id, error = %err, "stop failed");
                    (
                        false,
                        "RUNTIME_ERROR".to_string(),
                        Some(ack_message(&err)),
                        None,
                    )
                }
            },
            other => {
                warn!(command_id = %command.id, command_type = other, "command is not implemented");
                (
                    false,
                    "RUNTIME_ERROR".to_string(),
                    Some(format!("{other} is not implemented in the Rust agent yet")),
                    None,
                )
            }
        };

        AckRequest {
            lease_token: command.lease_token.clone(),
            success,
            result_code,
            message,
            container_id,
        }
    }

    /// Filoperationer levererar sitt `result_data` till mastern FÖRE acken
    /// — masterns ack verifierar att resultatet finns och matchar
    /// kommandots payload.
    async fn file_command(&self, command: &Command) -> AckRequest {
        let Some(node_id) = self.store.state().node_id.clone() else {
            return failure(command, "the agent has no node id");
        };

        let (result, result_code) = match command.command_type.as_str() {
            "file_list" => (self.files.list(&command.payload), "FILE_LISTED"),
            "file_read" => (self.files.read(&command.payload), "FILE_READ"),
            _ => (
                self.files
                    .mutate(
                        &command.payload,
                        &self.api,
                        &node_id,
                        &command.id,
                        &command.lease_token,
                    )
                    .await,
                "FILE_MUTATED",
            ),
        };

        let value = match result {
            Ok(value) => value,
            Err(err) => {
                warn!(command_id = %command.id, error = %err, "file operation failed");
                return failure(command, &ack_message(&err));
            }
        };

        if let Err(err) = self
            .api
            .store_result(&node_id, &command.id, &command.lease_token, &value)
            .await
        {
            warn!(command_id = %command.id, error = %err, "storing the file result failed");
            return failure(command, &ack_message(&err));
        }

        AckRequest {
            lease_token: command.lease_token.clone(),
            success: true,
            result_code: result_code.to_string(),
            message: None,
            container_id: None,
        }
    }

    /// Filnedladdning: mastern får råbytes via PUT innan acken, och acken
    /// måste bära samma sha256 (message) och storlek (container_id) som
    /// mastern sparade.
    async fn download_command(&self, command: &Command) -> AckRequest {
        let Some(node_id) = self.store.state().node_id.clone() else {
            return failure(command, "the agent has no node id");
        };

        let (path, bytes) = match self.files.download_source(&command.payload) {
            Ok(source) => source,
            Err(err) => {
                warn!(command_id = %command.id, error = %err, "download source is invalid");
                return failure(command, &ack_message(&err));
            }
        };
        let digest = match tokio::task::spawn_blocking({
            let path = path.clone();
            move || sha256_file(&path)
        })
        .await
        {
            Ok(Ok(digest)) => digest,
            Ok(Err(err)) => {
                warn!(command_id = %command.id, error = %err, "hashing the download failed");
                return failure(command, "the file could not be read");
            }
            Err(_) => return failure(command, "hashing the download failed"),
        };

        let stored = match self
            .api
            .upload_file_result(&node_id, &command.id, &command.lease_token, &path, bytes)
            .await
        {
            Ok(stored) => stored,
            Err(err) => {
                warn!(command_id = %command.id, error = %err, "uploading the download failed");
                return failure(command, &ack_message(&err));
            }
        };
        let stored_sha = stored.get("sha256").and_then(Value::as_str);
        let stored_bytes = stored.get("bytes").and_then(Value::as_u64);
        if stored_sha != Some(digest.as_str()) || stored_bytes != Some(bytes) {
            warn!(command_id = %command.id, "the master stored a different file than was sent");
            return failure(command, "the master stored a different file than was sent");
        }

        AckRequest {
            lease_token: command.lease_token.clone(),
            success: true,
            result_code: "FILE_DOWNLOADED".to_string(),
            message: Some(digest),
            container_id: Some(bytes.to_string()),
        }
    }

    /// Migrationer: destinationen skapar nycklar och importerar, källan
    /// exporterar och raderar först efter masterns uttryckliga tillstånd.
    async fn migration_command(&self, command: &Command) -> AckRequest {
        let Some(node_id) = self.store.state().node_id.clone() else {
            return failure(command, "the agent has no node id");
        };

        let outcome = match command.command_type.as_str() {
            "migration_prepare" => self
                .migrations
                .prepare(&command.payload, &node_id)
                .map(|key| ("MIGRATION_PREPARED", Some(key), None)),
            "migration_export" => self
                .migrations
                .export(&command.payload, &node_id, &self.docker, &self.api, command)
                .await
                .map(|done| {
                    (
                        "MIGRATION_EXPORTED",
                        Some(done.sha256),
                        Some(done.bytes.to_string()),
                    )
                }),
            "migration_import" => self
                .migrations
                .import(&command.payload, &node_id, &self.api, command)
                .await
                .map(|done| {
                    (
                        "MIGRATION_STAGED",
                        Some(done.sha256),
                        Some(done.bytes.to_string()),
                    )
                }),
            "migration_commit" => self
                .migrations
                .commit(&command.payload, &node_id)
                .map(|_| ("MIGRATION_COMMITTED", None, None)),
            _ => self
                .migrations
                .delete_source(&command.payload, &node_id)
                .map(|_| ("MIGRATION_SOURCE_DELETED", None, None)),
        };

        match outcome {
            Ok((result_code, message, container_id)) => AckRequest {
                lease_token: command.lease_token.clone(),
                success: true,
                result_code: result_code.to_string(),
                message,
                container_id,
            },
            Err(err) => {
                warn!(command_id = %command.id, error = %err, "migration command failed");
                failure(command, &ack_message(&err))
            }
        }
    }

    /// Rapporterar status, containerdata, spelare och nya konsolrader för
    /// varje server som mastern tilldelat noden. Varje rapport konsumerar
    /// ett nytt sekvensnummer (samma räknare som heartbeat/claim), annars
    /// avvisar mastern den som replay.
    async fn report_servers(&mut self) {
        let Some(node_id) = self.store.state().node_id.clone() else {
            return;
        };
        let listing = match self.api.servers(&node_id).await {
            Ok(listing) => listing,
            Err(err) => {
                warn!(error = %err, "fetching the assigned servers failed");
                return;
            }
        };
        let Some(servers) = listing.get("servers").and_then(Value::as_array) else {
            return;
        };
        let artifacts = listing.get("artifacts");

        for id in servers.iter().filter_map(Value::as_str) {
            let artifact = artifacts.and_then(|map| map.get(id)).and_then(|entry| {
                Some(Artifact {
                    jar: entry.get("jar").and_then(Value::as_str)?,
                    min_bytes: entry.get("min_bytes").and_then(Value::as_u64).unwrap_or(0),
                })
            });
            let since = self.store.log_cursor(id).map(str::to_string);
            let sample = match self
                .reporter
                .sample(
                    &self.docker,
                    &self.cfg.server_data_root,
                    id,
                    since.as_deref(),
                    artifact,
                )
                .await
            {
                Ok(sample) => sample,
                Err(err) => {
                    warn!(server_id = %id, error = %err, "sampling server telemetry failed");
                    continue;
                }
            };
            if let Some(cursor) = sample.cursor.as_deref() {
                if since.as_deref() != Some(cursor) {
                    if let Err(err) = self.store.set_log_cursor(id, cursor) {
                        warn!(server_id = %id, error = %err, "saving the log cursor failed");
                    }
                }
            }

            let sequence = match self.store.next_sequence() {
                Ok(sequence) => sequence,
                Err(err) => {
                    warn!(error = %err, "advancing the report sequence failed");
                    return;
                }
            };

            let mut report = serde_json::Map::new();
            report.insert("sequence".into(), json!(sequence));
            report.insert("server_id".into(), json!(id));
            report.insert("status".into(), json!(sample.status));
            if let Some(value) = sample.cpu_percent {
                report.insert("cpu_percent".into(), json!(value));
            }
            if let Some(value) = sample.memory_used_mb {
                report.insert("memory_used_mb".into(), json!(value));
            }
            if let Some(value) = sample.memory_limit_mb {
                report.insert("memory_limit_mb".into(), json!(value));
            }
            if let Some(value) = sample.players_online {
                report.insert("players_online".into(), json!(value));
            }
            if let Some(value) = sample.players_max {
                report.insert("players_max".into(), json!(value));
            }
            if let Some(value) = sample.disk_used_mb {
                report.insert("disk_used_mb".into(), json!(value));
            }
            if let Some(value) = sample.artifact_present {
                report.insert("artifact_present".into(), json!(value));
            }
            if !sample.logs.is_empty() {
                let logs: Vec<Value> = sample
                    .logs
                    .iter()
                    .map(|(ts, text)| json!({ "ts": ts, "text": text }))
                    .collect();
                report.insert("logs".into(), Value::Array(logs));
            }

            match self
                .api
                .report_server(&node_id, &Value::Object(report))
                .await
            {
                Ok(_) => {}
                Err(AgentError::Conflict(body)) => {
                    warn!(server_id = %id, error = %body, "the report was rejected as a replay");
                }
                Err(err) => warn!(server_id = %id, error = %err, "reporting the server failed"),
            }
        }

        self.last_report = Some(Instant::now());
    }

    fn forget_credential(&mut self) -> Result<()> {
        {
            let state = self.store.state_mut();
            state.node_id = None;
            state.credential = None;
            state.enrollment = None;
        }
        self.store.save()?;
        self.api.set_credential(None);
        Ok(())
    }
}

/// Mastern tillåter högst 300 tecken i ack:ens `message`; ett längre
/// Docker-fel ska trunkeras, inte göra acken ogiltig.
fn ack_message(error: &AgentError) -> String {
    error.to_string().chars().take(300).collect()
}

fn failure(command: &Command, message: &str) -> AckRequest {
    AckRequest {
        lease_token: command.lease_token.clone(),
        success: false,
        result_code: "RUNTIME_ERROR".to_string(),
        message: Some(message.to_string()),
        container_id: None,
    }
}

/// Läser filen i 1 MiB-block så stora servrar inte behöver ligga i minnet.
fn sha256_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Mastern verifierar `sha256(payload.command)` i console-acken.
fn sha256_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn console_ack_digest_matches_the_master_contract() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let digest = sha256_hex("say hello");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(digest, sha256_hex("say hello "));
    }
}
