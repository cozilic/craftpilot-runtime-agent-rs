mod agent;
mod api;
mod config;
mod docker;
mod error;
mod files;
mod model;
mod snapshot;
mod state;

use tracing::error;
use tracing_subscriber::EnvFilter;

use crate::agent::Agent;
use crate::config::Config;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(err) = run().await {
        error!(error = %err, "agent stopped");
        std::process::exit(1);
    }
}

async fn run() -> error::Result<()> {
    let config = Config::from_env()?;
    Agent::new(config)?.run().await
}
