use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::sleep;
use tracing::{info, warn};

use crate::api::MasterClient;
use crate::config::Config;
use crate::docker::Docker;
use crate::error::{AgentError, Result};
use crate::file_tree::{FileTreeIndex, SYNC_BATCH};
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
    file_tree: Arc<FileTreeIndex>,
    migrations: Migrations,
    reporter: Reporter,
    last_report: Option<Instant>,
    assigned_servers: BTreeSet<String>,
    last_file_tree_progress: Option<String>,
    started_at: Instant,
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
        let file_tree = Arc::new(FileTreeIndex::new(
            cfg.server_data_root.clone(),
            cfg.state_dir.clone(),
        ));
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
            file_tree,
            migrations,
            reporter: Reporter::new(),
            last_report: None,
            assigned_servers: BTreeSet::new(),
            last_file_tree_progress: None,
            started_at: Instant::now(),
        })
    }

    pub async fn run(mut self) -> Result<()> {
        self.file_tree.start();
        self.start_session().await?;
        self.main_loop().await
    }

    /// Startar en session: säkerställ credential och en accepterad handshake.
    ///
    /// En sparad credential som mastern inte längre känner igen (noden är
    /// borttagen eller återskapad i panelen) får inte döda processen: då
    /// startar containern om i en evig loop med samma döda credential. Den
    /// släpps i stället och en ny enrollment fil:as, så agenten skriver ut en
    /// färsk claim-kod i loggen och väntar på att noden kopplas igen.
    async fn start_session(&mut self) -> Result<()> {
        loop {
            self.ensure_credentialed().await?;
            match self.handshake().await {
                Ok(()) => return Ok(()),
                Err(err) if needs_new_credential(&err) => {
                    warn!(
                        "the master rejected the saved node credential — filing a replacement enrollment request"
                    );
                    self.forget_credential()?;
                    // En registreringstoken som mastern nekar ska inte
                    // försökas igen i en tight loop: nästa varv går via
                    // själv-enrollment, som väntar på en ny claim.
                    if self.cfg.registration_token.is_some() {
                        self.cfg.registration_token = None;
                    }
                    sleep(Duration::from_secs(5)).await;
                }
                Err(err) => return Err(err),
            }
        }
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
        let heartbeat = self
            .api
            .heartbeat(&node_id, sequence, self.uptime_seconds())
            .await?;
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

    /// Fil:ar en ny anmälan, sparar ärendet och skriver ut claim-koden.
    async fn create_enrollment(&mut self) -> Result<crate::state::Enrollment> {
        let created = self
            .api
            .create_enrollment(
                &self.cfg.hostname,
                &self.cfg.agent_version,
                self.cfg.reported_address.as_deref(),
            )
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
        Ok(enrollment)
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
                        saved
                    }
                    // Ärendet finns inte längre hos mastern: avvisat, utgånget
                    // eller redan förbrukat. Ett request-secret som mastern inte
                    // känner igen svarar 401, så det kan aldrig bli `invalid` i
                    // poll-svaret. Släpp ärendet och fil:a ett nytt, annars står
                    // agenten utan claim-kod för alltid.
                    Err(err) if needs_new_credential(&err) => {
                        warn!(
                            request_id = %saved.request_id,
                            "the saved enrollment request is gone — filing a new one"
                        );
                        self.create_enrollment().await?
                    }
                    Err(err) => {
                        warn!(
                            error = %err,
                            request_id = %saved.request_id,
                            "could not refresh the claim code; keeping the saved enrollment"
                        );
                        saved
                    }
                }
            }
            None => self.create_enrollment().await?,
        };

        loop {
            let poll = match self
                .api
                .poll_enrollment(&enrollment.request_id, &enrollment.request_secret)
                .await
            {
                Ok(poll) => poll,
                // Samma sak för poll: ett ärende mastern glömt bort (t.ex.
                // städat efter 24 h) svarar 401. Då ska agenten fil:a ett nytt
                // i stället för att dö med det gamla ärendet kvar på disk.
                Err(err) if needs_new_credential(&err) => {
                    warn!(
                        request_id = %enrollment.request_id,
                        "the enrollment request is unknown to the master — filing a new one"
                    );
                    enrollment = self.create_enrollment().await?;
                    continue;
                }
                Err(err) => return Err(err),
            };

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
                    // (Python-buggen `master_request_failed status=401`): fil:a
                    // ett nytt ärende i stället.
                    warn!(
                        request_id = %enrollment.request_id,
                        status = %poll.status,
                        "the enrollment request is gone — filing a new one"
                    );
                    enrollment = self.create_enrollment().await?;
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
                        if needs_new_credential(&err) {
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

    /// Agentprocessens drifttid. Skickas med heartbeat/claim så ägarvyn kan
    /// visa hur länge noden varit igång sedan senaste omstart.
    fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    async fn tick(&mut self) -> Result<()> {
        let node_id = self
            .store
            .state()
            .node_id
            .clone()
            .ok_or(AgentError::Unauthorized)?;
        let sequence = self.store.next_sequence()?;
        let claim = self
            .api
            .claim(&node_id, sequence, self.uptime_seconds())
            .await?;

        // Telemetrin hör inte till kommandokön: den måste skickas även när
        // claim är tomt, annars stannar panelens status så snart inget körs.
        let report_due = self
            .last_report
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(30));
        let assigned = if report_due {
            self.report_servers().await
        } else {
            self.assigned_servers.clone()
        };
        self.file_tree.set_assigned(&assigned);
        self.sync_file_tree(&assigned).await;

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
            "start" | "stop" | "cancel_start" | "delete_server" | "migration_stop"
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

    /// `cancel_start` avbryter en hängande STARTING. Mastern sätter CANCELLED
    /// först när acken bekräftar, så statusen speglar vad noden faktiskt gjorde.
    async fn cancel_start(&self, payload: &Value) -> Result<crate::docker::StopOutcome> {
        self.docker.cancel_start(payload).await
    }

    /// `delete_server` tar bort serverns runtime-spår på noden: containern
    /// först och datakatalogen sist, så en process som fortfarande skriver inte
    /// kan återskapa katalogen. Båda stegen är idempotenta — saknad container
    /// och saknad katalog är ett lyckat resultat.
    async fn delete_server(&mut self, payload: &Value) -> Result<()> {
        let server_id = crate::docker::server_id(payload)?;
        self.docker.remove_container(payload).await?;

        match deletable_server_data(&self.cfg.server_data_root, &server_id)? {
            Some(directory) => {
                std::fs::remove_dir_all(&directory).map_err(|err| {
                    AgentError::Runtime(format!("server data could not be removed: {err}"))
                })?;
                info!(server_id = %server_id, "server_data_deleted");
            }
            None => {
                info!(server_id = %server_id, "server_data_delete_noop reason=directory_missing");
            }
        }

        self.forget_rcon_password(&server_id);
        Ok(())
    }

    /// Lösenordet hör till serverns datakatalog: när den är borta ska inget
    /// gammalt lösenord ligga kvar i state-filen.
    fn forget_rcon_password(&mut self, server_id: &str) {
        if self
            .store
            .state_mut()
            .rcon_passwords
            .remove(server_id)
            .is_none()
        {
            return;
        }
        if let Err(err) = self.store.save() {
            warn!(server_id = %server_id, error = %err, "could not persist the removed rcon password");
        }
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
            "install_artifact" => return self.install_command(command).await,
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
            "cancel_start" => match self.cancel_start(&command.payload).await {
                Ok(outcome) => (
                    true,
                    if outcome.already_stopped {
                        "ALREADY_STOPPED"
                    } else {
                        "START_CANCELLED"
                    }
                    .to_string(),
                    None,
                    None,
                ),
                Err(err) => {
                    warn!(command_id = %command.id, error = %err, "cancel start failed");
                    (
                        false,
                        "RUNTIME_ERROR".to_string(),
                        Some(ack_message(&err)),
                        None,
                    )
                }
            },
            "delete_server" => match self.delete_server(&command.payload).await {
                Ok(()) => (true, "SERVER_DELETED".to_string(), None, None),
                Err(err) => {
                    warn!(command_id = %command.id, error = %err, "delete server failed");
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

    /// Artefaktinstallation (docs/RUNTIME-NODE-ARTIFACT-INSTALL.md): mastern
    /// har redan verifierat filen; agenten hämtar den med sin lease, verifierar
    /// storlek + SHA-256 och placerar den atomiskt i serverroten.
    ///
    /// Resultatkoden säger vad som hände. Panelens "installerad" kommer från
    /// nästa serverreport (`artifact_present`), aldrig från den här acken.
    async fn install_command(&self, command: &Command) -> AckRequest {
        let Some(node_id) = self.store.state().node_id.clone() else {
            return failure(command, "the agent has no node id");
        };
        let server_id = match crate::docker::server_id(&command.payload) {
            Ok(server_id) => server_id,
            Err(err) => {
                return AckRequest {
                    lease_token: command.lease_token.clone(),
                    success: false,
                    result_code: "ARTIFACT_INVALID".to_string(),
                    message: Some(ack_message(&err)),
                    container_id: None,
                }
            }
        };

        // Skriv aldrig i en serverrot vars container kör: filen skulle bytas
        // under drift och nästa start hade laddat en annan binär.
        let running = match self.docker.is_running(&server_id).await {
            Ok(running) => running,
            Err(err) => return failure(command, &ack_message(&err)),
        };

        let outcome = self
            .files
            .install_artifact(
                &command.payload,
                &self.api,
                &node_id,
                &command.id,
                &command.lease_token,
                running,
            )
            .await;

        AckRequest {
            lease_token: command.lease_token.clone(),
            success: outcome.result_code == "ARTIFACT_INSTALLED",
            result_code: outcome.result_code.to_string(),
            message: Some(outcome.message),
            container_id: None,
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

        if command.command_type == "file_mutate" {
            if let Some(server_id) = command.server_id() {
                self.file_tree
                    .invalidate_after_mutation(server_id, &command.payload);
            }
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

    /// Håller masterns filcache uppdaterad. En full återscanning sker vid
    /// start, när tilldelningen ändras och sedan var 30:e sekund; egna
    /// mutationer invaliderar bara den berörda katalogen.
    async fn sync_file_tree(&mut self, assigned: &BTreeSet<String>) {
        let Some(node_id) = self.store.state().node_id.clone() else {
            return;
        };
        let index = Arc::clone(&self.file_tree);
        let stage = index.progress();
        if self.last_file_tree_progress.as_deref() != Some(stage.as_str()) {
            match self.api.report_file_tree_progress(&node_id, &stage).await {
                Ok(_) => self.last_file_tree_progress = Some(stage),
                Err(err) => warn!(error = %err, "reporting file tree progress failed"),
            }
        }

        loop {
            let pending = index.pending_updates(assigned, SYNC_BATCH);
            if pending.is_empty() {
                break;
            }
            let directories = pending
                .iter()
                .map(|update| update.payload.clone())
                .collect::<Vec<_>>();
            let payload = json!({ "directories": directories });
            match self.api.sync_file_tree(&node_id, &payload).await {
                Ok(_) => index.acknowledge(&pending),
                Err(err) => {
                    warn!(error = %err, "syncing the file tree failed; keeping the delta for retry");
                    break;
                }
            }
        }
    }

    /// Rapporterar status, containerdata, spelare och nya konsolrader för
    /// varje server som mastern tilldelat noden. Varje rapport konsumerar
    /// ett nytt sekvensnummer (samma räknare som heartbeat/claim), annars
    /// avvisar mastern den som replay.
    async fn report_servers(&mut self) -> BTreeSet<String> {
        let Some(node_id) = self.store.state().node_id.clone() else {
            return self.assigned_servers.clone();
        };
        let listing = match self.api.servers(&node_id).await {
            Ok(listing) => listing,
            Err(err) => {
                warn!(error = %err, "fetching the assigned servers failed");
                return self.assigned_servers.clone();
            }
        };
        let Some(servers) = listing.get("servers").and_then(Value::as_array) else {
            return self.assigned_servers.clone();
        };
        let assigned = servers
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        self.assigned_servers = assigned.clone();
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
                    return assigned;
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
            if let Some(value) = sample.uptime_seconds {
                report.insert("uptime_seconds".into(), json!(value));
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
        assigned
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

/// Bara en nekad credential ska leda till en ny enrollment. Nätverksfel,
/// konflikter och masterns övriga fel får aldrig kasta bort en credential som
/// fortfarande fungerar.
fn needs_new_credential(err: &AgentError) -> bool {
    matches!(err, AgentError::Unauthorized)
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

/// Serverns katalog, kontrollerad mot data-roten (AGENTS §6): en symlink
/// eller en sökväg som lämnar roten vägras, och en katalog som redan är borta
/// är ett lyckat resultat — därför `None` i stället för ett fel.
fn deletable_server_data(root: &Path, server_id: &str) -> Result<Option<PathBuf>> {
    let candidate = root.join(server_id);
    match std::fs::symlink_metadata(&candidate) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(AgentError::Runtime(format!(
                "server data could not be inspected: {err}"
            )))
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AgentError::Runtime(
                "the server data directory is a symlink".into(),
            ))
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(AgentError::Runtime(
                "the server data path is not a directory".into(),
            ))
        }
        Ok(_) => {}
    }

    let canonical_root = std::fs::canonicalize(root)?;
    let resolved = std::fs::canonicalize(&candidate)?;
    if resolved == canonical_root || !resolved.starts_with(&canonical_root) {
        return Err(AgentError::Runtime(
            "the server data directory escaped the configured root".into(),
        ));
    }
    Ok(Some(resolved))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{deletable_server_data, needs_new_credential, sha256_hex, Agent};
    use crate::config::Config;
    use crate::error::AgentError;
    use crate::state::{Enrollment, StateStore};
    use crate::test_master::{FakeMaster, NEW_CLAIM_CODE, NEW_CREDENTIAL, NEW_REQUEST_ID};

    /// En nod som tagits bort i panelen svarar 401 på nästa heartbeat. Det ska
    /// leda till en ny enrollment — inte till att agenten dör med den sparade
    /// credentialen kvar på disk och startar om i samma 401-loop.
    #[test]
    fn only_a_rejected_credential_requires_a_new_enrollment() {
        assert!(needs_new_credential(&AgentError::Unauthorized));
        assert!(!needs_new_credential(&AgentError::Conflict(
            "replayed".to_string()
        )));
        assert!(!needs_new_credential(&AgentError::Rejected {
            status: 500,
            body: "boom".to_string()
        }));
    }

    /// En temporär state-katalog per test, så testerna aldrig delar
    /// credential eller identity.conf med varandra.
    fn temp_state_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-enroll-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Noden togs bort i panelen medan agenten var stoppad. Första starten får
    /// 401 på sin sparade credential och ska då själv fil:a en ny anmälan och
    /// skriva ut en ny claim-kod — ingen behöver trycka på "Installera /
    /// Starta" och inget gammalt ärende ligger kvar.
    #[tokio::test]
    async fn a_restart_after_the_node_was_deleted_files_a_new_claim_code() {
        let master = FakeMaster::start().await;
        let state_dir = temp_state_dir("restart-after-delete");
        {
            let mut store = StateStore::open(&state_dir).unwrap();
            store.state_mut().node_id = Some("3d2529cc-26bb-4601-bf21-364e4ce6ee0a".to_string());
            store.state_mut().credential = Some("revoked-credential".to_string());
            store.save().unwrap();
        }

        let mut agent = Agent::new(Config::for_test(master.url(), state_dir.clone())).unwrap();
        agent.start_session().await.unwrap();

        let requests = master.requests();
        assert!(requests
            .iter()
            .any(|request| request.method == "POST" && request.path.ends_with("/heartbeat")));
        assert!(requests
            .iter()
            .any(|request| request.method == "POST" && request.path == "/api/runtime/enrollments"));

        let store = StateStore::open(&state_dir).unwrap();
        assert_eq!(store.state().credential.as_deref(), Some(NEW_CREDENTIAL));
        assert!(store.state().enrollment.is_none());
        // Koden skrevs ut när ärendet fil:ades och togs bort när anslutningen
        // slutfördes: agenten är tillbaka utan att någon klickade på panelen.
        assert!(!state_dir.join("identity.conf").exists());
    }

    /// Ett sparat ärende som mastern inte längre känner igen (avvisat eller
    /// utgånget) svarar 401 på både claim-code och poll. Det får inte stoppa
    /// agenten: ärendet släpps och ett nytt fil:as, annars finns ingen kod att
    /// koppla med — och uppstarten dör i en loop.
    #[tokio::test]
    async fn a_stale_saved_enrollment_is_replaced_by_a_new_claim_code() {
        let master = FakeMaster::start_waiting().await;
        let state_dir = temp_state_dir("stale-enrollment");
        {
            let mut store = StateStore::open(&state_dir).unwrap();
            store.state_mut().enrollment = Some(Enrollment {
                request_id: "forgotten-request".to_string(),
                request_secret: "forgotten-secret".to_string(),
            });
            store.save().unwrap();
        }

        let mut agent = Agent::new(Config::for_test(master.url(), state_dir.clone())).unwrap();
        // Mastern väntar på en ägare, så agenten stannar i poll-loopen: det är
        // läget där den nya koden ska ligga i identity.conf utan att någon har
        // klickat i panelen.
        let waiting =
            tokio::time::timeout(std::time::Duration::from_secs(2), agent.self_enroll()).await;
        assert!(
            waiting.is_err(),
            "agenten ska vänta på en claim, inte avsluta"
        );

        let identity = std::fs::read_to_string(state_dir.join("identity.conf")).unwrap();
        assert!(identity.contains(NEW_CLAIM_CODE), "{identity}");
        let requests = master.requests();
        assert!(requests
            .iter()
            .any(|request| request.path.ends_with("/claim-code")));
        assert!(!requests.iter().any(|request| {
            request.path.contains("forgotten-request") && request.path.ends_with("/poll")
        }));
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path == "/api/runtime/enrollments")
                .count(),
            1
        );

        let store = StateStore::open(&state_dir).unwrap();
        assert!(store.state().credential.is_none());
        assert_eq!(
            store
                .state()
                .enrollment
                .as_ref()
                .map(|saved| saved.request_id.as_str()),
            Some(NEW_REQUEST_ID)
        );
    }

    /// `delete_server` får bara röra serverns egen katalog: en saknad katalog
    /// är ett lyckat resultat, en symlink avvisas, och inget utanför
    /// data-roten kan nås eftersom server-id:t är en validerad UUID.
    #[test]
    fn deleting_server_data_stays_inside_the_data_root() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!(
            "craftpilot-cancel-delete-{}-{unique}",
            std::process::id()
        ));
        let id = "096d46aa-4221-4e49-acd9-01624347f131";

        assert_eq!(deletable_server_data(&root, id).unwrap(), None);

        std::fs::create_dir_all(root.join(id)).unwrap();
        std::fs::write(root.join(id).join("server.jar"), b"jar").unwrap();
        let directory = deletable_server_data(&root, id).unwrap().unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert!(!root.join(id).exists());
        assert_eq!(deletable_server_data(&root, id).unwrap(), None);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, root.join(id)).unwrap();
            assert!(deletable_server_data(&root, id).is_err());
            std::fs::remove_file(root.join(id)).unwrap();
        }

        let _ = std::fs::remove_dir_all(&root);
    }

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
