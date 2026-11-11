use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::warn;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::api::MasterClient;
use crate::docker::server_id;
use crate::error::{AgentError, Result};
use crate::snapshot::extract_zip;

/// Samma gränser som masterns `ServerPath`.
const MAX_SEGMENT_BYTES: usize = 255;
const MAX_DEPTH: usize = 32;
const MAX_LENGTH: usize = 4096;
/// Mastern accepterar högst 1000 poster i en kataloglistning.
const MAX_ENTRIES: usize = 1000;
const MAX_ZIP_FILES: u64 = 50_000;
const MAX_ZIP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_WALK_DEPTH: usize = 64;

/// Filoperationer inom en servers sandboxrot. Varje sökväg normaliseras
/// lexikalt och resolveras sedan mot roten: ingen `..`, inga absoluta
/// sökvägar, inga kontrolltecken och ingen symlink som leder utanför roten.
pub struct Files {
    data_root: PathBuf,
    max_edit_bytes: u64,
    max_upload_bytes: u64,
}

impl Files {
    pub fn new(data_root: PathBuf, max_edit_bytes: u64, max_upload_bytes: u64) -> Self {
        Self {
            data_root,
            max_edit_bytes,
            max_upload_bytes,
        }
    }

    pub fn list(&self, payload: &Value) -> Result<Value> {
        let root = self.root_for(&server_id(payload)?)?;
        let path = payload_path(payload)?;
        let directory = resolve(&root, &path, true)?;

        let metadata = std::fs::symlink_metadata(&directory)
            .map_err(|_| invalid("the directory does not exist"))?;
        if !metadata.is_dir() {
            return Err(invalid("the path is not a directory"));
        }

        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let name = match entry.file_name().into_string() {
                Ok(name) if is_safe_name(&name) => name,
                _ => {
                    warn!("skipping a directory entry with an unsafe name");
                    continue;
                }
            };
            let metadata = std::fs::symlink_metadata(entry.path())?;
            let kind = if metadata.file_type().is_symlink() {
                "link"
            } else if metadata.is_dir() {
                "dir"
            } else {
                "file"
            };
            let size = if kind == "dir" {
                Value::Null
            } else {
                json!(metadata.len())
            };
            let modified_at = metadata
                .modified()
                .map(iso8601)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());

            entries.push(json!({
                "name": name,
                "type": kind,
                "size": size,
                "modified_at": modified_at,
            }));
        }

        if entries.len() > MAX_ENTRIES {
            return Err(invalid("the directory has too many entries to list"));
        }
        entries.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));

        Ok(json!({
            "path": path,
            "parent": parent_of(&path),
            "entries": entries,
        }))
    }

    pub fn read(&self, payload: &Value) -> Result<Value> {
        let root = self.root_for(&server_id(payload)?)?;
        let path = payload_path(payload)?;
        if path.is_empty() {
            return Err(invalid("the server root is a directory"));
        }

        let absolute = resolve(&root, &path, true)?;
        let metadata =
            std::fs::metadata(&absolute).map_err(|_| invalid("the file does not exist"))?;
        if !metadata.is_file() {
            return Err(invalid("the path is not a file"));
        }
        if metadata.len() > self.max_edit_bytes {
            return Err(invalid("the file is larger than the edit limit"));
        }

        let bytes = std::fs::read(&absolute)?;
        let content =
            String::from_utf8(bytes).map_err(|_| invalid("the file is not valid UTF-8 text"))?;

        Ok(json!({
            "path": path,
            "content": content,
            "bytes": content.len(),
            "name": basename(&path),
        }))
    }

    pub async fn mutate(
        &self,
        payload: &Value,
        api: &MasterClient,
        node_id: &str,
        command_id: &str,
        lease_token: &str,
    ) -> Result<Value> {
        let root = self.root_for(&server_id(payload)?)?;
        let path = payload_path(payload)?;
        let operation = payload
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("file mutation has no operation"))?;

        match operation {
            "write" => {
                let content = payload
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("file mutation has no content"))?;
                if content.len() as u64 > self.max_edit_bytes {
                    return Err(invalid("the content is larger than the edit limit"));
                }
                self.write(&root, &path, content)?;
            }
            "mkdir" => {
                let name = payload_name(payload)?;
                let parent = existing_directory(&root, &path)?;
                create_entry(&parent.join(&name), true)?;
            }
            "touch" => {
                let name = payload_name(payload)?;
                let parent = existing_directory(&root, &path)?;
                create_entry(&parent.join(&name), false)?;
            }
            "rename" => {
                let name = payload_name(payload)?;
                self.rename(&root, &path, &name)?;
            }
            "delete" => self.delete(&root, &path)?,
            "zip" => self.zip(&root, &path)?,
            "unzip" => self.unzip(&root, &path)?,
            "upload" => {
                return self
                    .upload(payload, &root, &path, api, node_id, command_id, lease_token)
                    .await
            }
            other => return Err(invalid(&format!("unsupported file mutation {other}"))),
        }

        Ok(json!({ "operation": operation, "ok": true }))
    }

    fn write(&self, root: &Path, path: &str, content: &str) -> Result<()> {
        if path.is_empty() {
            return Err(invalid("the server root is a directory"));
        }

        let absolute = resolve(root, path, true)?;
        if std::fs::metadata(&absolute)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            return Err(invalid("the path is a directory"));
        }
        let parent = absolute
            .parent()
            .ok_or_else(|| invalid("the parent directory does not exist"))?;
        if !parent.is_dir() {
            return Err(invalid("the parent directory does not exist"));
        }

        let temporary = parent.join(format!(".cp-write-{}", unique_suffix()));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(content.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            set_mode(&temporary, 0o644)?;
            std::fs::rename(&temporary, &absolute)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    fn rename(&self, root: &Path, path: &str, name: &str) -> Result<()> {
        if path.is_empty() {
            return Err(invalid("the server root cannot be renamed"));
        }

        let absolute = resolve(root, path, false)?;
        if std::fs::symlink_metadata(&absolute).is_err() {
            return Err(invalid("the entry does not exist"));
        }
        let parent = absolute
            .parent()
            .ok_or_else(|| invalid("the entry does not exist"))?;
        let target = parent.join(name);
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(invalid("an entry with that name already exists"));
        }

        std::fs::rename(&absolute, &target)?;
        Ok(())
    }

    fn delete(&self, root: &Path, path: &str) -> Result<()> {
        if path.is_empty() {
            return Err(invalid("the server root cannot be deleted"));
        }

        let absolute = resolve(root, path, false)?;
        let metadata = std::fs::symlink_metadata(&absolute)
            .map_err(|_| invalid("the entry does not exist"))?;

        if metadata.file_type().is_symlink() || metadata.is_file() {
            std::fs::remove_file(&absolute)?;
            return Ok(());
        }
        if metadata.is_dir() {
            std::fs::remove_dir_all(&absolute)?;
            return Ok(());
        }

        Err(invalid("the entry is not a file, directory or link"))
    }

    fn zip(&self, root: &Path, path: &str) -> Result<()> {
        if path.is_empty() {
            return Err(invalid("choose a file or directory to pack"));
        }

        let absolute = resolve(root, path, true)?;
        let metadata = std::fs::symlink_metadata(&absolute)
            .map_err(|_| invalid("the entry does not exist"))?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("symbolic links cannot be packed"));
        }

        let parent = absolute
            .parent()
            .ok_or_else(|| invalid("the entry does not exist"))?;
        let name = absolute
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid("the entry name is not valid UTF-8"))?;
        let target = parent.join(format!("{name}.zip"));
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(invalid("an archive with that name already exists"));
        }

        let result = write_zip(&absolute, &target, name);
        if result.is_err() {
            let _ = std::fs::remove_file(&target);
        }
        let _ = result?;
        Ok(())
    }

    fn unzip(&self, root: &Path, path: &str) -> Result<()> {
        if path.is_empty() {
            return Err(invalid("choose an archive to unpack"));
        }

        let absolute = resolve(root, path, true)?;
        let metadata = std::fs::symlink_metadata(&absolute)
            .map_err(|_| invalid("the archive does not exist"))?;
        if metadata.is_dir() {
            return Err(invalid("the path is a directory"));
        }

        let parent = absolute
            .parent()
            .ok_or_else(|| invalid("the archive does not exist"))?;
        let temporary = parent.join(format!(".cp-unzip-{}", unique_suffix()));
        std::fs::create_dir(&temporary)?;
        set_mode(&temporary, 0o700)?;

        let result = (|| -> Result<()> {
            extract_zip(&absolute, &temporary)?;

            // Kontrollera ALLA namn innan något flyttas, så en konflikt
            // inte lämnar en halvt uppackad katalog.
            let mut names = Vec::new();
            for entry in std::fs::read_dir(&temporary)? {
                let entry = entry?;
                if std::fs::symlink_metadata(parent.join(entry.file_name())).is_ok() {
                    return Err(invalid("an entry with that name already exists"));
                }
                names.push(entry.file_name());
            }
            for name in names {
                std::fs::rename(temporary.join(&name), parent.join(&name))?;
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&temporary);
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload(
        &self,
        payload: &Value,
        root: &Path,
        path: &str,
        api: &MasterClient,
        node_id: &str,
        command_id: &str,
        lease_token: &str,
    ) -> Result<Value> {
        use tokio::io::AsyncWriteExt;

        let name = payload_name(payload)?;
        let overwrite = payload
            .get("overwrite")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid("upload has no overwrite flag"))?;
        let upload = payload
            .get("upload")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("upload metadata is missing"))?;
        let expected_bytes = upload
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("upload size is invalid"))?;
        let expected_sha = upload
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|value| is_sha256(value))
            .ok_or_else(|| invalid("upload digest is invalid"))?;
        if expected_bytes > self.max_upload_bytes {
            return Err(invalid("the upload is larger than the upload limit"));
        }

        let directory = existing_directory(root, path)?;
        let target = directory.join(&name);
        if std::fs::symlink_metadata(&target).is_ok() {
            if !overwrite {
                return Err(invalid("an entry with that name already exists"));
            }
            if target.is_dir() {
                return Err(invalid("the name belongs to a directory"));
            }
        }

        let temporary = directory.join(format!(".cp-upload-{}", unique_suffix()));
        let result = async {
            let mut response = api.fetch_upload(node_id, command_id, lease_token).await?;
            let mut file = tokio::fs::File::create(&temporary).await?;
            let mut hasher = Sha256::new();
            let mut received: u64 = 0;

            while let Some(chunk) = response.chunk().await? {
                received += chunk.len() as u64;
                if received > expected_bytes {
                    return Err(invalid("the upload exceeded its declared size"));
                }
                hasher.update(&chunk);
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            file.sync_all().await?;
            drop(file);

            if received != expected_bytes {
                return Err(invalid("the upload size does not match the metadata"));
            }
            let digest = hex(&hasher.finalize());
            if digest != expected_sha {
                return Err(invalid("the upload digest does not match the metadata"));
            }
            set_mode(&temporary, 0o644)?;
            tokio::fs::rename(&temporary, &target).await?;
            Ok(Value::Null)
        }
        .await;

        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;

        Ok(json!({
            "operation": "upload",
            "ok": true,
            "bytes": expected_bytes,
            "sha256": expected_sha,
        }))
    }

    /// Roten är alltid en riktig katalog under data-roten — aldrig en
    /// symlink som pekar någon annanstans.
    fn root_for(&self, server_id: &str) -> Result<PathBuf> {
        let root = std::fs::canonicalize(&self.data_root)?;
        let directory = root.join(server_id);
        std::fs::create_dir_all(&directory)?;
        if std::fs::symlink_metadata(&directory)?
            .file_type()
            .is_symlink()
        {
            return Err(invalid("the server data directory is a symbolic link"));
        }
        let resolved = std::fs::canonicalize(&directory)?;
        if !resolved.starts_with(&root) {
            return Err(invalid("the server data directory escaped the root"));
        }
        Ok(resolved)
    }
}

fn existing_directory(root: &Path, path: &str) -> Result<PathBuf> {
    let directory = resolve(root, path, true)?;
    if !directory.exists() {
        return Err(invalid("the parent directory does not exist"));
    }
    if !directory.is_dir() {
        return Err(invalid("the parent path is not a directory"));
    }
    Ok(directory)
}

fn create_entry(path: &Path, directory: bool) -> Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(invalid("an entry with that name already exists"));
    }
    if directory {
        std::fs::create_dir(path)?;
        set_mode(path, 0o755)?;
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        set_mode(path, 0o644)?;
    }
    Ok(())
}

fn write_zip(source: &Path, target: &Path, name: &str) -> Result<u64> {
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut writer = ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut counts = ZipCounts::default();

    add_to_zip(&mut writer, source, name, options, &mut counts)?;
    writer
        .finish()
        .map_err(|_| invalid("the archive could not be written"))?
        .flush()?;
    Ok(counts.files)
}

#[derive(Default)]
struct ZipCounts {
    files: u64,
    bytes: u64,
}

fn add_to_zip(
    writer: &mut ZipWriter<std::fs::File>,
    source: &Path,
    entry: &str,
    options: SimpleFileOptions,
    counts: &mut ZipCounts,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    if metadata.is_dir() {
        writer
            .add_directory(format!("{entry}/"), options)
            .map_err(|_| invalid("the archive could not be written"))?;
        if entry.matches('/').count() > MAX_WALK_DEPTH {
            return Err(invalid("the directory tree is too deep"));
        }
        for child in std::fs::read_dir(source)? {
            let child = child?;
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| invalid("the entry name is not valid UTF-8"))?;
            if !is_safe_name(&name) {
                return Err(invalid("the directory contains an unsafe entry name"));
            }
            add_to_zip(
                writer,
                &child.path(),
                &format!("{entry}/{name}"),
                options,
                counts,
            )?;
        }
        return Ok(());
    }

    counts.files += 1;
    counts.bytes = counts.bytes.saturating_add(metadata.len());
    if counts.files > MAX_ZIP_FILES || counts.bytes > MAX_ZIP_BYTES {
        return Err(invalid("the archive exceeds the size or file limit"));
    }

    let mut file = std::fs::File::open(source)?;
    writer
        .start_file(entry, options)
        .map_err(|_| invalid("the archive could not be written"))?;
    std::io::copy(&mut file, writer)?;
    Ok(())
}

/// `relative` → normaliserade segment. Allt som masterns `ServerPath`
/// avvisar avvisas här också, innan filsystemet rörs.
fn normalize_segments(relative: &str) -> Result<Vec<String>> {
    if relative.contains('\0') || relative.chars().any(char::is_control) {
        return Err(invalid("the path contains control characters"));
    }
    if relative.starts_with('/') || is_windows_absolute(relative) {
        return Err(invalid("absolute paths are not allowed"));
    }
    if relative.len() > MAX_LENGTH {
        return Err(invalid("the path is too long"));
    }

    let mut segments = Vec::new();
    for segment in relative.replace('\\', "/").split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(invalid("the path escapes the server directory"));
        }
        if segment.len() > MAX_SEGMENT_BYTES {
            return Err(invalid("a path segment is too long"));
        }
        segments.push(segment.to_string());
    }
    if segments.len() > MAX_DEPTH {
        return Err(invalid("the path is too deep"));
    }

    Ok(segments)
}

fn normalize(relative: &str) -> Result<String> {
    Ok(normalize_segments(relative)?.join("/"))
}

fn is_windows_absolute(relative: &str) -> bool {
    let bytes = relative.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Lexikal kandidat + kontroll av närmast existerande föregångare, precis
/// som masterns `ServerPath::resolve`.
fn resolve(root: &Path, relative: &str, follow_final: bool) -> Result<PathBuf> {
    let segments = normalize_segments(relative)?;
    let mut candidate = root.to_path_buf();
    for segment in &segments {
        candidate.push(segment);
    }

    let mut probe = if follow_final || segments.is_empty() {
        candidate.clone()
    } else {
        candidate
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf())
    };

    while !probe.exists() && std::fs::symlink_metadata(&probe).is_err() {
        match probe.parent() {
            Some(parent) if parent.starts_with(root) => probe = parent.to_path_buf(),
            _ => break,
        }
    }
    if !probe.starts_with(root) {
        return Err(invalid("the path escapes the server directory"));
    }

    let canonical_root = std::fs::canonicalize(root)?;
    match std::fs::canonicalize(&probe) {
        Ok(real) if real.starts_with(&canonical_root) => {}
        Ok(_) => return Err(invalid("the path escapes the server directory")),
        Err(_) => {
            // Trasig symlink eller saknad post: föräldern avgör.
            let parent = probe
                .parent()
                .ok_or_else(|| invalid("the path escapes the server directory"))?;
            let real = std::fs::canonicalize(parent)
                .map_err(|_| invalid("the path escapes the server directory"))?;
            if !real.starts_with(&canonical_root) {
                return Err(invalid("the path escapes the server directory"));
            }
        }
    }

    Ok(candidate)
}

fn payload_path(payload: &Value) -> Result<String> {
    let raw = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the file command has no path"))?;
    normalize(raw)
}

fn payload_name(payload: &Value) -> Result<String> {
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the file mutation has no name"))?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.chars().any(char::is_control)
        || name.len() > MAX_SEGMENT_BYTES
    {
        return Err(invalid("the entry name is invalid"));
    }
    Ok(name.to_string())
}

fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && name.len() <= MAX_SEGMENT_BYTES
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn parent_of(path: &str) -> Option<String> {
    path.rsplit_once('/').map(|(parent, _)| parent.to_string())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

fn iso8601(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3600,
        (remainder % 3600) / 60,
        remainder % 60
    )
}

/// Howard Hinnants algoritm: dagar sedan 1970-01-01 → (år, månad, dag).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(mode);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn invalid(message: &str) -> AgentError {
    AgentError::Runtime(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sandbox() -> (PathBuf, Files) {
        let dir = std::env::temp_dir().join(format!("craftpilot-files-{}", unique_suffix()));
        std::fs::create_dir_all(dir.join("root")).unwrap();
        let files = Files::new(dir.join("root"), 1024 * 1024, 8 * 1024 * 1024);
        (dir, files)
    }

    fn server_root(dir: &Path) -> PathBuf {
        let root = dir.join("root").join(server_id());
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn server_id() -> &'static str {
        "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    }

    fn payload(path: &str) -> Value {
        json!({ "server_id": server_id(), "path": path })
    }

    #[test]
    fn normalizes_and_rejects_unsafe_paths() {
        assert_eq!(normalize("").unwrap(), "");
        assert_eq!(normalize("world/level.dat").unwrap(), "world/level.dat");
        assert_eq!(normalize("world\\\\level.dat").unwrap(), "world/level.dat");
        assert!(normalize("/world//level.dat").is_err());
        assert!(normalize("../etc/passwd").is_err());
        assert!(normalize("world/../../etc").is_err());
        assert!(normalize("C:/Windows").is_err());
        assert!(normalize("bad\u{0}name").is_err());
        assert!(normalize("a\nb").is_err());
        assert!(normalize(&"a/".repeat(MAX_DEPTH + 1)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn follows_no_symlink_out_of_the_root() {
        let (dir, files) = sandbox();
        let root = server_root(&dir);
        std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();

        assert!(resolve(&root, "escape/passwd", true).is_err());
        assert!(resolve(&root, "escape", true).is_err());
        // Länken själv får hanteras (namnbyte/radering).
        assert!(resolve(&root, "escape", false).is_ok());
        assert!(files.read(&payload("escape/passwd")).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lists_reads_and_writes_files() {
        let (dir, files) = sandbox();
        let root = server_root(&dir);
        std::fs::write(root.join("server.properties"), "motd=hej\n").unwrap();
        std::fs::create_dir_all(root.join("world")).unwrap();

        let listing = files.list(&payload("")).unwrap();
        assert_eq!(listing["path"], "");
        assert_eq!(listing["parent"], Value::Null);
        let names: Vec<&str> = listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["server.properties", "world"]);
        assert_eq!(listing["entries"][1]["size"], Value::Null);

        let read = files.read(&payload("server.properties")).unwrap();
        assert_eq!(read["content"], "motd=hej\n");
        assert_eq!(read["name"], "server.properties");

        let write = json!({
            "server_id": server_id(),
            "path": "world/level.dat",
            "operation": "write",
            "content": "level",
        });
        futures_block_on(files.mutate(
            &write,
            &MasterClient::new(url::Url::parse("https://runtime-tls").unwrap()).unwrap(),
            "node",
            "command",
            &"a".repeat(64),
        ));
        assert_eq!(
            std::fs::read_to_string(root.join("world/level.dat")).unwrap(),
            "level"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Bara för de synkrona mutationerna i testerna.
    fn futures_block_on(value: impl std::future::Future<Output = Result<Value>>) -> Value {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(value)
            .unwrap()
    }

    #[test]
    fn creates_renames_and_deletes_entries() {
        let (dir, files) = sandbox();
        let root = server_root(&dir);
        let master = MasterClient::new(url::Url::parse("https://runtime-tls").unwrap()).unwrap();
        let run = |payload: Value| {
            futures_block_on(files.mutate(&payload, &master, "node", "command", &"a".repeat(64)))
        };

        run(json!({"server_id": server_id(), "path": "", "operation": "mkdir", "name": "plugins"}));
        assert!(root.join("plugins").is_dir());

        run(
            json!({"server_id": server_id(), "path": "plugins", "operation": "touch", "name": "config.yml"}),
        );
        assert!(root.join("plugins/config.yml").is_file());

        run(
            json!({"server_id": server_id(), "path": "plugins/config.yml", "operation": "rename", "name": "config.old.yml"}),
        );
        assert!(root.join("plugins/config.old.yml").is_file());

        run(json!({"server_id": server_id(), "path": "plugins", "operation": "delete"}));
        assert!(!root.join("plugins").exists());

        // Roten får aldrig raderas eller byta namn.
        assert!(files.delete(&root, "").is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zips_and_unzips_within_the_root() {
        let (dir, files) = sandbox();
        let root = server_root(&dir);
        std::fs::create_dir_all(root.join("world")).unwrap();
        std::fs::write(root.join("world/level.dat"), "level").unwrap();

        files.zip(&root, "world").unwrap();
        assert!(root.join("world.zip").is_file());

        std::fs::remove_dir_all(root.join("world")).unwrap();
        files.unzip(&root, "world.zip").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("world/level.dat")).unwrap(),
            "level"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unzip_refuses_a_traversing_archive() {
        let (dir, files) = sandbox();
        let root = server_root(&dir);
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        writer.start_file("../escaped.txt", options).unwrap();
        writer.write_all(b"nope").unwrap();
        let archive = writer.finish().unwrap().into_inner();
        std::fs::write(root.join("evil.zip"), archive).unwrap();

        assert!(files.unzip(&root, "evil.zip").is_err());
        assert!(!dir.join("root").join("escaped.txt").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn formats_timestamps_as_iso8601() {
        assert_eq!(
            iso8601(UNIX_EPOCH + std::time::Duration::from_secs(0)),
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(
            iso8601(UNIX_EPOCH + std::time::Duration::from_secs(1_793_079_200)),
            "2026-10-27T05:33:20Z"
        );
    }
}
