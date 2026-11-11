use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use crate::error::{AgentError, Result};
use crate::model::{
    AckRequest, ClaimResponse, EnrollmentCreated, EnrollmentPoll, HeartbeatResponse,
    RegisterResponse, StatusResponse,
};

/// Tunn klient mot masterns `/api/runtime/...`-endpoints. Inga redirects
/// följs (samma regel som Python-agenten), och credentialen skickas bara
/// som bearer när anropet är autentiserat.
pub struct MasterClient {
    http: Client,
    base: Url,
    credential: Option<String>,
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

        Ok(Self {
            http,
            base,
            credential: None,
        })
    }

    pub fn set_credential(&mut self, credential: Option<String>) {
        self.credential = credential;
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
            let credential = self.credential.as_deref().ok_or(AgentError::Unauthorized)?;
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
            status => Err(AgentError::Rejected {
                status: status.as_u16(),
                body,
            }),
        }
    }

    /// Själv-enrollment: agenten fil:ar ärendet och en sysadmin godkänner
    /// det i panelen. Inget credential krävs.
    pub async fn create_enrollment(
        &self,
        hostname: &str,
        agent_version: &str,
    ) -> Result<EnrollmentCreated> {
        let body = serde_json::json!({
            "hostname": hostname,
            "agent_version": agent_version,
        });
        self.request(Method::POST, "api/runtime/enrollments", Some(&body), false)
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
        let body = serde_json::json!({ "registration_token": registration_token });
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/register"),
            Some(&body),
            false,
        )
        .await
    }

    pub async fn heartbeat(&self, node_id: &str, sequence: i64) -> Result<HeartbeatResponse> {
        let body = serde_json::json!({ "sequence": sequence });
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
    pub async fn claim(&self, node_id: &str, sequence: i64) -> Result<ClaimResponse> {
        let body = serde_json::json!({ "sequence": sequence });
        self.request(
            Method::POST,
            &format!("api/runtime/nodes/{node_id}/commands/claim"),
            Some(&body),
            true,
        )
        .await
    }

    /// Hämtar en snapshot med nodens credential. Redirects följs aldrig,
    /// så en 302 kan inte flytta nedladdningen till en annan värd.
    pub async fn download(&self, url: &Url) -> Result<reqwest::Response> {
        let credential = self.credential.as_deref().ok_or(AgentError::Unauthorized)?;
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

    /// Hämtar en uppladdning som mastern redan verifierat. Leasen går i
    /// `X-Result-Lease`, inte i URL:en.
    pub async fn fetch_upload(
        &self,
        node_id: &str,
        command_id: &str,
        lease_token: &str,
    ) -> Result<reqwest::Response> {
        let credential = self.credential.as_deref().ok_or(AgentError::Unauthorized)?;
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
