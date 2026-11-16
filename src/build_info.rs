//! Byggversionen som agenten rapporterar till mastern.
//!
//! `build.rs` läser `CRAFTPILOT_AGENT_VERSION` (satt av CI/Docker-bygget) eller
//! `git describe --tags --always --dirty` och skriver in resultatet här.
//! Källkörning utan git-historik rapporterar `dev` — aldrig Cargo-paketets
//! platshållarversion, som inte säger något om vilken binär noden kör.

pub const AGENT_VERSION: &str = env!("CRAFTPILOT_BUILD_VERSION");

#[cfg(test)]
mod tests {
    use super::AGENT_VERSION;

    /// Mastern validerar `agent_version` med `^[A-Za-z0-9][A-Za-z0-9._+-]*$`
    /// och max 40 tecken (`RegisterRuntimeNodeRequest`/heartbeat-requesten).
    #[test]
    fn the_build_version_fits_the_master_contract() {
        assert_ne!(AGENT_VERSION, "local-docker");
        assert!(AGENT_VERSION.len() <= 40, "too long: {AGENT_VERSION}");
        assert!(
            AGENT_VERSION
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric()),
            "must start with a letter or digit: {AGENT_VERSION}"
        );
        assert!(
            AGENT_VERSION
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c)),
            "invalid characters for the master: {AGENT_VERSION}"
        );
    }
}
