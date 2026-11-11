use std::time::Duration;

use tokio::time::sleep;
use tracing::{info, warn};

use crate::api::MasterClient;
use crate::config::Config;
use crate::docker::Docker;
use crate::error::{AgentError, Result};
use crate::model::{AckRequest, Command};
use crate::snapshot::Snapshotter;
use crate::state::StateStore;

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
        Ok(Self {
            cfg,
            store,
            api,
            docker,
            snapshot,
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

    async fn self_enroll(&mut self) -> Result<()> {
        let mut enrollment = match self.store.state().enrollment.clone() {
            Some(saved) => saved,
            None => {
                let created = self
                    .api
                    .create_enrollment(&self.cfg.hostname, &self.cfg.agent_version)
                    .await?;
                info!(
                    request_id = %created.request_id,
                    expires_at = %created.expires_at,
                    "filed an enrollment request — approve it under Ops & log → Runtime nodes"
                );
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

        let Some(command) = claim.command else {
            return Ok(());
        };

        info!(
            command_id = %command.id,
            command_type = %command.command_type,
            "claimed a command"
        );

        let ack = self.execute(&command).await;
        match self.api.acknowledge(&node_id, &command.id, &ack).await {
            Ok(_) => Ok(()),
            Err(AgentError::Conflict(body)) => {
                warn!(command_id = %command.id, error = %body, "lease expired before the acknowledge");
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Kör START/STOP via Docker. Övriga operationer ackas ärligt som
    /// RUNTIME_ERROR så att masterns kö aldrig fastnar på en död lease.
    async fn execute(&self, command: &Command) -> AckRequest {
        let (success, result_code, message, container_id) = match command.command_type.as_str() {
            "start" => match self.docker.start(&command.payload).await {
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
            "stop" => match self.docker.stop(&command.payload).await {
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
