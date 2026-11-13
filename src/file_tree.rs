use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::docker::is_uuid;
use crate::error::Result;

/// Samma gränser som Python-agentens `file_tree_index.py` och masterns
/// `SyncRuntimeNodeFileTreeRequest`.
const INDEX_VERSION: u32 = 1;
const INDEX_FILENAME: &str = "file-tree-index.json";
const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 1001;
const MAX_DEPTH: usize = 32;
const MAX_NAME_BYTES: usize = 255;
pub const SYNC_BATCH: usize = 20;

/// En återscanning var 30:e sekund fångar ändringar som gjorts direkt på
/// värden (modpack-installationer, manuella filbyten) utan att kräva en
/// inotify-watcher. Egna filmutationer invaliderar sin katalog direkt.
const FULL_SCAN_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: Option<u64>,
    pub modified_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    mtime_ns: u64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    directories: BTreeMap<String, RecordDocument>,
}

#[derive(Serialize, Deserialize)]
struct RecordDocument {
    mtime_ns: u64,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Progress {
    Fetching,
    Building,
    Indexing,
    Ready,
}

impl Progress {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fetching => "fetching",
            Self::Building => "building",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
        }
    }
}

struct State {
    directories: BTreeMap<String, Record>,
    dirty: BTreeSet<String>,
    updates: BTreeMap<String, (u64, Option<Listing>)>,
    revision: u64,
    ready: bool,
    assignment_known: bool,
    progress: Progress,
    scanned: BTreeSet<String>,
    last_scan: Option<Instant>,
}

/// Ett delta som mastern kan ta emot i `POST /file-tree`. `revision` används
/// bara lokalt för att inte acka ett nyare delta än det som skickades.
pub struct PendingUpdate {
    pub key: String,
    pub revision: u64,
    pub payload: Value,
}

/// Beständigt katalogindex per server. Endast kataloglister cachas; själva
/// filinnehållet läses fortfarande genom den säkra filhanteraren.
pub struct FileTreeIndex {
    data_root: PathBuf,
    state_dir: PathBuf,
    index_path: PathBuf,
    state: Mutex<State>,
    assigned: Mutex<BTreeSet<String>>,
    started: AtomicBool,
}

impl FileTreeIndex {
    pub fn new(data_root: PathBuf, state_dir: PathBuf) -> Self {
        let index_path = state_dir.join(INDEX_FILENAME);
        let directories = load_index(&index_path);
        let dirty = directories.keys().cloned().collect();
        Self {
            data_root,
            state_dir,
            index_path,
            state: Mutex::new(State {
                directories,
                dirty,
                updates: BTreeMap::new(),
                revision: 0,
                ready: false,
                assignment_known: false,
                progress: Progress::Fetching,
                scanned: BTreeSet::new(),
                last_scan: None,
            }),
            assigned: Mutex::new(BTreeSet::new()),
            started: AtomicBool::new(false),
        }
    }

    /// Startar en bakgrundstråd så en stor värld kan indexeras utan att
    /// blockera claim/ack. Agenten uppdaterar tilldelningen med
    /// `set_assigned`; tråden skannar om vid start, ändrad tilldelning och
    /// sedan var 30:e sekund.
    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let index = Arc::clone(self);
        let result = std::thread::Builder::new()
            .name("craftpilot-file-tree".to_string())
            .spawn(move || loop {
                let assigned = index
                    .assigned
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                if index.should_scan(&assigned, FULL_SCAN_INTERVAL) {
                    if let Err(err) = index.scan_assigned(&assigned) {
                        warn!(error = %err, "scanning the server file tree failed");
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            });
        if let Err(err) = result {
            warn!(error = %err, "starting the file tree worker failed");
        }
    }

    pub fn set_assigned(&self, assigned: &BTreeSet<String>) {
        let mut current = self.lock_assigned();
        if *current != *assigned {
            *current = assigned.clone();
        }
        self.lock().assignment_known = true;
    }

    pub fn progress(&self) -> String {
        self.lock().progress.as_str().to_string()
    }

    pub fn should_scan(&self, assigned: &BTreeSet<String>, interval: Duration) -> bool {
        let state = self.lock();
        if assigned.is_empty() && !state.assignment_known {
            return false;
        }
        !state.ready
            || &state.scanned != assigned
            || state
                .last_scan
                .is_none_or(|last| last.elapsed() >= interval.max(FULL_SCAN_INTERVAL))
    }

    /// Skannar om alla tilldelade serverrötter. Kataloglister som är
    /// oförändrade skickas inte igen; borttagna kataloger skickas som
    /// `deleted` så mastern kan rensa sin cache.
    pub fn scan_assigned(&self, assigned: &BTreeSet<String>) -> Result<()> {
        self.set_progress(Progress::Fetching);
        let mut directories = BTreeMap::new();
        if !assigned.is_empty() {
            self.set_progress(Progress::Building);
            for server_id in assigned {
                if !is_uuid(server_id) {
                    continue;
                }
                let root = self.data_root.join(server_id);
                if !is_real_directory(&root) {
                    continue;
                }
                Self::scan_subtree(&root, server_id, 0, &mut directories);
            }
        }

        self.set_progress(Progress::Indexing);
        let mut state = self.lock();
        let initial = !state.ready;
        let mut queued = Vec::new();
        for (key, record) in &directories {
            let changed = state
                .directories
                .get(key)
                .map(|old| old.entries != record.entries)
                .unwrap_or(true);
            if initial || changed {
                queued.push((key.clone(), Some(listing_for(key, &record.entries))));
            }
        }
        for key in state.directories.keys() {
            let server_id = server_for(key);
            if assigned.contains(server_id) && !directories.contains_key(key) {
                queued.push((key.clone(), None));
            }
        }

        for (key, listing) in queued {
            queue_locked(&mut state, &key, listing);
        }
        state
            .updates
            .retain(|key, _| assigned.contains(server_for(key)));
        state.directories = directories;
        state.dirty.clear();
        state.ready = true;
        state.assignment_known = true;
        state.scanned = assigned.clone();
        state.last_scan = Some(Instant::now());
        state.progress = Progress::Ready;
        persist_locked(&self.index_path, &self.state_dir, &state)?;
        Ok(())
    }

    /// Markerar katalogen som innehåller en agentstyrd filändring.
    pub fn invalidate(&self, server_id: &str, relative: &str) {
        let key = directory_key(server_id, relative);
        if !valid_key(&key) {
            return;
        }
        let mut state = self.lock();
        state.dirty.insert(key);
    }

    /// Översätter kommandots payload till de kataloger som kan ha ändrats.
    /// En borttagen eller flyttad katalog prunas direkt; nya underkataloger
    /// indexeras vid nästa fullständiga återscanning.
    pub fn invalidate_after_mutation(&self, server_id: &str, payload: &Value) {
        let operation = payload
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let path = payload
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let directory = match operation {
            "mkdir" | "touch" | "upload" => path.to_string(),
            _ => path
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_string())
                .unwrap_or_default(),
        };
        self.invalidate(server_id, &directory);

        if matches!(operation, "delete" | "rename") {
            self.prune_subtree(server_id, path);
        }
    }

    /// Returnerar nästa batch, root-kataloger först. Anroparen ackar först
    /// efter att mastern svarat, så ett nätverksfel kan retryas nästa tick.
    pub fn pending_updates(&self, assigned: &BTreeSet<String>, limit: usize) -> Vec<PendingUpdate> {
        if limit == 0 {
            return Vec::new();
        }
        let dirty = {
            let state = self.lock();
            if !state.ready {
                return Vec::new();
            }
            state
                .dirty
                .iter()
                .filter(|key| assigned.contains(server_for(key)))
                .cloned()
                .collect::<Vec<_>>()
        };
        for key in dirty {
            self.refresh_key(&key);
        }

        let state = self.lock();
        if !state.ready {
            return Vec::new();
        }
        let mut pending: Vec<_> = state
            .updates
            .iter()
            .filter(|(key, _)| assigned.contains(server_for(key)))
            .map(|(key, (revision, listing))| (key.clone(), *revision, listing.clone()))
            .collect();
        pending.sort_by(|left, right| {
            (left.0.contains('/'), &left.0).cmp(&(right.0.contains('/'), &right.0))
        });
        pending.truncate(limit);
        pending
            .into_iter()
            .map(|(key, revision, listing)| {
                let server_id = server_for(&key).to_string();
                let relative = relative_for(&key);
                let payload = match listing {
                    Some(listing) => json!({
                        "server_id": server_id,
                        "path": relative,
                        "listing": listing,
                    }),
                    None => json!({
                        "server_id": server_id,
                        "path": relative,
                        "deleted": true,
                    }),
                };
                PendingUpdate {
                    key,
                    revision,
                    payload,
                }
            })
            .collect()
    }

    pub fn acknowledge(&self, updates: &[PendingUpdate]) {
        let mut state = self.lock();
        for update in updates {
            let current = state.updates.get(&update.key);
            if current.map(|(revision, _)| *revision) == Some(update.revision) {
                state.updates.remove(&update.key);
            }
        }
    }

    fn set_progress(&self, progress: Progress) {
        self.lock().progress = progress;
    }

    fn scan_subtree(
        directory: &Path,
        key: &str,
        depth: usize,
        output: &mut BTreeMap<String, Record>,
    ) {
        let Ok(metadata) = fs::symlink_metadata(directory) else {
            return;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return;
        }
        let entries = scan_directory(directory);
        output.insert(
            key.to_string(),
            Record {
                mtime_ns: mtime_ns(&metadata),
                entries: entries.clone(),
            },
        );
        if depth >= MAX_DEPTH {
            return;
        }
        for entry in entries {
            if entry.kind != "dir" {
                continue;
            }
            let child = directory.join(&entry.name);
            if !is_real_directory(&child) {
                continue;
            }
            let child_key = format!("{key}/{}", entry.name);
            Self::scan_subtree(&child, &child_key, depth + 1, output);
        }
    }

    fn refresh_key(&self, key: &str) {
        if !valid_key(key) {
            return;
        }
        let Some(path) = self.resolve_key(key) else {
            self.prune_subtree_key(key);
            return;
        };
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            self.prune_subtree_key(key);
            return;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            self.prune_subtree_key(key);
            return;
        }
        let entries = scan_directory(&path);
        let mut state = self.lock();
        let changed = state
            .directories
            .get(key)
            .map(|record| record.entries != entries)
            .unwrap_or(true);
        state.directories.insert(
            key.to_string(),
            Record {
                mtime_ns: mtime_ns(&metadata),
                entries: entries.clone(),
            },
        );
        state.dirty.remove(key);
        if changed {
            queue_locked(&mut state, key, Some(listing_for(key, &entries)));
        }
        if let Err(err) = persist_locked(&self.index_path, &self.state_dir, &state) {
            warn!(error = %err, "persisting the file tree index failed");
        }
    }

    fn prune_subtree(&self, server_id: &str, relative: &str) {
        let key = directory_key(server_id, relative);
        if valid_key(&key) {
            self.prune_subtree_key(&key);
        }
    }

    fn prune_subtree_key(&self, key: &str) {
        let prefix = format!("{key}/");
        let mut state = self.lock();
        let removed = state
            .directories
            .keys()
            .filter(|path| *path == key || path.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        for path in removed {
            state.directories.remove(&path);
            state.dirty.remove(&path);
            queue_locked(&mut state, &path, None);
        }
        if let Err(err) = persist_locked(&self.index_path, &self.state_dir, &state) {
            warn!(error = %err, "persisting the file tree index failed");
        }
    }

    fn resolve_key(&self, key: &str) -> Option<PathBuf> {
        let mut parts = key.split('/');
        let server_id = parts.next()?;
        if !is_uuid(server_id) {
            return None;
        }
        let mut path = self.data_root.join(server_id);
        if !is_real_directory(&path) {
            return None;
        }
        for part in parts {
            if !is_safe_name(part) {
                return None;
            }
            path.push(part);
            if !is_real_directory(&path) {
                return None;
            }
        }
        Some(path)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_assigned(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.assigned
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn load_index(path: &Path) -> BTreeMap<String, Record> {
    let Ok(metadata) = fs::metadata(path) else {
        return BTreeMap::new();
    };
    if metadata.len() > MAX_INDEX_BYTES as u64 {
        warn!("ignoring an oversized file tree index");
        return BTreeMap::new();
    }
    let Ok(bytes) = fs::read(path) else {
        return BTreeMap::new();
    };
    let Ok(document) = serde_json::from_slice::<Document>(&bytes) else {
        warn!("ignoring a corrupt file tree index");
        return BTreeMap::new();
    };
    if document.version != INDEX_VERSION {
        return BTreeMap::new();
    }

    let mut directories = BTreeMap::new();
    for (key, record) in document.directories {
        if !valid_key(&key)
            || record.entries.len() > MAX_DIRECTORY_ENTRIES
            || record.entries.iter().any(|entry| !valid_entry(entry))
        {
            warn!("ignoring a file tree index with invalid entries");
            return BTreeMap::new();
        }
        directories.insert(
            key,
            Record {
                mtime_ns: record.mtime_ns,
                entries: record.entries,
            },
        );
    }
    directories
}

fn persist_locked(path: &Path, state_dir: &Path, state: &State) -> Result<()> {
    let directories = state
        .directories
        .iter()
        .map(|(key, record)| {
            (
                key.clone(),
                RecordDocument {
                    mtime_ns: record.mtime_ns,
                    entries: record.entries.clone(),
                },
            )
        })
        .collect();
    let bytes = serde_json::to_vec(&Document {
        version: INDEX_VERSION,
        directories,
    })?;
    if bytes.len() > MAX_INDEX_BYTES {
        warn!(
            bytes = bytes.len(),
            "file tree index is too large to persist"
        );
        return Ok(());
    }

    fs::create_dir_all(state_dir)?;
    let temporary = state_dir.join(format!(".{INDEX_FILENAME}.tmp"));
    {
        let mut file = fs::File::create(&temporary)?;
        restrict_permissions(&temporary, 0o600)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn scan_directory(directory: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let Ok(iterator) = fs::read_dir(directory) else {
        return entries;
    };
    for entry in iterator {
        if entries.len() >= MAX_DIRECTORY_ENTRIES {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_safe_name(&name) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let kind = if metadata.file_type().is_symlink() {
            "link"
        } else if metadata.is_dir() {
            "dir"
        } else if metadata.is_file() {
            "file"
        } else {
            "link"
        };
        let size = if kind == "file" || kind == "link" {
            Some(metadata.len())
        } else {
            None
        };
        let modified_at = metadata
            .modified()
            .map(crate::files::iso8601)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
        entries.push(Entry {
            name,
            kind: kind.to_string(),
            size,
            modified_at,
        });
    }
    entries.sort_by(|left, right| {
        (rank(&left.kind), &left.name).cmp(&(rank(&right.kind), &right.name))
    });
    entries
}

fn rank(kind: &str) -> u8 {
    match kind {
        "dir" => 0,
        "link" => 1,
        _ => 2,
    }
}

fn queue_locked(state: &mut State, key: &str, listing: Option<Listing>) {
    state.revision += 1;
    state
        .updates
        .insert(key.to_string(), (state.revision, listing));
}

fn listing_for(key: &str, entries: &[Entry]) -> Listing {
    let relative = relative_for(key);
    let parent = if relative.is_empty() {
        None
    } else {
        Some(
            relative
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_string())
                .unwrap_or_default(),
        )
    };
    Listing {
        path: relative.to_string(),
        parent,
        entries: entries.to_vec(),
    }
}

fn directory_key(server_id: &str, relative: &str) -> String {
    if relative.is_empty() {
        server_id.to_string()
    } else {
        format!("{server_id}/{relative}")
    }
}

fn server_for(key: &str) -> &str {
    key.split('/').next().unwrap_or(key)
}

fn relative_for(key: &str) -> &str {
    key.split_once('/')
        .map(|(_, relative)| relative)
        .unwrap_or("")
}

fn valid_key(key: &str) -> bool {
    if key.is_empty() || key.starts_with('/') || key.contains('\\') {
        return false;
    }
    let mut parts = key.split('/');
    let Some(server_id) = parts.next() else {
        return false;
    };
    if !is_uuid(server_id) {
        return false;
    }
    let mut depth = 0;
    for part in parts {
        if !is_safe_name(part) {
            return false;
        }
        depth += 1;
    }
    depth <= MAX_DEPTH
}

fn valid_entry(entry: &Entry) -> bool {
    if !is_safe_name(&entry.name) || !matches!(entry.kind.as_str(), "file" | "dir" | "link") {
        return false;
    }
    match entry.kind.as_str() {
        "dir" => entry.size.is_none(),
        _ => entry.size.is_some(),
    }
}

fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= MAX_NAME_BYTES
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn mtime_ns(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
                .unwrap_or_default()
        })
}

#[cfg(unix)]
fn restrict_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = "11111111-1111-1111-1111-111111111111";

    fn sandbox(label: &str) -> (PathBuf, PathBuf, BTreeSet<String>) {
        let root = std::env::temp_dir().join(format!(
            "craftpilot-file-tree-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let data = root.join("servers");
        let state = root.join("state");
        fs::create_dir_all(data.join(SERVER).join("world")).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(data.join(SERVER).join("server.properties"), "motd=test\n").unwrap();
        fs::write(data.join(SERVER).join("world").join("level.dat"), [1, 2, 3]).unwrap();
        let assigned = BTreeSet::from([SERVER.to_string()]);
        (data, state, assigned)
    }

    fn cleanup(data: &Path) {
        let _ = fs::remove_dir_all(data.parent().unwrap());
    }

    #[test]
    fn scan_queues_root_first_and_batches_updates() {
        let (data, state, assigned) = sandbox("scan");
        let index = FileTreeIndex::new(data.clone(), state);
        index.scan_assigned(&assigned).unwrap();
        assert_eq!(index.progress(), "ready");

        let pending = index.pending_updates(&assigned, SYNC_BATCH);
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].payload["path"], "");
        assert_eq!(pending[0].payload["server_id"], SERVER);
        assert_eq!(pending[0].payload["listing"]["parent"], Value::Null);
        assert_eq!(pending[1].payload["path"], "world");
        assert_eq!(pending[1].payload["listing"]["parent"], "");
        assert_eq!(
            pending[0].payload["listing"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == "server.properties")
                .unwrap()["type"],
            "file"
        );

        index.acknowledge(&pending);
        assert!(index.pending_updates(&assigned, SYNC_BATCH).is_empty());
        cleanup(&data);
    }

    #[test]
    fn invalidation_sends_a_fresh_listing() {
        let (data, state, assigned) = sandbox("invalidate");
        let index = FileTreeIndex::new(data.clone(), state);
        index.scan_assigned(&assigned).unwrap();
        index.acknowledge(&index.pending_updates(&assigned, SYNC_BATCH));

        fs::write(data.join(SERVER).join("new-file.txt"), "hello").unwrap();
        index.invalidate(SERVER, "");
        let pending = index.pending_updates(&assigned, SYNC_BATCH);
        assert_eq!(pending.len(), 1);
        assert!(pending[0].payload["listing"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "new-file.txt"));
        cleanup(&data);
    }

    #[test]
    fn a_removed_directory_is_reported_as_deleted() {
        let (data, state, assigned) = sandbox("deleted");
        let index = FileTreeIndex::new(data.clone(), state);
        index.scan_assigned(&assigned).unwrap();
        index.acknowledge(&index.pending_updates(&assigned, SYNC_BATCH));

        fs::remove_dir_all(data.join(SERVER).join("world")).unwrap();
        index.scan_assigned(&assigned).unwrap();
        let pending = index.pending_updates(&assigned, SYNC_BATCH);
        assert!(pending.iter().any(|update| {
            update.payload["path"] == "world" && update.payload["deleted"] == true
        }));
        cleanup(&data);
    }

    #[test]
    fn agent_mutations_invalidate_the_containing_directory() {
        let (data, state, assigned) = sandbox("mutation");
        let index = FileTreeIndex::new(data.clone(), state);
        index.scan_assigned(&assigned).unwrap();
        index.acknowledge(&index.pending_updates(&assigned, SYNC_BATCH));

        fs::write(
            data.join(SERVER).join("world").join("level.dat"),
            [9, 9, 9, 9],
        )
        .unwrap();
        index.invalidate_after_mutation(
            SERVER,
            &json!({ "operation": "write", "path": "world/level.dat" }),
        );
        let pending = index.pending_updates(&assigned, SYNC_BATCH);
        assert_eq!(pending[0].payload["path"], "world");
        cleanup(&data);
    }

    #[test]
    fn unsafe_names_are_never_indexed() {
        let (data, state, assigned) = sandbox("unsafe");
        fs::write(data.join(SERVER).join("bad\nname"), "x").unwrap();
        fs::write(data.join(SERVER).join("back\\slash"), "x").unwrap();
        let index = FileTreeIndex::new(data.clone(), state);
        index.scan_assigned(&assigned).unwrap();
        let pending = index.pending_updates(&assigned, SYNC_BATCH);
        let root = pending
            .iter()
            .find(|update| update.payload["path"] == "")
            .unwrap();
        let names = root.payload["listing"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!names.iter().any(|name| name.contains('\n')));
        assert!(!names.iter().any(|name| name.contains('\\')));
        cleanup(&data);
    }

    #[test]
    fn persisted_index_is_private_and_not_trusted_as_ready() {
        let (data, state, assigned) = sandbox("persist");
        let index = FileTreeIndex::new(data.clone(), state.clone());
        index.scan_assigned(&assigned).unwrap();
        assert!(state.join(INDEX_FILENAME).exists());

        let reloaded = FileTreeIndex::new(data.clone(), state);
        assert!(reloaded.should_scan(&assigned, FULL_SCAN_INTERVAL));
        assert_eq!(reloaded.progress(), "fetching");
        assert!(reloaded.pending_updates(&assigned, SYNC_BATCH).is_empty());
        cleanup(&data);
    }
}
