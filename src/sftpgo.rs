//! SFTPGo:s admin-API och översättningen från masterns state till lokala
//! konton och mappar (docs/RUNTIME-NODE-SFTP.md i CraftPilot-repot).
//!
//! Mastern äger behörigheten: den skickar en fullständig lista med
//! `username`, publika nycklar och `folders[] = {name, server_id}`. Agenten
//! får aldrig hitta på ett konto eller en mapp själv — den bygger bara
//! sökvägen ur `server_id` och vägrar allt som inte ligger under data-roten.
//!
//! Inga lösenord lagras här: lösenordskanalen (`sftp-password`) levererar en
//! klartext som bara finns i minnet under en apply-omgång. Ett konto utan
//! minst en publik nyckel skapas aldrig utan ett applicerat lösenord, eftersom
//! SFTPGo annars skulle få ett konto helt utan autentiseringsmetod.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::{Client, Method, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;
use url::Url;

use crate::error::{AgentError, Result};

/// Var SFTPGo-containern ser nodens serverkataloger. Samma mount i alla
/// noder, så masterns `server_id` räcker för att bygga sökvägen.
pub const CONTAINER_SERVERS_ROOT: &str = "/srv/craftpilot/servers";

/// Admin-token lever i 15 minuter som default; förnya i god tid.
const TOKEN_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_SUMMARY: usize = 200;
/// Fallback bara när varken ett override eller katalogens ägare kan läsas
/// (t.ex. en utvecklingsnod utan unix-ägare).
const DEFAULT_FILE_UID: u32 = 33;
const DEFAULT_FILE_GID: u32 = 33;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SftpFolder {
    pub name: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SftpUser {
    pub username: String,
    #[serde(default)]
    pub public_keys: Vec<String>,
    /// Masterns sanning om lösenordet. En revision som är nyare än nodens
    /// egen gör att agenten hämtar hemligheten en gång och applicerar den.
    #[serde(default)]
    pub password_set: bool,
    #[serde(default)]
    pub password_revision: u64,
    #[serde(default)]
    pub folders: Vec<SftpFolder>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SftpState {
    pub revision: String,
    #[serde(default)]
    pub users: Vec<SftpUser>,
}

/// En engångshemlighet från `POST …/sftp/secrets`. Klartexten får bara leva i
/// minnet mellan hämtningen och det PUT/POST som applicerar den.
#[derive(Debug, Clone, Deserialize)]
pub struct SftpSecret {
    pub username: String,
    pub password: String,
    pub revision: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct SftpSecrets {
    #[serde(default)]
    pub secrets: Vec<SftpSecret>,
}

/// Vad en apply-omgång ska göra med ett kontos lösenord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordIntent {
    /// Sätt det här lösenordet (klartext, endast i minnet).
    Set(String),
    /// Kontot har redan lösenordet; `password` utelämnas så hashen består.
    Keep,
    /// Ta bort lösenordet (`password: ""`), så kontot blir nyckelbaserat.
    Clear,
}

/// En kvittenspost till `POST …/sftp/status`: vilken revision nodens SFTPGo
/// faktiskt har (0 = inget lösenord). Aldrig lösenordet.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SftpStatusReport {
    pub username: String,
    pub password_revision: u64,
}

/// Konfiguration för en apply-omgång. Hålls utanför `Config` så testerna
/// kan peka på en temporär data-rot.
pub struct ApplyConfig<'a> {
    pub data_root: &'a Path,
    /// `None` = använd serverkatalogens ägare.
    pub file_uid: Option<u32>,
    pub file_gid: Option<u32>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub users: usize,
    pub folders: usize,
    /// Servrar vars katalog saknas (eller inte är en riktig katalog under
    /// data-roten) och därför inte mappades.
    pub skipped_folders: usize,
    /// Konton där en lösenordsavsikt faktiskt skrevs (alltså inte `Keep`).
    pub password_users: BTreeSet<String>,
}

/// Klient mot SFTPGo:s admin-API. Alla anrop går över det interna
/// Docker-nätet; admin-porten publiceras aldrig.
pub struct SftpGoClient {
    http: Client,
    base: Url,
    username: String,
    password: String,
    token: Option<(String, Instant)>,
}

impl SftpGoClient {
    pub fn new(base: Url, username: String, password: String) -> Result<Self> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .user_agent("craftpilot-runtime-agent-rs/sftpgo")
            .build()?;

        Ok(Self {
            http,
            base,
            username,
            password,
            token: None,
        })
    }

    /// SFTPGo:s tjänstestatus. Används både som readiness-koll och för
    /// värdnyckeln: `ssh.host_keys[].fingerprint` är redan `SHA256:…`.
    pub async fn status(&mut self) -> Result<Value> {
        let (status, value) = self.send(Method::GET, "api/v2/status", None).await?;
        expect_success(status, "status", &value)?;
        Ok(value)
    }

    /// Skapar/uppdaterar mappar och konton så att SFTPGo speglar state.
    /// Idempotent: andra körningen med samma revision skriver ingenting.
    /// Bara testerna anropar den utan lösenordsavsikter; produktionsvägen går
    /// alltid via `apply_with_passwords`.
    #[cfg(test)]
    pub async fn apply(
        &mut self,
        state: &SftpState,
        cfg: &ApplyConfig<'_>,
    ) -> Result<ApplyOutcome> {
        self.apply_with_passwords(state, cfg, &BTreeMap::new())
            .await
    }

    /// Samma synk, men med de lösenordsavsikter noden räknat ut. Klartexten
    /// finns bara i `passwords` under anropet: den skrivs aldrig till state,
    /// logg eller ett svar tillbaka till mastern.
    pub async fn apply_with_passwords(
        &mut self,
        state: &SftpState,
        cfg: &ApplyConfig<'_>,
        passwords: &BTreeMap<String, PasswordIntent>,
    ) -> Result<ApplyOutcome> {
        // 1. Vilka mappar vill mastern ha? En server vars katalog saknas
        //    mappas aldrig — annars skulle SFTPGo peka på en tom sökväg.
        let mut folders: BTreeMap<String, String> = BTreeMap::new();
        let mut skipped_folders = 0usize;
        for user in &state.users {
            if !managed_username(&user.username) {
                continue;
            }
            for folder in &user.folders {
                match server_mount(folder, cfg.data_root) {
                    Some(mount) => {
                        folders.insert(mount.name, mount.mapped);
                    }
                    None => skipped_folders += 1,
                }
            }
        }

        // 2. Mappar före konton (samma ordning som masterns SFTP-synk).
        let existing_folders = self.list_folders().await?;
        for (name, mapped) in &folders {
            match existing_folders.get(name) {
                Some(current) if current == mapped => {}
                Some(_) => self.update_folder(name, mapped).await?,
                None => self.create_folder(name, mapped).await?,
            }
        }

        // 3. Konton. Listan ger bara namnen; varje konto läses separat så
        //    jämförelsen ser nycklar och virtual folders som de faktiskt är.
        let existing_users = self.list_usernames().await?;
        let mut desired_users = BTreeSet::new();
        let mut password_users = BTreeSet::new();

        for user in &state.users {
            if !managed_username(&user.username) {
                continue;
            }

            let mut pairs: Vec<(String, String)> = Vec::new();
            let mut derived_owner: Option<(u32, u32)> = None;
            let mut differing_owners = 0usize;
            for folder in &user.folders {
                let Some(mount) = server_mount(folder, cfg.data_root) else {
                    continue;
                };
                if let Some(owner) = directory_owner(&mount.resolved) {
                    match derived_owner {
                        None => derived_owner = Some(owner),
                        Some(first) if first != owner => differing_owners += 1,
                        _ => {}
                    }
                }
                pairs.push((mount.name, mount.mapped));
            }
            if differing_owners > 0 {
                // SFTPGo har en uid/gid per konto, inte per mapp. Den första
                // serverns ägare används (deterministiskt), och antalet
                // avvikande servrar loggas så en blandad nod syns.
                warn!(
                    servers = differing_owners + 1,
                    "sftp servers have different owners; using the first one"
                );
            }
            let (file_uid, file_gid) = effective_owner(cfg.file_uid, cfg.file_gid, derived_owner);

            let intent = passwords.get(&user.username);
            // Lösenordet räknas som autentiseringsmetod: ett konto med bara
            // lösenord får finnas, ett konto utan båda får inte.
            let has_password = !matches!(intent, None | Some(PasswordIntent::Clear));
            let Some(mut payload) = desired_user(
                &user.username,
                &user.public_keys,
                &pairs,
                file_uid,
                file_gid,
                has_password,
            ) else {
                // Varken giltig nyckel eller lösenord (eller inga mappar):
                // inget konto. Ett befintligt konto tas bort i städningen.
                // Ett rensat lösenord är ändå applicerat när kontot försvinner.
                if matches!(intent, Some(PasswordIntent::Clear)) {
                    password_users.insert(user.username.clone());
                }
                continue;
            };
            let password_changed = match intent {
                Some(PasswordIntent::Set(password)) => {
                    payload["password"] = json!(password);
                    true
                }
                Some(PasswordIntent::Clear) => {
                    // Tom sträng rensar hashen i SFTPGo.
                    payload["password"] = json!("");
                    true
                }
                _ => false,
            };

            desired_users.insert(user.username.clone());
            if password_changed {
                password_users.insert(user.username.clone());
            }
            match self.get_user(&user.username).await? {
                // En lösenordsändring måste skrivas även när kontot annars är
                // identiskt, och en `Keep` får aldrig skicka `password`.
                Some(current) if !password_changed && user_matches(&current, &payload) => {}
                Some(current) => {
                    // SFTPGo behåller en öppen sessions inställningar, så en ny
                    // mapp kräver att sessionerna stängs. Nycklar och ägare får
                    // däremot inte kasta ut en pågående överföring.
                    let disconnect = folders_changed(&current, &payload);
                    self.update_user(&user.username, &payload, disconnect)
                        .await?
                }
                None => self.create_user(&payload).await?,
            }
        }

        // 4. Städa bort allt som inte längre finns i state. Bara konton som
        //    ser ut som agentens egna (`u<id>`) rörs — admin-kontot lämnas.
        for username in existing_users {
            if managed_username(&username) && !desired_users.contains(&username) {
                self.delete_user(&username).await?;
            }
        }
        for name in existing_folders.keys() {
            if !folders.contains_key(name) {
                // En mapp som fortfarande används kan inte tas bort; det är
                // ofarligt och blir rätt vid nästa poll.
                let _ = self.delete_folder(name).await;
            }
        }

        Ok(ApplyOutcome {
            users: desired_users.len(),
            folders: folders.len(),
            skipped_folders,
            password_users,
        })
    }

    async fn list_usernames(&mut self) -> Result<BTreeSet<String>> {
        let (status, value) = self
            .send(Method::GET, "api/v2/users?limit=500", None)
            .await?;
        expect_success(status, "users", &value)?;
        let mut names = BTreeSet::new();
        if let Some(users) = value.as_array() {
            for user in users {
                if let Some(name) = user.get("username").and_then(Value::as_str) {
                    names.insert(name.to_string());
                }
            }
        }
        Ok(names)
    }

    async fn list_folders(&mut self) -> Result<BTreeMap<String, String>> {
        let (status, value) = self
            .send(Method::GET, "api/v2/folders?limit=500", None)
            .await?;
        expect_success(status, "folders", &value)?;
        let mut folders = BTreeMap::new();
        if let Some(entries) = value.as_array() {
            for folder in entries {
                let name = folder.get("name").and_then(Value::as_str);
                let mapped = folder.get("mapped_path").and_then(Value::as_str);
                if let (Some(name), Some(mapped)) = (name, mapped) {
                    folders.insert(name.to_string(), mapped.to_string());
                }
            }
        }
        Ok(folders)
    }

    async fn get_user(&mut self, username: &str) -> Result<Option<Value>> {
        let (status, value) = self
            .send(
                Method::GET,
                &format!("api/v2/users/{}", path_segment(username)?),
                None,
            )
            .await?;
        match status {
            200 => Ok(Some(value)),
            404 => Ok(None),
            _ => Err(request_error(status, "user", &value)),
        }
    }

    async fn create_user(&mut self, payload: &Value) -> Result<()> {
        let (status, value) = self
            .send(Method::POST, "api/v2/users", Some(payload))
            .await?;
        expect_success(status, "create user", &value)
    }

    async fn update_user(
        &mut self,
        username: &str,
        payload: &Value,
        disconnect: bool,
    ) -> Result<()> {
        let (status, value) = self
            .send(
                Method::PUT,
                &user_path(username, disconnect)?,
                Some(payload),
            )
            .await?;
        expect_success(status, "update user", &value)
    }

    async fn delete_user(&mut self, username: &str) -> Result<()> {
        let (status, value) = self
            .send(
                Method::DELETE,
                &format!("api/v2/users/{}", path_segment(username)?),
                None,
            )
            .await?;
        match status {
            200 | 204 | 404 => Ok(()),
            _ => Err(request_error(status, "delete user", &value)),
        }
    }

    async fn create_folder(&mut self, name: &str, mapped_path: &str) -> Result<()> {
        let payload = json!({ "name": name, "mapped_path": mapped_path });
        let (status, value) = self
            .send(Method::POST, "api/v2/folders", Some(&payload))
            .await?;
        expect_success(status, "create folder", &value)
    }

    async fn update_folder(&mut self, name: &str, mapped_path: &str) -> Result<()> {
        let payload = json!({ "name": name, "mapped_path": mapped_path });
        let (status, value) = self
            .send(
                Method::PUT,
                &format!("api/v2/folders/{}", path_segment(name)?),
                Some(&payload),
            )
            .await?;
        expect_success(status, "update folder", &value)
    }

    async fn delete_folder(&mut self, name: &str) -> Result<()> {
        let (status, value) = self
            .send(
                Method::DELETE,
                &format!("api/v2/folders/{}", path_segment(name)?),
                None,
            )
            .await?;
        match status {
            200 | 204 | 404 => Ok(()),
            _ => Err(request_error(status, "delete folder", &value)),
        }
    }

    async fn send(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let mut refreshed = false;
        loop {
            let token = self.access_token().await?;
            let url = self
                .base
                .join(path)
                .map_err(|err| AgentError::Config(format!("invalid sftpgo path {path}: {err}")))?;
            let mut request = self
                .http
                .request(method.clone(), url)
                .bearer_auth(&token)
                .header("Accept", "application/json");
            if let Some(body) = body {
                request = request.json(body);
            }

            let response = request.send().await?;
            let status = response.status();
            let text = response.text().await?;

            // Admin-token kan ha gått ut eller SFTPGo startats om: förnya
            // en gång i stället för att låta hela synken fallera.
            if status == StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                self.token = None;
                continue;
            }

            let value = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap_or(Value::String(text))
            };
            return Ok((status.as_u16(), value));
        }
    }

    async fn access_token(&mut self) -> Result<String> {
        if let Some((token, issued_at)) = &self.token {
            if issued_at.elapsed() < TOKEN_TTL {
                return Ok(token.clone());
            }
        }

        let url = self
            .base
            .join("api/v2/token")
            .map_err(|err| AgentError::Config(format!("invalid sftpgo token path: {err}")))?;
        let response = self
            .http
            .get(url)
            .basic_auth(&self.username, Some(&self.password))
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;

        if !status.is_success() {
            return Err(AgentError::Runtime(format!(
                "sftpgo token request failed: {} {}",
                status.as_u16(),
                summarize(&text)
            )));
        }

        let value: Value = serde_json::from_str(&text)?;
        let token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AgentError::Runtime("sftpgo returned no admin access token".into()))?
            .to_string();

        self.token = Some((token.clone(), Instant::now()));
        Ok(token)
    }
}

fn expect_success(status: u16, operation: &str, value: &Value) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(request_error(status, operation, value))
}

fn request_error(status: u16, operation: &str, value: &Value) -> AgentError {
    AgentError::Runtime(format!(
        "sftpgo {operation} failed: {status} {}",
        summarize(&serde_json::to_string(value).unwrap_or_default())
    ))
}

/// Ett svar kan innehålla en valideringsorsak; det är användbart i loggen
/// men aldrig en hemlighet. Nycklar och lösenord skrivs aldrig ut.
fn summarize(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    clean.trim().chars().take(MAX_SUMMARY).collect()
}

/// Bara `<username>` som redan är `u<id>` får bli en sökväg. Mastern
/// genererar dem, men klienten litar inte på det.
pub(crate) fn managed_username(username: &str) -> bool {
    let Some(rest) = username.strip_prefix('u') else {
        return false;
    };
    !rest.is_empty() && rest.len() <= 20 && rest.chars().all(|ch| ch.is_ascii_digit())
}

/// Mappnamnet blir både SFTPGo-folder och virtual path (`/<slug>`), så det
/// måste vara en säker, enkel komponent — aldrig `..`, `/` eller tomt.
pub fn valid_slug(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || name.starts_with('.') {
        return false;
    }
    name.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        && !name.contains("..")
}

fn valid_public_key(key: &str) -> bool {
    let key = key.trim();
    if key.is_empty() || key.len() > 16 * 1024 || key.lines().count() != 1 {
        return false;
    }
    ["ssh-", "ecdsa-", "sk-ssh-", "sk-ecdsa-"]
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

/// `u42` → `{status, username, home_dir, public_keys, permissions,
/// virtual_folders, uid, gid}`. `None` när kontot inte får finnas: ett konto
/// kräver minst en autentiseringsmetod, alltså en giltig nyckel eller ett
/// lösenord som redan är (eller strax blir) applicerat. Lösenordet läggs
/// aldrig i det här objektet — det gör `apply_with_passwords`.
pub fn desired_user(
    username: &str,
    public_keys: &[String],
    folders: &[(String, String)],
    file_uid: u32,
    file_gid: u32,
    has_password: bool,
) -> Option<Value> {
    if !managed_username(username) {
        return None;
    }

    let mut seen_keys = BTreeSet::new();
    let keys: Vec<String> = public_keys
        .iter()
        .map(|key| key.trim().to_string())
        .filter(|key| valid_public_key(key))
        .filter(|key| seen_keys.insert(key.clone()))
        .collect();
    if keys.is_empty() && !has_password {
        return None;
    }

    let mut seen_folders = BTreeSet::new();
    let virtual_folders: Vec<Value> = folders
        .iter()
        .filter(|(name, _)| valid_slug(name))
        .filter(|(name, _)| seen_folders.insert(name.clone()))
        .map(|(name, _)| {
            json!({
                "name": name,
                "virtual_path": format!("/{name}"),
                "quota_size": 0,
                "quota_files": 0,
            })
        })
        .collect();
    if virtual_folders.is_empty() {
        return None;
    }

    Some(json!({
        "status": 1,
        "username": username,
        "description": "CraftPilot managed SFTP account",
        "home_dir": format!("/srv/sftpgo/data/{username}"),
        "public_keys": keys,
        // Hela trädet, men bara de virtual folders som mastern räknat ut.
        "permissions": {"/": ["*"]},
        "virtual_folders": virtual_folders,
        "uid": file_uid,
        "gid": file_gid,
    }))
}

/// Jämför det agenten äger. `password` jämförs aldrig: hashen kan inte läsas
/// tillbaka, så en `Keep` utelämnar fältet och en ändring tvingas igenom
/// separat av `apply_with_passwords`.
fn user_matches(existing: &Value, desired: &Value) -> bool {
    let keys = |value: &Value| -> BTreeSet<String> {
        value
            .get("public_keys")
            .and_then(Value::as_array)
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    existing.get("status").and_then(Value::as_i64) == Some(1)
        && existing.get("home_dir").and_then(Value::as_str)
            == desired.get("home_dir").and_then(Value::as_str)
        && existing.get("uid").and_then(Value::as_u64) == desired.get("uid").and_then(Value::as_u64)
        && existing.get("gid").and_then(Value::as_u64) == desired.get("gid").and_then(Value::as_u64)
        && keys(existing) == keys(desired)
        && user_folders(existing) == user_folders(desired)
}

/// Kontots mappar som SFTPGo har dem: namn och virtuell sökväg.
fn user_folders(value: &Value) -> BTreeSet<(String, String)> {
    value
        .get("virtual_folders")
        .and_then(Value::as_array)
        .map(|folders| {
            folders
                .iter()
                .filter_map(|folder| {
                    Some((
                        folder.get("name")?.as_str()?.to_string(),
                        folder.get("virtual_path")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Sant när uppdateringen lägger till eller tar bort mappar för kontot. Bara då
/// ska SFTPGo stänga kontots sessioner: SFTPGo läser kontot vid inloggning, så
/// en ny mapp syns annars först vid nästa inloggning. Ett nyckel- eller
/// ägarbyte lämnar pågående överföringar i fred.
fn folders_changed(current: &Value, desired: &Value) -> bool {
    user_folders(current) != user_folders(desired)
}

/// Kontots URL. `disconnect=1` stänger kontots öppna sessioner efter
/// uppdateringen, vilket krävs för att en ny mapp ska synas direkt.
fn user_path(username: &str, disconnect: bool) -> Result<String> {
    let segment = path_segment(username)?;
    Ok(if disconnect {
        format!("api/v2/users/{segment}?disconnect=1")
    } else {
        format!("api/v2/users/{segment}")
    })
}

/// En servermapp som får mountas: namnet mastern använder, sökvägen i
/// containern och den resolverade katalogen på noden (för ägaruppslag).
struct ServerMount {
    name: String,
    mapped: String,
    resolved: PathBuf,
}

/// Sökvägen SFTPGo ska mounta för en server, om katalogen finns och ligger
/// under data-roten. Symlänkar och escape-vägar avvisas (AGENTS §6).
fn server_mount(folder: &SftpFolder, data_root: &Path) -> Option<ServerMount> {
    if !valid_slug(&folder.name) {
        return None;
    }
    let resolved = resolve_server_dir(data_root, &folder.server_id)?;
    Some(ServerMount {
        name: folder.name.clone(),
        mapped: format!("{CONTAINER_SERVERS_ROOT}/{}", folder.server_id),
        resolved,
    })
}

/// Serverkatalogens ägare. SFTPGo ska skapa filer med samma uid/gid som
/// katalogen, annars kan spelets container inte skriva dem när noden kör en
/// annan användare än panelens images.
#[cfg(unix)]
fn directory_owner(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.uid(), metadata.gid()))
}

#[cfg(not(unix))]
fn directory_owner(_path: &Path) -> Option<(u32, u32)> {
    None
}

/// Ett explicit override vinner över katalogens ägare; utan båda används
/// panelens tidigare default (33).
fn effective_owner(
    uid_override: Option<u32>,
    gid_override: Option<u32>,
    derived: Option<(u32, u32)>,
) -> (u32, u32) {
    (
        uid_override
            .or_else(|| derived.map(|(uid, _)| uid))
            .unwrap_or(DEFAULT_FILE_UID),
        gid_override
            .or_else(|| derived.map(|(_, gid)| gid))
            .unwrap_or(DEFAULT_FILE_GID),
    )
}

/// Resolverar `<data root>/<server uuid>` och kräver att den verkliga
/// sökvägen ligger under roten. `None` för saknad katalog, symlink, fil
/// eller ett id som inte är ett UUID.
pub fn resolve_server_dir(data_root: &Path, server_id: &str) -> Option<PathBuf> {
    if !crate::docker::is_uuid(server_id) {
        return None;
    }

    let candidate = data_root.join(server_id);
    let metadata = std::fs::symlink_metadata(&candidate).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }

    let root = std::fs::canonicalize(data_root).ok()?;
    let resolved = std::fs::canonicalize(&candidate).ok()?;
    if resolved == root || !resolved.starts_with(&root) {
        return None;
    }
    Some(resolved)
}

fn path_segment(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(AgentError::Runtime(
            "sftpgo object name is not a safe path segment".into(),
        ));
    }
    Ok(value.to_string())
}

/// Värdnyckeln som panelen visar. Ed25519 föredras: det är nyckeln moderna
/// klienter faktiskt visar vid första anslutningen. Fingeravtrycket kommer
/// färdigt från SFTPGo (`SHA256:<base64>`), så agenten behöver aldrig läsa
/// privata nycklar från disk.
pub fn host_key_fingerprint(status: &Value) -> Option<String> {
    let keys = status.get("ssh")?.get("host_keys")?.as_array()?;

    let has_ed25519 = |key: &Value| {
        key.get("algorithms")
            .and_then(Value::as_array)
            .map(|algos| {
                algos
                    .iter()
                    .any(|algo| algo.as_str() == Some("ssh-ed25519"))
            })
            .unwrap_or(false)
    };
    let path_is_ed25519 = |key: &Value| {
        key.get("path")
            .and_then(Value::as_str)
            .map(|path| path.contains("ed25519"))
            .unwrap_or(false)
    };

    let key = keys
        .iter()
        .find(|key| has_ed25519(key))
        .or_else(|| keys.iter().find(|key| path_is_ed25519(key)))
        .or_else(|| keys.first())?;

    let fingerprint = key.get("fingerprint")?.as_str()?.trim().to_string();
    if fingerprint.starts_with("SHA256:")
        && fingerprint.len() <= 120
        && !fingerprint.chars().any(char::is_whitespace)
    {
        Some(fingerprint)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-sftp-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const SERVER_ID: &str = "1f0a9c1e-1111-2222-3333-444455556666";

    fn state(users: Vec<Value>) -> SftpState {
        serde_json::from_value(json!({ "revision": "r1", "users": users })).unwrap()
    }

    fn user(name: &str, key: bool, folder: bool) -> Value {
        json!({
            "username": name,
            "enabled": true,
            "public_keys": if key {
                json!(["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII27tFLzuyH+m76qcssy22gIecV3AWkMIPWBSZvkSS test@host"])
            } else {
                json!([])
            },
            "folders": if folder {
                json!([{ "name": "myserver-ab12cd", "server_id": SERVER_ID }])
            } else {
                json!([])
            },
        })
    }

    #[test]
    fn managed_usernames_are_only_u_ids() {
        assert!(managed_username("u42"));
        assert!(managed_username("u0"));
        assert!(!managed_username("u"));
        assert!(!managed_username("admin"));
        assert!(!managed_username("u42x"));
        assert!(!managed_username("u../42"));
        assert!(!managed_username("craftpilot-agent"));
    }

    #[test]
    fn slugs_are_single_safe_components() {
        assert!(valid_slug("myserver-ab12cd"));
        assert!(valid_slug("my.server_1"));
        assert!(!valid_slug("../etc"));
        assert!(!valid_slug("a/b"));
        assert!(!valid_slug(".hidden"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a..b"));
        assert!(!valid_slug(&"a".repeat(65)));
    }

    #[test]
    fn a_key_must_be_one_openssh_line() {
        assert!(valid_public_key(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII27tFLzuyH+m76qcssy22gIecV3AWkMIPWBSZvkSS test@host"
        ));
        assert!(valid_public_key("ecdsa-sha2-nistp256 AAAA test"));
        assert!(!valid_public_key("not-a-key"));
        assert!(!valid_public_key("ssh-ed25519 AAAA\nssh-ed25519 BBBB"));
        assert!(!valid_public_key(""));
    }

    #[test]
    fn a_user_without_keys_is_never_created() {
        let folders = vec![("myserver-ab12cd".to_string(), format!("/mount/{SERVER_ID}"))];
        assert!(desired_user("u42", &[], &folders, 33, 33, false).is_none());
        assert!(desired_user("u42", &["garbage".to_string()], &folders, 33, 33, false).is_none());
        assert!(desired_user(
            "u42",
            &["ssh-ed25519 AAAA x".to_string()],
            &[],
            33,
            33,
            false
        )
        .is_none());
        // Ett lösenord är också en autentiseringsmetod, men kontot kräver en mapp.
        assert!(desired_user("u42", &[], &folders, 33, 33, true).is_some());
        assert!(desired_user("u42", &[], &[], 33, 33, true).is_none());
    }

    #[test]
    fn desired_user_maps_each_folder_at_its_slug_without_a_password() {
        let folders = vec![("myserver-ab12cd".to_string(), format!("/mount/{SERVER_ID}"))];
        let payload = desired_user(
            "u42",
            &["ssh-ed25519 AAAA x".to_string()],
            &folders,
            33,
            44,
            false,
        )
        .unwrap();

        assert_eq!(payload["username"], "u42");
        assert_eq!(payload["uid"], 33);
        assert_eq!(payload["gid"], 44);
        assert_eq!(payload["home_dir"], "/srv/sftpgo/data/u42");
        assert_eq!(
            payload["virtual_folders"][0]["virtual_path"],
            "/myserver-ab12cd"
        );
        assert!(payload.get("password").is_none());
    }

    #[test]
    fn the_ed25519_host_key_is_preferred() {
        let status = json!({
            "ssh": {
                "host_keys": [
                    {"path": "id_rsa", "fingerprint": "SHA256:rsa", "algorithms": ["rsa-sha2-512"]},
                    {"path": "id_ed25519", "fingerprint": "SHA256:ed25519", "algorithms": ["ssh-ed25519"]}
                ]
            }
        });
        assert_eq!(
            host_key_fingerprint(&status).as_deref(),
            Some("SHA256:ed25519")
        );
    }

    #[test]
    fn a_missing_or_invalid_host_key_is_not_reported() {
        assert_eq!(host_key_fingerprint(&json!({})), None);
        assert_eq!(
            host_key_fingerprint(&json!({"ssh": {"host_keys": []}})),
            None
        );
        assert_eq!(
            host_key_fingerprint(&json!({"ssh": {"host_keys": [{"fingerprint": "nope"}]}})),
            None
        );
    }

    #[test]
    fn server_paths_stay_under_the_data_root() {
        let root = temp_dir("resolve");
        let server = root.join(SERVER_ID);
        std::fs::create_dir_all(&server).unwrap();

        assert!(resolve_server_dir(&root, SERVER_ID).is_some());
        assert!(resolve_server_dir(&root, "../../etc").is_none());
        assert!(resolve_server_dir(&root, "not-a-uuid").is_none());
        assert!(resolve_server_dir(&root, "11111111-2222-3333-4444-555555555555").is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_server_directory_is_refused() {
        let root = temp_dir("symlink");
        let outside = temp_dir("symlink-outside");
        std::os::unix::fs::symlink(&outside, root.join(SERVER_ID)).unwrap();

        assert!(resolve_server_dir(&root, SERVER_ID).is_none());

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn a_folder_for_a_missing_server_directory_is_not_mapped() {
        let root = temp_dir("missing");
        let folder = SftpFolder {
            name: "myserver-ab12cd".into(),
            server_id: SERVER_ID.into(),
        };

        assert!(server_mount(&folder, &root).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_server_directory_owner_is_used_unless_overridden() {
        assert_eq!(
            effective_owner(None, None, Some((1000, 1001))),
            (1000, 1001)
        );
        assert_eq!(
            effective_owner(Some(2000), None, Some((1000, 1001))),
            (2000, 1001)
        );
        assert_eq!(
            effective_owner(None, Some(2001), Some((1000, 1001))),
            (1000, 2001)
        );
        assert_eq!(
            effective_owner(None, None, None),
            (DEFAULT_FILE_UID, DEFAULT_FILE_GID)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_creates_files_with_the_server_directory_owner() {
        let root = temp_dir("owner");
        let server_dir = root.join(SERVER_ID);
        std::fs::create_dir_all(&server_dir).unwrap();
        // CI och agentcontainern kör som root; ett vanligt konto kan inte byta
        // ägare på en katalog, och då hoppar vi över i stället för att falla.
        if std::os::unix::fs::chown(&server_dir, Some(1000), Some(1001)).is_err() {
            std::fs::remove_dir_all(&root).unwrap();
            return;
        }
        assert_eq!(directory_owner(&server_dir), Some((1000, 1001)));

        let server = MockSftpGo::start().await;
        let mut client = server.client().await;
        client
            .apply(
                &state(vec![user("u42", true, true)]),
                &ApplyConfig {
                    data_root: &root,
                    file_uid: None,
                    file_gid: None,
                },
            )
            .await
            .unwrap();
        let stored = server.user("u42").unwrap();
        assert_eq!(stored["uid"], 1000);
        assert_eq!(stored["gid"], 1001);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn apply_creates_folders_and_key_only_users_idempotently() {
        let root = temp_dir("apply");
        std::fs::create_dir_all(root.join(SERVER_ID)).unwrap();
        let server = MockSftpGo::start().await;
        let mut client = server.client().await;

        let state = state(vec![user("u42", true, true)]);
        let outcome = client
            .apply(
                &state,
                &ApplyConfig {
                    data_root: &root,
                    file_uid: None,
                    file_gid: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.users, 1);
        assert_eq!(outcome.folders, 1);
        assert_eq!(outcome.skipped_folders, 0);

        let requests = server.requests();
        assert!(
            requests.contains(&"POST /api/v2/folders".to_string()),
            "{requests:?}"
        );
        assert!(
            requests.contains(&"POST /api/v2/users".to_string()),
            "{requests:?}"
        );

        let stored = server.user("u42").unwrap();
        assert_eq!(
            stored["virtual_folders"][0]["virtual_path"],
            "/myserver-ab12cd"
        );
        assert_eq!(stored["virtual_folders"][0]["name"], "myserver-ab12cd");
        // Den lokala sökvägen ligger på folder-objektet; användaren bär bara
        // mappens namn och var den syns.
        let expected_mount = format!("{CONTAINER_SERVERS_ROOT}/{SERVER_ID}");
        assert_eq!(
            server.folder("myserver-ab12cd").as_deref(),
            Some(expected_mount.as_str())
        );
        assert_eq!(stored["public_keys"].as_array().unwrap().len(), 1);
        assert!(stored.get("password").is_none());

        server.clear_requests();
        client
            .apply(
                &state,
                &ApplyConfig {
                    data_root: &root,
                    file_uid: None,
                    file_gid: None,
                },
            )
            .await
            .unwrap();
        let requests = server.requests();
        assert!(
            requests.iter().all(|request| request.starts_with("GET ")),
            "en oförändrad revision ska inte skriva: {requests:?}"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn apply_removes_accounts_that_left_the_state_but_not_the_admin() {
        let root = temp_dir("cleanup");
        std::fs::create_dir_all(root.join(SERVER_ID)).unwrap();
        let server = MockSftpGo::start().await;
        server.seed_user(
            "u7",
            json!({"status": 1, "username": "u7", "home_dir": "/srv/sftpgo/data/u7"}),
        );
        server.seed_user(
            "craftpilot-agent",
            json!({"status": 1, "username": "craftpilot-agent"}),
        );
        server.seed_folder("old-server", "/srv/craftpilot/servers/old");

        let mut client = server.client().await;
        let state = state(vec![]);
        let outcome = client
            .apply(
                &state,
                &ApplyConfig {
                    data_root: &root,
                    file_uid: None,
                    file_gid: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.users, 0);

        let requests = server.requests();
        assert!(
            requests.contains(&"DELETE /api/v2/users/u7".to_string()),
            "{requests:?}"
        );
        assert!(
            requests.contains(&"DELETE /api/v2/folders/old-server".to_string()),
            "{requests:?}"
        );
        assert!(server.user("u7").is_none());
        assert!(server.user("craftpilot-agent").is_some());
        assert!(server.folder("old-server").is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn apply_never_creates_a_password_only_account() {
        let root = temp_dir("no-keys");
        std::fs::create_dir_all(root.join(SERVER_ID)).unwrap();
        let server = MockSftpGo::start().await;
        let mut client = server.client().await;

        let outcome = client
            .apply(
                &state(vec![user("u42", false, true)]),
                &ApplyConfig {
                    data_root: &root,
                    file_uid: None,
                    file_gid: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.users, 0);
        assert!(server.user("u42").is_none());
        assert!(!server
            .requests()
            .iter()
            .any(|request| request.starts_with("POST /api/v2/users")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn apply_does_not_mount_a_server_whose_directory_is_gone() {
        let root = temp_dir("missing-dir");
        let server = MockSftpGo::start().await;
        let mut client = server.client().await;

        let outcome = client
            .apply(
                &state(vec![user("u42", true, true)]),
                &ApplyConfig {
                    data_root: &root,
                    file_uid: None,
                    file_gid: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.folders, 0);
        assert_eq!(outcome.skipped_folders, 1);
        assert!(!server
            .requests()
            .iter()
            .any(|request| request.starts_with("POST /api/v2/folders")));
        assert!(server.user("u42").is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_user_path_disconnects_only_when_asked() {
        assert_eq!(user_path("u42", false).unwrap(), "api/v2/users/u42");
        assert_eq!(
            user_path("u42", true).unwrap(),
            "api/v2/users/u42?disconnect=1"
        );
        assert!(user_path("u/42", true).is_err());
    }

    #[test]
    fn folders_changed_only_when_the_folder_list_differs() {
        let one = json!({"virtual_folders": [
            {"name": "myserver-ab12cd", "virtual_path": "/myserver-ab12cd"},
        ]});
        let same = json!({"virtual_folders": [
            {"name": "myserver-ab12cd", "virtual_path": "/myserver-ab12cd"},
        ]});
        let two = json!({"virtual_folders": [
            {"name": "myserver-ab12cd", "virtual_path": "/myserver-ab12cd"},
            {"name": "extra-ab12cd", "virtual_path": "/extra-ab12cd"},
        ]});
        let remapped = json!({"virtual_folders": [
            {"name": "myserver-ab12cd", "virtual_path": "/annat"},
        ]});

        assert!(!folders_changed(&one, &same));
        assert!(folders_changed(&one, &two));
        assert!(folders_changed(&two, &one));
        assert!(folders_changed(&one, &remapped));
        // SFTPGo kan svara utan fältet när kontot saknar mappar.
        assert!(!folders_changed(
            &json!({}),
            &json!({"virtual_folders": []})
        ));
        assert!(folders_changed(&json!({}), &two));
    }

    const SECOND_SERVER_ID: &str = "2b1c8d2f-aaaa-bbbb-cccc-ddddeeeeffff";

    fn two_folder_user(key: &str) -> Value {
        json!({
            "username": "u42",
            "enabled": true,
            "public_keys": [key],
            "folders": [
                { "name": "myserver-ab12cd", "server_id": SERVER_ID },
                { "name": "extra-ab12cd", "server_id": SECOND_SERVER_ID },
            ],
        })
    }

    #[tokio::test]
    async fn apply_disconnects_sessions_only_when_the_folder_list_changes() {
        let root = temp_dir("disconnect");
        std::fs::create_dir_all(root.join(SERVER_ID)).unwrap();
        std::fs::create_dir_all(root.join(SECOND_SERVER_ID)).unwrap();
        let server = MockSftpGo::start().await;
        let mut client = server.client().await;
        let cfg = ApplyConfig {
            data_root: &root,
            file_uid: None,
            file_gid: None,
        };
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII27tFLzuyH+m76qcssy22gIecV3AWkMIPWBSZvkSS test@host";

        client
            .apply(&state(vec![user("u42", true, true)]), &cfg)
            .await
            .unwrap();
        assert!(!server
            .requests()
            .iter()
            .any(|request| request.contains("disconnect=1")));

        // En mapp till: sessionerna stängs så mappen syns vid nästa anslutning.
        server.clear_requests();
        client
            .apply(&state(vec![two_folder_user(key)]), &cfg)
            .await
            .unwrap();
        let requests = server.requests();
        assert!(
            requests.contains(&"PUT /api/v2/users/u42?disconnect=1".to_string()),
            "{requests:?}"
        );

        // Bara en ny nyckel: ingen frånkoppling, pågående överföringar lämnas.
        server.clear_requests();
        let other_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII27tFLzuyH+m76qcssy22gIecV3AWkMIPWBSZvkSS laptop@host";
        client
            .apply(&state(vec![two_folder_user(other_key)]), &cfg)
            .await
            .unwrap();
        let requests = server.requests();
        assert!(
            requests.contains(&"PUT /api/v2/users/u42".to_string()),
            "{requests:?}"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.contains("disconnect=1")),
            "{requests:?}"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_password_only_account_is_created_and_removed_when_it_has_no_auth_method() {
        let root = temp_dir("password");
        std::fs::create_dir_all(root.join(SERVER_ID)).unwrap();
        let server = MockSftpGo::start().await;
        let mut client = server.client().await;
        let cfg = ApplyConfig {
            data_root: &root,
            file_uid: None,
            file_gid: None,
        };
        let state = state(vec![user("u42", false, true)]);

        let mut passwords = BTreeMap::new();
        passwords.insert(
            "u42".to_string(),
            PasswordIntent::Set("hunter2".to_string()),
        );
        client
            .apply_with_passwords(&state, &cfg, &passwords)
            .await
            .unwrap();

        let stored = server.user("u42").unwrap();
        assert_eq!(stored["password"], "hunter2");
        assert_eq!(stored["public_keys"].as_array().unwrap().len(), 0);

        // Rensat lösenord utan nyckel: ingen autentiseringsmetod kvar.
        let mut passwords = BTreeMap::new();
        passwords.insert("u42".to_string(), PasswordIntent::Clear);
        client
            .apply_with_passwords(&state, &cfg, &passwords)
            .await
            .unwrap();
        assert!(server.user("u42").is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn an_applied_password_is_never_resent() {
        let root = temp_dir("keep");
        std::fs::create_dir_all(root.join(SERVER_ID)).unwrap();
        let server = MockSftpGo::start().await;
        let mut client = server.client().await;
        let cfg = ApplyConfig {
            data_root: &root,
            file_uid: None,
            file_gid: None,
        };
        let state = state(vec![user("u42", true, true)]);

        let mut passwords = BTreeMap::new();
        passwords.insert(
            "u42".to_string(),
            PasswordIntent::Set("hunter2".to_string()),
        );
        let outcome = client
            .apply_with_passwords(&state, &cfg, &passwords)
            .await
            .unwrap();
        assert_eq!(outcome.password_users.len(), 1);

        // `Keep` ska varken skriva om kontot eller skicka lösenordet igen.
        server.clear_requests();
        let mut passwords = BTreeMap::new();
        passwords.insert("u42".to_string(), PasswordIntent::Keep);
        client
            .apply_with_passwords(&state, &cfg, &passwords)
            .await
            .unwrap();
        let requests = server.requests();
        assert!(
            !requests
                .iter()
                .any(|request| request.starts_with("PUT /api/v2/users")),
            "{requests:?}"
        );
        assert_eq!(server.user("u42").unwrap()["password"], "hunter2");

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Minimal SFTPGo-admin för testerna: bara de endpoints klienten
    /// använder, med samma svarsformer som 2.6.
    struct MockSftpGo {
        state: Arc<Mutex<MockState>>,
        base: Url,
    }

    #[derive(Default)]
    struct MockState {
        users: BTreeMap<String, Value>,
        folders: BTreeMap<String, String>,
        requests: Vec<String>,
    }

    impl MockSftpGo {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let state = Arc::new(Mutex::new(MockState::default()));
            let shared = Arc::clone(&state);

            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let state = Arc::clone(&shared);
                    tokio::spawn(async move {
                        let _ = handle(stream, state).await;
                    });
                }
            });

            Self {
                state,
                base: Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
            }
        }

        async fn client(&self) -> SftpGoClient {
            SftpGoClient::new(
                self.base.clone(),
                "craftpilot-agent".to_string(),
                "test-password".to_string(),
            )
            .unwrap()
        }

        fn requests(&self) -> Vec<String> {
            self.state.lock().unwrap().requests.clone()
        }

        fn clear_requests(&self) {
            self.state.lock().unwrap().requests.clear();
        }

        fn seed_user(&self, name: &str, value: Value) {
            self.state
                .lock()
                .unwrap()
                .users
                .insert(name.to_string(), value);
        }

        fn seed_folder(&self, name: &str, mapped: &str) {
            self.state
                .lock()
                .unwrap()
                .folders
                .insert(name.to_string(), mapped.to_string());
        }

        fn user(&self, name: &str) -> Option<Value> {
            self.state.lock().unwrap().users.get(name).cloned()
        }

        fn folder(&self, name: &str) -> Option<String> {
            self.state.lock().unwrap().folders.get(name).cloned()
        }
    }

    async fn handle(mut stream: TcpStream, state: Arc<Mutex<MockState>>) -> std::io::Result<()> {
        let Some((method, target, body)) = read_request(&mut stream).await else {
            return Ok(());
        };
        state
            .lock()
            .unwrap()
            .requests
            .push(format!("{method} {target}"));
        let path = target
            .split('?')
            .next()
            .unwrap_or(target.as_str())
            .to_string();

        let (status, payload) = route(&method, &path, &body, &state);
        write_response(&mut stream, status, &payload).await
    }

    fn route(method: &str, path: &str, body: &[u8], state: &Arc<Mutex<MockState>>) -> (u16, Value) {
        match (method, path) {
            ("GET", "/api/v2/token") => {
                (200, json!({"access_token": "test-token", "expires_at": 0}))
            }
            ("GET", "/api/v2/status") => (
                200,
                json!({
                    "ssh": {"host_keys": [
                        {"path": "id_rsa", "fingerprint": "SHA256:rsa-key", "algorithms": ["rsa-sha2-512"]},
                        {"path": "id_ed25519", "fingerprint": "SHA256:ed25519-key", "algorithms": ["ssh-ed25519"]}
                    ]}
                }),
            ),
            ("GET", "/api/v2/users") => {
                let users: Vec<Value> = state.lock().unwrap().users.values().cloned().collect();
                (200, Value::Array(users))
            }
            ("POST", "/api/v2/users") => {
                let value: Value = serde_json::from_slice(body).unwrap();
                let name = value["username"].as_str().unwrap().to_string();
                state.lock().unwrap().users.insert(name, value.clone());
                (201, value)
            }
            ("GET", "/api/v2/folders") => {
                let folders: Vec<Value> = state
                    .lock()
                    .unwrap()
                    .folders
                    .iter()
                    .map(|(name, mapped)| json!({"name": name, "mapped_path": mapped}))
                    .collect();
                (200, Value::Array(folders))
            }
            ("POST", "/api/v2/folders") => {
                let value: Value = serde_json::from_slice(body).unwrap();
                let name = value["name"].as_str().unwrap().to_string();
                let mapped = value["mapped_path"].as_str().unwrap().to_string();
                state.lock().unwrap().folders.insert(name, mapped);
                (201, value)
            }
            _ if path.starts_with("/api/v2/users/") => {
                let name = &path["/api/v2/users/".len()..];
                match method {
                    "GET" => match state.lock().unwrap().users.get(name).cloned() {
                        Some(value) => (200, value),
                        None => (404, json!({"error": "not found"})),
                    },
                    "PUT" => {
                        let value: Value = serde_json::from_slice(body).unwrap();
                        state
                            .lock()
                            .unwrap()
                            .users
                            .insert(name.to_string(), value.clone());
                        (200, value)
                    }
                    "DELETE" => {
                        state.lock().unwrap().users.remove(name);
                        (204, Value::Null)
                    }
                    _ => (405, Value::Null),
                }
            }
            _ if path.starts_with("/api/v2/folders/") => {
                let name = &path["/api/v2/folders/".len()..];
                match method {
                    "GET" => match state.lock().unwrap().folders.get(name).cloned() {
                        Some(mapped) => (200, json!({"name": name, "mapped_path": mapped})),
                        None => (404, json!({"error": "not found"})),
                    },
                    "PUT" => {
                        let value: Value = serde_json::from_slice(body).unwrap();
                        let mapped = value["mapped_path"].as_str().unwrap().to_string();
                        state
                            .lock()
                            .unwrap()
                            .folders
                            .insert(name.to_string(), mapped);
                        (200, value)
                    }
                    "DELETE" => {
                        state.lock().unwrap().folders.remove(name);
                        (204, Value::Null)
                    }
                    _ => (405, Value::Null),
                }
            }
            _ => (404, json!({"error": "not found"})),
        }
    }

    async fn read_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position;
            }
        };

        let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
        let mut lines = head.lines();
        let mut first = lines.next()?.split_whitespace();
        let method = first.next()?.to_string();
        let target = first.next()?.to_string();

        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);

        let mut body = buffer[head_end + 4..].to_vec();
        while body.len() < length {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        body.truncate(length);

        Some((method, target, body))
    }

    async fn write_response(
        stream: &mut TcpStream,
        status: u16,
        payload: &Value,
    ) -> std::io::Result<()> {
        let body = if payload.is_null() {
            String::new()
        } else {
            payload.to_string()
        };
        let response = format!(
            "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            if status < 300 { "OK" } else { "Error" },
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await
    }
}
