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
    /// Nodens namn i panelen. Mastern skickar det i claim-svaret
    /// (`node_settings.name`); agenten sparar det så desktop-panelen kan visa
    /// vilken nod den här agenten är — annars går flera agenter inte att
    /// skilja åt när en av dem är nere. Namnet är inte hemligt (det står i
    /// panelen), men det skrivs bara till den privata state-filen.
    #[serde(default)]
    pub node_name: Option<String>,
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub enrollment: Option<Enrollment>,
    /// Unix-tid (sekunder) för senaste avvisade anmälan. En avvisad nod får
    /// inte fil:a ett nytt ärende vid varje poll eller omstart — det fyller
    /// sysadminens inkorg med dubbletter. Fältet rensas när ett nytt ärende
    /// fil:as, och agenten väntar `enrollment_rejected_retry_seconds` innan
    /// den frågar igen.
    #[serde(default)]
    pub enrollment_rejected_at: Option<u64>,
    #[serde(default)]
    pub heartbeat_sequence: u64,
    /// Per-server RCON-lösenord (samma fält som Python-agenten). Skapas vid
    /// första starten och används av STOP; filen är 0600.
    #[serde(default)]
    pub rcon_passwords: BTreeMap<String, String>,
    /// Senaste loggtidsstämpeln som rapporterats per server. Sparas så en
    /// omstart fortsätter där agenten slutade i stället för att skicka om
    /// hela loggfönstret till panelen.
    #[serde(default)]
    pub log_cursors: BTreeMap<String, String>,
    /// Lösenordet för nodens lokala SFTPGo-admin. Genereras en gång och
    /// skrivs bara till den privata state-filen (0600) — det är agentens
    /// enda väg in i SFTPGo:s admin-API och aldrig en användarhemlighet.
    #[serde(default)]
    pub sftp_admin_password: Option<String>,
    /// Vad nodens SFTPGo faktiskt har per konto: masterns revision som
    /// applicerades och om kontot har ett lösenord. Bara metadata —
    /// användarlösenordet skrivs aldrig hit.
    #[serde(default)]
    pub sftp_passwords: BTreeMap<String, SftpPasswordState>,
}

/// Ett kontos lösenordsläge på noden. `revision` är masterns nummer för det
/// senaste agenten applicerade; `has_password` skiljer "mastern har aldrig
/// satt något" från "agenten rensade det nyss". Själva lösenordet lagras
/// aldrig här, så en omstart kan fortsätta utan att fråga mastern igen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpPasswordState {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub has_password: bool,
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

    pub fn log_cursor(&self, server_id: &str) -> Option<&str> {
        self.state.log_cursors.get(server_id).map(String::as_str)
    }

    pub fn sftp_passwords(&self) -> &BTreeMap<String, SftpPasswordState> {
        &self.state.sftp_passwords
    }

    /// Sparar kursorn direkt: en krasch efter rapporten får inte leda till
    /// att samma rader skickas igen vid nästa start.
    pub fn set_log_cursor(&mut self, server_id: &str, timestamp: &str) -> Result<()> {
        if self.log_cursor(server_id) == Some(timestamp) {
            return Ok(());
        }
        self.state
            .log_cursors
            .insert(server_id.to_string(), timestamp.to_string());
        self.save()
    }

    /// Skriver `identity.conf` med claim-koden (2026-11-12) så att ägaren
    /// kan läsa den igen utan att scrolla i terminalen. Filen är 0600 och
    /// innehåller inget credential — bara engångskoden och ärendets id.
    pub fn write_identity_file(
        &self,
        claim_code: &str,
        request_id: &str,
        expires_at: &str,
    ) -> Result<()> {
        let path = self.dir.join("identity.conf");
        let temporary = self.dir.join("identity.conf.tmp");
        let contents = format!(
            "# CraftPilot node identity — enter the code in the panel under My node.\n\
             # The code is single-use and expires; restart the agent for a new one.\n\
             CRAFTPILOT_CLAIM_CODE={claim_code}\n\
             CRAFTPILOT_REQUEST_ID={request_id}\n\
             CRAFTPILOT_CLAIM_EXPIRES_AT={expires_at}\n"
        );

        {
            let mut file = fs::File::create(&temporary)?;
            restrict_permissions(&temporary, 0o600)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
        }

        fs::rename(&temporary, &path)?;
        Ok(())
    }

    pub fn identity_file_path(&self) -> PathBuf {
        self.dir.join("identity.conf")
    }

    /// Tar bort identity.conf när noden är kopplad: en gammal kod ska inte
    /// ligga kvar och se giltig ut i en backup eller på en skärmbild.
    pub fn clear_identity_file(&self) -> Result<()> {
        match fs::remove_file(self.identity_file_path()) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
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
    fn node_name_round_trips_and_keeps_old_state_files_valid() {
        let dir = temporary_dir("node-name");
        let mut store = StateStore::open(&dir).unwrap();
        store.state_mut().node_name = Some("silksong".into());
        store.save().unwrap();

        let reloaded = StateStore::open(&dir).unwrap();
        assert_eq!(reloaded.state().node_name.as_deref(), Some("silksong"));

        // En state-fil från före fältet får inte bli oläsbar.
        let legacy = temporary_dir("legacy-node-name");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("state.json"),
            br#"{"node_id":"node-1","credential":"credential"}"#,
        )
        .unwrap();
        let opened = StateStore::open(&legacy).unwrap();
        assert_eq!(opened.state().node_name, None);

        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&legacy).unwrap();
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

    #[test]
    fn log_cursor_survives_a_restart() {
        let dir = temporary_dir("log-cursor");
        let mut store = StateStore::open(&dir).unwrap();
        store
            .set_log_cursor("server-1", "2026-11-11T22:00:00.000000001Z")
            .unwrap();
        drop(store);

        let mut reloaded = StateStore::open(&dir).unwrap();
        assert_eq!(
            reloaded.log_cursor("server-1"),
            Some("2026-11-11T22:00:00.000000001Z")
        );
        reloaded
            .set_log_cursor("server-1", "2026-11-11T22:00:01Z")
            .unwrap();
        assert_eq!(
            StateStore::open(&dir).unwrap().log_cursor("server-1"),
            Some("2026-11-11T22:00:01Z")
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn identity_file_holds_the_claim_code_and_stays_private() {
        let dir = temporary_dir("identity");
        let store = StateStore::open(&dir).unwrap();
        store
            .write_identity_file("CP-ABCDE-FGHJK", "request-1", "2026-11-12T12:00:00Z")
            .unwrap();

        let contents = fs::read_to_string(dir.join("identity.conf")).unwrap();
        assert!(contents.contains("CRAFTPILOT_CLAIM_CODE=CP-ABCDE-FGHJK"));
        assert!(contents.contains("CRAFTPILOT_REQUEST_ID=request-1"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("identity.conf"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        store.clear_identity_file().unwrap();
        assert!(!dir.join("identity.conf").exists());
        // Idempotent: en redan borta fil är inget fel.
        store.clear_identity_file().unwrap();

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
