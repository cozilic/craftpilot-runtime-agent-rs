use std::result::Result as StdResult;

/// Agentens fel. Varje variant motsvarar ett läge som huvudloopen måste
/// kunna skilja på: konfigurationsfel (starta inte), nekad credential
/// (fil:a ny enrollment) och lease-konflikt (logga och gå vidare).
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("config: {0}")]
    Config(String),

    #[error("state: {0}")]
    State(String),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),

    /// 401 från mastern: credentialen är återkallad, okänd eller tillhör
    /// en nod som tagits bort. Agenten ska sluta polla och fil:a en ny
    /// enrollment-request i stället för att retra för evigt.
    #[error("the master rejected the node credential")]
    Unauthorized,

    /// 409: lease har gått ut eller sekvensen är redan använd.
    #[error("the master rejected the request as a conflict: {0}")]
    Conflict(String),

    #[error("the master rejected the request: {status} {body}")]
    Rejected { status: u16, body: String },

    /// 403 `RUNTIME_NODE_ENROLLMENT_REJECTED`: en sysadmin avvisade anmälan.
    /// Det är inte samma sak som ett okänt ärende — agenten ska sluta fråga i
    /// stället för att fil:a ett nytt (inkorgen ska inte fyllas med
    /// dubbletter).
    #[error("the sysadmin rejected the enrollment request")]
    EnrollmentRejected,

    /// Ett kommando som mastern bad om men som agenten inte kunde utföra.
    /// Texten hamnar i ack:ens `message` (max 300 tecken).
    #[error("{0}")]
    Runtime(String),
}

pub type Result<T> = StdResult<T, AgentError>;
