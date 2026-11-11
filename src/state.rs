use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};

/// Samma innehåll som Python-agentens state-fil, minus fält som hör till
/// funktioner som inte är porterade ännu. `rcon_passwords` finns med redan
/// nu så formatet inte behöver migreras när konsolvägen kommer.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AgentState {
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub enrollment: Option<Enrollment>,
    #[serde(default)]
    pub heartbeat_sequence: u64,
    /// Reserved for the console path (RCON password per server). Kept in the
    /// format now so the state file does not need a migration later.
    #[allow(dead_code)]
    #[serde(default)]
    pub rcon_passwords: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enrollment {
    pub request_id: String,
    pub request_secret: String,
}

/// State-katalogen är en hemlighet: den innehåller nodens credential och
/// (senare) RCON-lösenord. Katalogen ska vara 0700 och filen 0600, och
/// skrivningen går via temp + rename så en krasch aldrig lämnar en halv fil.
pub struct StateStore {
    dir: PathBuf,
    state: AgentState,
}

impl StateStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        restrict_permissions(&dir, 0o700)?;

        let path = dir.join("state.json");
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<AgentState>(&bytes).map_err(|err| {
                AgentError::State(format!(
                    "{} is not valid agent state: {err}",
                    path.display()
                ))
            })?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => AgentState::default(),
            Err(err) => return Err(err.into()),
        };

        Ok(Self { dir, state })
    }

    pub fn state(&self) -> &AgentState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut AgentState {
        &mut self.state
    }

    /// Nästa sekvensnummer för heartbeat/claim. Mastern avvisar en
    /// återanvänd eller lägre sekvens, så numret sparas innan det skickas
    /// (en krasch får inte återanvända ett nummer).
    pub fn next_sequence(&mut self) -> Result<i64> {
        const MAX_SEQUENCE: u64 = i32::MAX as u64;
        if self.state.heartbeat_sequence >= MAX_SEQUENCE {
            return Err(AgentError::State(
                "heartbeat sequence is exhausted; re-enroll the node".into(),
            ));
        }
        self.state.heartbeat_sequence += 1;
        self.save()?;
        Ok(self.state.heartbeat_sequence as i64)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.dir.join("state.json");
        let temporary = self.dir.join("state.json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.state)?;

        {
            let mut file = fs::File::create(&temporary)?;
            restrict_permissions(&temporary, 0o600)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }

        fs::rename(&temporary, &path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

/// Windows/macOS i utvecklingsmiljö: filrättigheterna sätts av värden.
/// Agenten kör i produktion på Linux, där grenen ovan gäller.
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-agent-test-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = temporary_dir("roundtrip");
        let mut store = StateStore::open(&dir).unwrap();
        store.state_mut().node_id = Some("node-1".into());
        store.state_mut().credential = Some("credential".into());
        store.save().unwrap();

        let reloaded = StateStore::open(&dir).unwrap();
        assert_eq!(reloaded.state().node_id.as_deref(), Some("node-1"));
        assert_eq!(reloaded.state().credential.as_deref(), Some("credential"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sequence_increases_and_is_persisted_before_use() {
        let dir = temporary_dir("sequence");
        let mut store = StateStore::open(&dir).unwrap();
        assert_eq!(store.next_sequence().unwrap(), 1);
        assert_eq!(store.next_sequence().unwrap(), 2);

        let mut reloaded = StateStore::open(&dir).unwrap();
        assert_eq!(reloaded.next_sequence().unwrap(), 3);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temporary_dir("permissions");
        let store = StateStore::open(&dir).unwrap();
        store.save().unwrap();

        let mode = fs::metadata(dir.join("state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        fs::remove_dir_all(&dir).unwrap();
    }
}
