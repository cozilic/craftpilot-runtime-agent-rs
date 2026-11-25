//! Wire types for the master's agent API.
//!
//! A few fields (`node_settings`, `lease_expires_at`, `master_url`, ...) are
//! not read yet. They stay declared so the contract is documented in one
//! place and phase 2 does not have to touch the serde structs.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct EnrollmentCreated {
    pub request_id: String,
    pub request_secret: String,
    /// Engångskod som ägaren anger i panelen (2026-11-12).
    #[serde(default)]
    pub claim_code: Option<String>,
    #[serde(default)]
    pub claim_expires_at: Option<String>,
    #[serde(default)]
    pub poll_interval_seconds: u64,
    #[serde(default)]
    pub expires_at: String,
}

impl EnrollmentCreated {
    pub fn into_enrollment(self) -> crate::state::Enrollment {
        crate::state::Enrollment {
            request_id: self.request_id,
            request_secret: self.request_secret,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ClaimCodeResponse {
    pub claim_code: String,
    #[serde(default)]
    pub claim_expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EnrollmentPoll {
    pub status: String,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub master_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterResponse {
    pub node_id: String,
    pub credential: String,
    #[serde(default)]
    pub heartbeat_interval_seconds: u64,
    #[serde(default)]
    pub master_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub heartbeat_interval_seconds: u64,
    #[serde(default)]
    pub agent_version: String,
}

#[derive(Debug, Deserialize)]
pub struct ClaimResponse {
    #[serde(default)]
    pub command: Option<Command>,
    #[serde(default)]
    pub node_settings: Option<NodeSettings>,
    /// Var noden ska prenumerera för att väckas när kön får ett kommando
    /// (2026-11-25). `None` betyder att push är avstängd på mastern — då
    /// pollar agenten som vanligt.
    #[serde(default)]
    pub realtime: Option<RealtimeConfig>,
}

/// Masterns push-instruktion: Reverbs `/app/<key>`-URL och nodens privata
/// kanal. Allt agenten behöver för att prenumerera; signeringen hämtas med
/// nodens credential.
#[derive(Debug, Clone, Deserialize)]
pub struct RealtimeConfig {
    pub url: String,
    pub channel: String,
}

#[derive(Debug, Deserialize)]
pub struct NodeSettings {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub max_servers: i64,
    #[serde(default)]
    pub poll_interval_seconds: u64,
}

/// Ett leaseat kommando. `type` är en sträng i stället för en enum: mastern
/// kan lägga till operationer, och en okänd typ ska ackas som RUNTIME_ERROR
/// i stället för att få JSON-parsningen att fallera.
#[derive(Debug, Clone, Deserialize)]
pub struct Command {
    pub id: String,
    #[serde(rename = "type")]
    pub command_type: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub lease_token: String,
    #[serde(default)]
    pub lease_expires_at: String,
    #[serde(default)]
    pub expires_at: String,
}

impl Command {
    pub fn server_id(&self) -> Option<&str> {
        self.payload
            .get("server_id")
            .and_then(|value| value.as_str())
    }
}

/// Det agenten frivilligt kan erbjuda noden just nu. `features` skickas med
/// varje heartbeat/claim (även tom) så att mastern kan sluta visa en
/// slutpunkt som inte längre fungerar. Värdnyckeln är publik information.
#[derive(Debug, Clone, Default)]
pub struct NodeCapabilities {
    pub features: Vec<String>,
    pub fingerprint: Option<String>,
    /// Värdporten nodens SFTPGo publicerades på. `None` när tjänsten inte
    /// svarar; då utelämnas fältet så masterns sparade värde står kvar.
    pub bound_port: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct AckRequest {
    pub lease_token: String,
    pub success: bool,
    pub result_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_parsed_from_the_master_shape() {
        let json = r#"{
            "command": {
                "id": "cmd-1",
                "type": "start",
                "payload": {"server_id": "srv-1", "startup_command": "java -jar server.jar"},
                "lease_token": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "lease_expires_at": "2026-11-11T21:00:00+00:00",
                "expires_at": "2026-11-11T21:05:00+00:00"
            },
            "node_settings": {"name": "node", "max_servers": 4, "poll_interval_seconds": 5}
        }"#;

        let claim: ClaimResponse = serde_json::from_str(json).unwrap();
        let command = claim.command.unwrap();
        assert_eq!(command.command_type, "start");
        assert_eq!(command.server_id(), Some("srv-1"));
        assert_eq!(claim.node_settings.unwrap().max_servers, 4);
    }

    #[test]
    fn an_unknown_command_type_still_parses() {
        let json = r#"{
            "id": "cmd-2",
            "type": "file_mutate",
            "payload": {},
            "lease_token": "b"
        }"#;

        let command: Command = serde_json::from_str(json).unwrap();
        assert_eq!(command.command_type, "file_mutate");
    }
}
