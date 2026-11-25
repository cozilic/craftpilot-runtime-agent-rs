use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use crate::build_info::AGENT_VERSION;
use crate::error::{AgentError, Result};
use crate::model::{
    AckRequest, ClaimResponse, EnrollmentCreated, EnrollmentPoll, HeartbeatResponse,
    NodeCapabilities, RegisterResponse, StatusResponse,
};
use crate::sftpgo::{SftpSecret, SftpSecrets, SftpState, SftpStatusReport};

/// Tunn klient mot masterns `/api/runtime/...`-endpoints. Inga redirects
/// följs (samma regel som Python-agenten), och credentialen skickas bara
/// som bearer när anropet är autentiserat.
/// Klonbar: push-loopen håller en egen kopia och läser alltid den SENASTE
/// credentialen. En omregistrering (som byter credential i samma process)
/// ska inte kräva att pushen startas om.
#[derive(Clone)]
pub struct MasterClient {
    http: Client,
    /// Nedladdningar strömmas: ingen total timeout, bara en anslutningstimeout.
    stream_http: Client,
    base: Url,
    credential: Arc<RwLock<Option<String>>>,
}

impl MasterClient {
    pub fn new(base: Url) -> Result<Self> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .user_agent(format!(
                "craftpilot-runtime-agent-rs/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .build()?;

        let stream_http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .user_agent(format!(
                "craftpilot-runtime-agent-rs/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .build()?;

        Ok(Self {
            http,
            stream_http,
            base,
            credential: Arc::new(RwLock::new(None)),
        })
    }

    pub fn set_credential(&mut self, credential: Option<String>) {
        if let Ok(mut guard) = self.credential.write() {
            *guard = credential;
        }
    }

    /// En kopia av credentialen. Låset hålls bara över klonen, aldrig över
    /// ett anrop.
    fn credential(&self) -> Result<String> {
        self.credential
            .read()
            .map_err(|_| AgentError::Config("the agent credential lock is poisoned".into()))?
            .clone()
            .ok_or(AgentError::Unauthorized)
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base
            .join(path)
            .map_err(|err| AgentError::Config(format!("invalid api path {path}: {err}")))
    }

    async fn request<T, B>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        authenticated: bool,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        let mut request = self
            .http
            .request(method, self.url(path)?)
            .header("Accept", "application/json");

        if authenticated {
            let credential = self.credential()?;
            request = request.bearer_auth(credential);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await?;
        let status = response.status();
        let body = response.text().await?;

        match status {
            StatusCode::UNAUTHORIZED => Err(AgentError::Unauthorized),
            StatusCode::CONFLICT => Err(AgentError::Conflict(body)),
            status if status.is_success() => {
                serde_json::from_str::<T>(&body).map_err(AgentError::from)
            }
            status => Err(request_error(status.as_u16(), body)),
        }
    }

    /// Som `request`, men för endpoints som svarar `204` utan kropp. Svaret
    /// töms ändå, så anslutningen återanvänds.
    async fn request_empty<B>(&self, method: Method, path: &str, body: Option<&B>) -> Result<()>
    where
        B: serde::Serialize + ?Sized,
    {
        let credential = self.credential()?;
        let mut request = self
            .http
            .request(method, self.url(path)?)
            .header("Accept", "application/json")
            .bearer_auth(credential);
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await?;
        let status = response.status();
        let body = response.text().await?;

        match status {
            StatusCode::UNAUTHORIZED => Err(AgentError::Unauthorized),
            StatusCode::CONFLICT => Err(AgentError::Conflict(body)),
            status if status.is_success() => Ok(()),
            status => Err(request_error(status.as_u16(), body)),
        }
    }

    /// Själv-enrollment: agenten fil:ar ärendet och en sysadmin godkänner
    /// det i panelen. Inget credential krävs.
    pub async fn create_enrollment(
        &self,
        hostname: &str,
        agent_version: &str,
        reported_address: Option<&str>,
    ) -> Result<EnrollmentCreated> {
        let mut body = serde_json::json!({
            "hostname": hostname,
            "agent_version": agent_version,
        });
        // Agentens egen adress (2026-11-16): panelen föreslår den i
        // claim-formuläret i stället för att gissa `host.docker.internal`.
        // Utelämnas helt när den inte kunde avgöras, så body:n är oförändrad
        // mot en master utan fältet.
        if let Some(address) = reported_address {
            body["reported_address"] = serde_json::Value::String(address.to_string());
        }

        self.request(Method::POST, "api/runtime/enrollments", Some(&body), false)
            .await
    }

    /// Ny claim-kod för en anmälan som fortfarande väntar. Används vid
    /// start med ett sparat ärende, så koden i terminalen alltid är giltig.
    pub async fn refresh_claim_code(
        &self,
        request_id: &str,
        request_secret: &str,
    ) -> Result<crate::model::ClaimCodeResponse> {
        let body = serde_json::json!({ "request_secret": request_secret });
        self.request(
            Method::POST,
            &format!("api/runtime/enrollments/{request_id}/claim-code"),
            Some(&body),
            false,
        )
        .await
    }

    pub async fn poll_enrollment(
        &self,
        request_id: &str,
        request_secret: &str,
    ) -> Result<EnrollmentPoll> {
        let body = serde_json::json!({ "request_secret": request_secret });
        self.request(
            Method::POST,
            &format!("api/runtime/enrollments/{request_id}/poll"),
            Some(&body),
            false,
        )
        .await
    }

    pub async fn complete_enrollment(
        &self,
        request_id: &str,
        request_secret: &str,
    ) -> Result<StatusResponse> {
        let body = serde_json::json!({ "request_secret": request_secret });
        self.request(
            Method::POST,
            &format!("api/runtime/enrollments/{request_id}/complete"),
            Some(&body),
            false,
        )
        .await
    }

    /// Endast för en nod som redan finns och har en engångstoken
    /// (`CRAFTPILOT_NODE_ID` + `CRAFTPILOT_REGISTRATION_TOKEN`).
    pub async fn register(
        &self,
        node_id: &str,
        registration_token: &str,
    ) -> Result<RegisterResponse> {
        let body = serde_json::json!({
            "registration_token": registration_token,
            "agent_version": AGENT_VERSION,
        });
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/register"),
            Some(&body),
            false,
        )
        .await
    }

    pub async fn heartbeat(
        &self,
        node_id: &str,
        sequence: i64,
        uptime_seconds: u64,
        capabilities: &NodeCapabilities,
    ) -> Result<HeartbeatResponse> {
        let mut body = serde_json::json!({
            "sequence": sequence,
            "uptime_seconds": uptime_seconds,
            // Versionen följer med varje poll, så en uppgraderad nod syns i
            // admin utan omregistrering.
            "agent_version": AGENT_VERSION,
        });
        insert_capabilities(&mut body, capabilities);
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/heartbeat"),
            Some(&body),
            true,
        )
        .await
    }

    /// Claim räknas också som heartbeat på mastern: samma sekvens räcker,
    /// men varje anrop måste ha ett NYTT nummer.
    pub async fn claim(
        &self,
        node_id: &str,
        sequence: i64,
        uptime_seconds: u64,
        capabilities: &NodeCapabilities,
    ) -> Result<ClaimResponse> {
        let mut body = serde_json::json!({
            "sequence": sequence,
            "uptime_seconds": uptime_seconds,
            "agent_version": AGENT_VERSION,
        });
        insert_capabilities(&mut body, capabilities);
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/commands/claim"),
            Some(&body),
            true,
        )
        .await
    }

    /// Signerar agentens prenumeration på sin egen nodkanal (2026-11-25).
    /// Mastern svarar `{auth}`; `404` betyder att pushen är avstängd och
    /// `401` att credentialen inte längre gäller. Båda stänger bara
    /// push-loopen — kommandoloopet pollar vidare.
    pub async fn broadcast_auth(
        &self,
        node_id: &str,
        channel: &str,
        socket_id: &str,
    ) -> Result<String> {
        let body = serde_json::json!({ "socket_id": socket_id, "channel_name": channel });
        let response: Value = self
            .request(
                Method::POST,
                &format!("api/runtime/nodes/{node_id}/broadcasting/auth"),
                Some(&body),
                true,
            )
            .await?;

        response
            .get("auth")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AgentError::Config("the master returned no push signature".into()))
    }

    /// Hämtar en snapshot med nodens credential. Redirects följs aldrig,
    /// så en 302 kan inte flytta nedladdningen till en annan värd.
    pub async fn download(&self, url: &Url) -> Result<reqwest::Response> {
        let credential = self.credential()?;
        let response = self
            .http
            .get(url.clone())
            .header("Accept", "application/zip")
            .bearer_auth(credential)
            .send()
            .await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(AgentError::Unauthorized);
        }
        if !status.is_success() {
            return Err(AgentError::Rejected {
                status: status.as_u16(),
                body: String::new(),
            });
        }
        Ok(response)
    }

    /// Masterns önskade SFTP-läge: hela listan med konton, nycklar och
    /// servermappar för den här noden. Lösenord ingår aldrig.
    pub async fn sftp_state(&self, node_id: &str) -> Result<SftpState> {
        self.request(
            Method::GET,
            &format!("api/runtime/nodes/{node_id}/sftp"),
            None::<&Value>,
            true,
        )
        .await
    }

    /// Engångshemligheterna noden ännu inte kvitterat. Tomt är normalfallet;
    /// endpointen har budgeten 10/min, så agenten frågar bara när en revision
    /// är nyare än nodens egen och aldrig två gånger för samma revision.
    pub async fn sftp_secrets(&self, node_id: &str) -> Result<Vec<SftpSecret>> {
        let secrets: SftpSecrets = self
            .request(
                Method::POST,
                &format!("api/runtime/nodes/{node_id}/sftp/secrets"),
                None::<&Value>,
                true,
            )
            .await?;
        Ok(secrets.secrets)
    }

    /// Kvitterar vilken lösenordsrevision nodens SFTPGo har. Mastern svarar
    /// `204`; en `422` betyder att ett användarnamn inte ingår i nodens state
    /// och ska inte stoppa SFTP-synken.
    pub async fn report_sftp_status(
        &self,
        node_id: &str,
        users: &[SftpStatusReport],
    ) -> Result<()> {
        let body = serde_json::json!({ "users": users });
        self.request_empty(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/sftp/status"),
            Some(&body),
        )
        .await
    }

    /// Servrarna som är tilldelade den här noden (`servers` + `artifacts`).
    pub async fn servers(&self, node_id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("api/runtime/nodes/{node_id}/servers"),
            None::<&Value>,
            true,
        )
        .await
    }

    /// Skickar ett batchat katalogdelta till masterns filcache. Mastern
    /// validerar att servern är tilldelad noden och att listningen matchar
    /// dess `path`/`parent`-kontrakt.
    pub async fn sync_file_tree(&self, node_id: &str, directories: &Value) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/file-tree"),
            Some(directories),
            true,
        )
        .await
    }

    /// Rapporterar `fetching`, `building`, `indexing` eller `ready` till
    /// panelens filtree-progress.
    pub async fn report_file_tree_progress(&self, node_id: &str, stage: &str) -> Result<Value> {
        let body = serde_json::json!({ "stage": stage });
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/file-tree/progress"),
            Some(&body),
            true,
        )
        .await
    }

    /// Nya konsolrader direkt till mastern (2026-11-25): samma rader som
    /// rapporten bär, men skickade sekunden de dyker upp. Mastern broadcastar
    /// dem till panelen utan att röra rapportens mått.
    pub async fn report_console_lines(
        &self,
        node_id: &str,
        server_id: &str,
        report: &Value,
    ) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/servers/{server_id}/logs"),
            Some(report),
            true,
        )
        .await
    }

    /// En statusrapport per server; `sequence` måste vara strikt ökande.
    pub async fn report_server(&self, node_id: &str, report: &Value) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/servers/report"),
            Some(report),
            true,
        )
        .await
    }

    /// File results are delivered before the ack: the master's ack checks
    /// `result_data`, which this endpoint stores.
    pub async fn store_result(
        &self,
        node_id: &str,
        command_id: &str,
        lease_token: &str,
        result: &Value,
    ) -> Result<Value> {
        let body = serde_json::json!({ "lease_token": lease_token, "result": result });
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/commands/{command_id}/result"),
            Some(&body),
            true,
        )
        .await
    }

    /// Strömmar en fil till masterns `/result`-endpoint. Leasen går i
    /// `X-Result-Lease` och `Content-Length` måste vara exakt — mastern
    /// läser råbody, inte JSON.
    pub async fn upload_file_result(
        &self,
        node_id: &str,
        command_id: &str,
        lease_token: &str,
        path: &std::path::Path,
        bytes: u64,
    ) -> Result<Value> {
        let credential = self.credential()?;
        let file = tokio::fs::File::open(path).await?;
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
        let response = self
            .stream_http
            .put(self.url(&format!(
                "api/runtime/nodes/{node_id}/commands/{command_id}/result"
            ))?)
            .header("X-Result-Lease", lease_token)
            .header("Content-Length", bytes.to_string())
            .header("Content-Type", "application/octet-stream")
            .bearer_auth(credential)
            .body(body)
            .send()
            .await?;
        self.migration_response(response).await
    }

    /// Hämtar en uppladdning som mastern redan verifierat. Leasen går i
    /// `X-Result-Lease`, inte i URL:en.
    pub async fn fetch_upload(
        &self,
        node_id: &str,
        command_id: &str,
        lease_token: &str,
    ) -> Result<reqwest::Response> {
        let credential = self.credential()?;
        let response = self
            .http
            .get(self.url(&format!(
                "api/runtime/nodes/{node_id}/commands/{command_id}/upload"
            ))?)
            .header("X-Result-Lease", lease_token)
            .bearer_auth(credential)
            .send()
            .await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(AgentError::Unauthorized);
        }
        if !status.is_success() {
            return Err(AgentError::Rejected {
                status: status.as_u16(),
                body: String::new(),
            });
        }
        Ok(response)
    }

    /// Hämtar serverns artefakt (pumpkin-binär eller jar) som mastern stageat
    /// och verifierat. Samma lease-header som uppladdningen: leasen, inte
    /// URL:en, avgör vem som får läsa blobben.
    pub async fn fetch_artifact(
        &self,
        node_id: &str,
        command_id: &str,
        lease_token: &str,
    ) -> Result<reqwest::Response> {
        let credential = self.credential()?;
        let response = self
            .http
            .get(self.url(&format!(
                "api/runtime/nodes/{node_id}/commands/{command_id}/artifact"
            ))?)
            .header("X-Result-Lease", lease_token)
            .bearer_auth(credential)
            .send()
            .await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(AgentError::Unauthorized);
        }
        if !status.is_success() {
            return Err(AgentError::Rejected {
                status: status.as_u16(),
                body: String::new(),
            });
        }
        Ok(response)
    }

    /// Migration relay: the lease identifies the command, the body is the
    /// chunk ciphertext. Bodies stay under the master's 1 MiB + 16 limit.
    pub async fn put_migration_manifest(
        &self,
        node_id: &str,
        migration_id: &str,
        command_id: &str,
        lease_token: &str,
        manifest: &Value,
    ) -> Result<Value> {
        let credential = self.credential()?;
        let response = self
            .http
            .put(self.url(&format!(
                "api/runtime/nodes/{node_id}/migrations/{migration_id}/manifest"
            ))?)
            .header("X-Migration-Command", command_id)
            .header("X-Migration-Lease", lease_token)
            .bearer_auth(credential)
            .json(manifest)
            .send()
            .await?;
        self.migration_response(response).await
    }

    pub async fn put_migration_chunk(
        &self,
        node_id: &str,
        migration_id: &str,
        command_id: &str,
        lease_token: &str,
        index: u64,
        chunk: &[u8],
    ) -> Result<Value> {
        let credential = self.credential()?;
        let response = self
            .http
            .put(self.url(&format!(
                "api/runtime/nodes/{node_id}/migrations/{migration_id}/chunks/{index}"
            ))?)
            .header("X-Migration-Command", command_id)
            .header("X-Migration-Lease", lease_token)
            .header("Content-Type", "application/octet-stream")
            .bearer_auth(credential)
            .body(chunk.to_vec())
            .send()
            .await?;
        self.migration_response(response).await
    }

    pub async fn get_migration_manifest(
        &self,
        node_id: &str,
        migration_id: &str,
        command_id: &str,
        lease_token: &str,
    ) -> Result<Value> {
        let credential = self.credential()?;
        let response = self
            .http
            .get(self.url(&format!(
                "api/runtime/nodes/{node_id}/migrations/{migration_id}/manifest"
            ))?)
            .header("X-Migration-Command", command_id)
            .header("X-Migration-Lease", lease_token)
            .bearer_auth(credential)
            .send()
            .await?;
        self.migration_response(response).await
    }

    pub async fn get_migration_chunk(
        &self,
        node_id: &str,
        migration_id: &str,
        command_id: &str,
        lease_token: &str,
        index: u64,
    ) -> Result<Vec<u8>> {
        let credential = self.credential()?;
        let response = self
            .http
            .get(self.url(&format!(
                "api/runtime/nodes/{node_id}/migrations/{migration_id}/chunks/{index}"
            ))?)
            .header("X-Migration-Command", command_id)
            .header("X-Migration-Lease", lease_token)
            .bearer_auth(credential)
            .send()
            .await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(AgentError::Unauthorized);
        }
        if !status.is_success() {
            return Err(AgentError::Rejected {
                status: status.as_u16(),
                body: String::new(),
            });
        }
        Ok(response.bytes().await?.to_vec())
    }

    async fn migration_response(&self, response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let body = response.text().await?;
        match status {
            StatusCode::UNAUTHORIZED => Err(AgentError::Unauthorized),
            StatusCode::CONFLICT => Err(AgentError::Conflict(body)),
            status if status.is_success() => {
                serde_json::from_str::<Value>(&body).map_err(AgentError::from)
            }
            status => Err(AgentError::Rejected {
                status: status.as_u16(),
                body,
            }),
        }
    }

    pub async fn acknowledge(
        &self,
        node_id: &str,
        command_id: &str,
        ack: &AckRequest,
    ) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/commands/{command_id}/ack"),
            Some(ack),
            true,
        )
        .await
    }
}

/// Förmågorna följer med varje poll. `features` skickas alltid (även tom),
/// så en nod som tappar sin SFTPGo slutar lova en slutpunkt. Ett utelämnat
/// fingeravtryck behåller masterns sparade värde, och det gör en utelämnad
/// port också.
fn insert_capabilities(body: &mut Value, capabilities: &NodeCapabilities) {
    body["features"] = serde_json::json!(capabilities.features);
    if let Some(fingerprint) = capabilities.fingerprint.as_deref() {
        body["sftp_host_key_fingerprint"] = serde_json::json!(fingerprint);
    }
    // Porten agenten faktiskt publicerade: mastern använder den när
    // operatören inte satt en egen `sftp_port`, så panelen och mappen pekar
    // på en port som lyssnar även när defaultporten var upptagen på värden.
    if let Some(port) = capabilities.bound_port {
        body["sftp_bound_port"] = serde_json::json!(port);
    }
}

/// Masterns felkod för en anmälan som en sysadmin har avvisat. Den ska
/// agenten inte svara på med ett nytt ärende.
pub const ENROLLMENT_REJECTED_CODE: &str = "RUNTIME_NODE_ENROLLMENT_REJECTED";

/// Ett avvisat ärende är inte ett okänt ärende: 403 med masterns kod blir
/// `EnrollmentRejected`, allt annat behåller status och body.
fn request_error(status: u16, body: String) -> AgentError {
    if status == 403 && body.contains(ENROLLMENT_REJECTED_CODE) {
        return AgentError::EnrollmentRejected;
    }

    AgentError::Rejected { status, body }
}
