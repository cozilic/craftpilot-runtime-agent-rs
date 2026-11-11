use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tracing::info;
use url::Url;

use crate::api::MasterClient;
use crate::docker::server_id;
use crate::error::{AgentError, Result};
use crate::model::Command;

/// Samma gränser som Python-agenten: 4 GiB okomprimerat, 50 000 filer,
/// 2 GiB per fil och max 200:1 kompressionsförhållande.
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILES: u64 = 50_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RATIO: u64 = 200;
const MAX_DEPTH: usize = 64;
const MAX_NAME_BYTES: usize = 4096;

/// Stegar masterns engångssnapshot: hämtar ZIP:en, verifierar storlek och
/// SHA-256, extraherar i en privat staging-katalog och byter sedan atomiskt
/// in katalogen på serverns plats. En icke-tom målkatalog rörs aldrig.
pub struct Snapshotter {
    data_root: PathBuf,
    master: Url,
    data_gid: u32,
}

pub struct SnapshotOutcome {
    pub sha256: String,
    pub bytes: u64,
}

impl Snapshotter {
    pub fn new(data_root: PathBuf, master: Url, data_gid: u32) -> Self {
        Self {
            data_root,
            master,
            data_gid,
        }
    }

    pub async fn stage(
        &self,
        node_id: &str,
        command: &Command,
        api: &MasterClient,
    ) -> Result<SnapshotOutcome> {
        let (server_id, expected_sha, expected_bytes) =
            parse_metadata(&command.payload, &command.id)?;
        let filename = format!("{server_id}-{}.zip", command.id);

        let raw_url = command
            .payload
            .get("download_url")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("snapshot command has no download url"))?;
        let url = self.download_url(raw_url, node_id, &command.id, &filename)?;

        let root = std::fs::canonicalize(&self.data_root)?;
        let destination = root.join(&server_id);
        ensure_destination(&destination)?;

        // Staging ligger i data-roten (samma filsystem) så bytet till den
        // slutliga katalogen kan göras med en atomisk rename.
        let staging = root.join(format!(".{server_id}-snapshot-{}", unique_suffix()));
        std::fs::create_dir(&staging)?;
        set_mode(&staging, 0o700)?;
        let extract_path = staging.join("extracted");
        std::fs::create_dir(&extract_path)?;

        let result = self
            .stage_inner(
                &staging,
                &extract_path,
                &destination,
                &url,
                &server_id,
                &expected_sha,
                expected_bytes,
                api,
            )
            .await;
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn stage_inner(
        &self,
        staging: &Path,
        extract_path: &Path,
        destination: &Path,
        url: &Url,
        server_id: &str,
        expected_sha: &str,
        expected_bytes: u64,
        api: &MasterClient,
    ) -> Result<SnapshotOutcome> {
        let archive_path = staging.join("snapshot.zip");
        let mut response = api.download(url).await?;
        let mut archive = tokio::fs::File::create(&archive_path).await?;
        let mut hasher = Sha256::new();
        let mut received: u64 = 0;

        while let Some(chunk) = response.chunk().await? {
            received += chunk.len() as u64;
            if received > expected_bytes {
                return Err(invalid("snapshot archive exceeded its declared size"));
            }
            hasher.update(&chunk);
            archive.write_all(&chunk).await?;
        }
        archive.flush().await?;
        archive.sync_all().await?;
        drop(archive);

        if received != expected_bytes {
            return Err(invalid(
                "snapshot archive size does not match the master metadata",
            ));
        }
        let digest = hex(&hasher.finalize());
        if digest != expected_sha {
            return Err(invalid(
                "snapshot archive SHA-256 does not match the master metadata",
            ));
        }

        let extraction = tokio::task::spawn_blocking({
            let archive_path = archive_path.clone();
            let extract_path = extract_path.to_path_buf();
            move || extract_zip(&archive_path, &extract_path)
        })
        .await
        .map_err(|_| invalid("snapshot extraction task failed"))??;

        ensure_destination(destination)?;
        if destination.exists() {
            std::fs::remove_dir(destination)?;
        }
        std::fs::rename(extract_path, destination)?;
        apply_shared_permissions(destination, self.data_gid);

        info!(
            server_id = %server_id,
            bytes = received,
            files = extraction,
            "server_snapshot_staged"
        );

        Ok(SnapshotOutcome {
            sha256: digest,
            bytes: received,
        })
    }

    /// Nedladdnings-URL:en måste peka på exakt den här nodens kommando hos
    /// den konfigurerade HTTPS-mastern — ingen annan värd, port, path,
    /// query eller userinfo accepteras.
    fn download_url(
        &self,
        raw: &str,
        node_id: &str,
        command_id: &str,
        filename: &str,
    ) -> Result<Url> {
        let url =
            Url::parse(raw).map_err(|_| invalid("snapshot download url is not a valid url"))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid("snapshot download url is not a plain HTTPS url"));
        }
        if url.origin() != self.master.origin() {
            return Err(invalid(
                "snapshot download url does not match the configured master",
            ));
        }
        let expected =
            format!("/api/runtime/nodes/{node_id}/commands/{command_id}/snapshot/{filename}");
        if url.path() != expected {
            return Err(invalid("snapshot download url does not match this command"));
        }
        Ok(url)
    }
}

/// `snapshot.filename` måste vara `<server>-<command>.zip`, och digesten
/// måste vara 64 gemena hextecken. Storleken är den mastern själv
/// verifierar acken mot.
fn parse_metadata(payload: &Value, command_id: &str) -> Result<(String, String, u64)> {
    let server_id = server_id(payload)?;
    let metadata = payload
        .get("snapshot")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("snapshot command metadata is invalid"))?;

    let filename = metadata
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if filename != format!("{server_id}-{command_id}.zip") {
        return Err(invalid("snapshot filename does not match the command"));
    }

    let sha256 = metadata
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_sha256(sha256) {
        return Err(invalid("snapshot digest is invalid"));
    }

    let bytes = metadata
        .get("bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("snapshot size is invalid"))?;
    if bytes == 0 || bytes > MAX_BYTES {
        return Err(invalid("snapshot size is invalid"));
    }

    Ok((server_id, sha256.to_string(), bytes))
}

fn ensure_destination(path: &Path) -> Result<()> {
    if path.is_symlink() {
        return Err(invalid("snapshot destination is a symbolic link"));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let mut entries = std::fs::read_dir(path)?;
            if entries.next().is_some() {
                return Err(invalid("snapshot destination must be missing or empty"));
            }
            Ok(())
        }
        Ok(_) => Err(invalid("snapshot destination is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// En ZIP-post får bara bli en vanlig fil eller katalog under
/// staging-roten: inga absoluta sökvägar, inga `..`, inga symlinks eller
/// specialfiler och inga kontrolltecken.
fn safe_entry(name: &str, unix_mode: Option<u32>) -> Result<PathBuf> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(invalid("snapshot contains an invalid entry name"));
    }
    let normalized = name.replace('\\', "/");
    if normalized
        .chars()
        .any(|ch| (ch as u32) < 0x20 || ch as u32 == 0x7f)
        || normalized.starts_with('/')
    {
        return Err(invalid("snapshot contains an unsafe entry path"));
    }
    if let Some(mode) = unix_mode {
        let kind = mode & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 {
            return Err(invalid("snapshot contains a symlink or special file"));
        }
    }

    let mut depth = 0usize;
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(invalid("snapshot contains a traversing entry path"));
        }
        depth += 1;
    }
    if depth == 0 || depth > MAX_DEPTH {
        return Err(invalid("snapshot contains an invalid entry path"));
    }

    Ok(PathBuf::from(normalized))
}

pub(crate) fn extract_zip(archive: &Path, target: &Path) -> Result<u64> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|_| invalid("snapshot archive is not a valid zip file"))?;
    if zip.is_empty() {
        return Err(invalid("snapshot archive is empty"));
    }
    if zip.len() as u64 > MAX_FILES {
        return Err(invalid("snapshot archive has too many entries"));
    }

    let mut total: u64 = 0;
    let mut files: u64 = 0;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|_| invalid("snapshot archive contains an unreadable entry"))?;

        let name = entry.name().to_string();
        let mode = entry.unix_mode();
        let is_directory = entry.is_dir();
        let size = entry.size();
        let compressed = entry.compressed_size();
        let relative = safe_entry(&name, mode)?;

        if is_directory {
            std::fs::create_dir_all(target.join(&relative))?;
            continue;
        }
        if size > MAX_FILE_BYTES {
            return Err(invalid("snapshot entry exceeds the file size limit"));
        }
        if compressed > 0 && size / compressed > MAX_RATIO {
            return Err(invalid(
                "snapshot archive exceeds the compression ratio limit",
            ));
        }
        total = total.saturating_add(size);
        if total > MAX_BYTES {
            return Err(invalid(
                "snapshot archive exceeds the extraction size limit",
            ));
        }
        files += 1;

        let output_path = target.join(&relative);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)?;
        std::io::copy(&mut entry, &mut output)?;
        output.flush()?;
        set_mode(&output_path, 0o660)?;
    }

    Ok(files)
}

/// En ny serverkatalog ska vara skrivbar för spelcontainerns grupp (33),
/// med setgid så filer som Minecraft skapar ärver gruppen.
pub(crate) fn apply_shared_permissions(root: &Path, gid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("chown")
            .arg("-R")
            .arg(format!(":{gid}"))
            .arg(root)
            .status();

        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                let _ = set_mode(&path, 0o2770);
                if let Ok(entries) = std::fs::read_dir(&path) {
                    for entry in entries.flatten() {
                        stack.push(entry.path());
                    }
                }
            } else {
                let _ = set_mode(&path, 0o660);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (root, gid);
    }
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

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f'))
}

fn invalid(message: &str) -> AgentError {
    AgentError::Runtime(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    const SERVER_ID: &str = "4f2b2d3a-fe50-42ff-8c61-af1493ecfd26";
    const COMMAND_ID: &str = "d5243ff4-f3d8-46db-bdff-1edd3c055b80";
    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn command_payload() -> Value {
        json!({
            "server_id": SERVER_ID,
            "snapshot": {
                "filename": format!("{SERVER_ID}-{COMMAND_ID}.zip"),
                "sha256": SHA,
                "bytes": 1024,
                "files": 1
            },
            "download_url": format!(
                "https://runtime-tls/api/runtime/nodes/node-1/commands/{COMMAND_ID}/snapshot/{SERVER_ID}-{COMMAND_ID}.zip"
            )
        })
    }

    fn snapshotter() -> Snapshotter {
        Snapshotter::new(
            PathBuf::from("/srv/craftpilot/servers"),
            Url::parse("https://runtime-tls").unwrap(),
            33,
        )
    }

    #[test]
    fn parses_the_master_metadata() {
        let (server_id, sha, bytes) = parse_metadata(&command_payload(), COMMAND_ID).unwrap();
        assert_eq!(server_id, SERVER_ID);
        assert_eq!(sha, SHA);
        assert_eq!(bytes, 1024);
    }

    #[test]
    fn rejects_a_foreign_filename_or_digest() {
        let mut payload = command_payload();
        payload["snapshot"]["filename"] = json!(format!("{SERVER_ID}-other.zip"));
        assert!(parse_metadata(&payload, COMMAND_ID).is_err());

        let mut payload = command_payload();
        payload["snapshot"]["sha256"] = json!("ABC");
        assert!(parse_metadata(&payload, COMMAND_ID).is_err());
    }

    #[test]
    fn rejects_an_empty_or_oversized_archive() {
        let mut payload = command_payload();
        payload["snapshot"]["bytes"] = json!(0);
        assert!(parse_metadata(&payload, COMMAND_ID).is_err());

        let mut payload = command_payload();
        payload["snapshot"]["bytes"] = json!(MAX_BYTES + 1);
        assert!(parse_metadata(&payload, COMMAND_ID).is_err());
    }

    #[test]
    fn accepts_only_the_command_own_download_url() {
        let filename = format!("{SERVER_ID}-{COMMAND_ID}.zip");
        let payload = command_payload();
        let url = payload["download_url"].as_str().unwrap();
        assert!(snapshotter()
            .download_url(url, "node-1", COMMAND_ID, &filename)
            .is_ok());
    }

    #[test]
    fn rejects_foreign_or_unsafe_download_urls() {
        let filename = format!("{SERVER_ID}-{COMMAND_ID}.zip");
        let cases = [
            format!("http://runtime-tls/api/runtime/nodes/node-1/commands/{COMMAND_ID}/snapshot/{filename}"),
            format!("https://evil.example/api/runtime/nodes/node-1/commands/{COMMAND_ID}/snapshot/{filename}"),
            format!("https://runtime-tls/api/runtime/nodes/node-2/commands/{COMMAND_ID}/snapshot/{filename}"),
            format!("https://runtime-tls/api/runtime/nodes/node-1/commands/{COMMAND_ID}/snapshot/{filename}?x=1"),
            format!("https://user:pass@runtime-tls/api/runtime/nodes/node-1/commands/{COMMAND_ID}/snapshot/{filename}"),
        ];
        for raw in cases {
            assert!(
                snapshotter()
                    .download_url(&raw, "node-1", COMMAND_ID, &filename)
                    .is_err(),
                "{raw} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_plain_relative_entries() {
        assert_eq!(
            safe_entry("world/level.dat", Some(0o100644)).unwrap(),
            PathBuf::from("world/level.dat")
        );
        assert_eq!(
            safe_entry("plugins/", Some(0o040755)).unwrap(),
            PathBuf::from("plugins/")
        );
        assert!(safe_entry("server.properties", None).is_ok());
    }

    #[test]
    fn rejects_traversal_absolute_and_control_character_entries() {
        for name in [
            "../evil",
            "world/../../evil",
            "/etc/passwd",
            "a\\..\\b",
            "bad\u{0}name",
        ] {
            assert!(
                safe_entry(name, None).is_err(),
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_symlinks_and_special_files() {
        assert!(safe_entry("link", Some(0o120777)).is_err());
        assert!(safe_entry("fifo", Some(0o010644)).is_err());
    }

    #[test]
    fn extracts_a_safe_archive() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .add_directory("world/", options)
            .expect("directory entry");
        writer.start_file("world/level.dat", options).expect("file");
        writer.write_all(b"level").expect("write");
        let archive_bytes = writer.finish().expect("finish").into_inner();

        let dir = std::env::temp_dir().join(format!(
            "craftpilot-snapshot-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let archive = dir.join("snapshot.zip");
        let target = dir.join("extracted");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(&archive, archive_bytes).unwrap();

        let files = extract_zip(&archive, &target).unwrap();
        assert_eq!(files, 1);
        assert_eq!(
            std::fs::read(target.join("world/level.dat")).unwrap(),
            b"level"
        );
        assert!(target.join("world").is_dir());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_an_empty_archive() {
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-snapshot-empty-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("snapshot.zip");
        let writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        writer.finish().unwrap();

        let target = dir.join("extracted");
        std::fs::create_dir_all(&target).unwrap();
        assert!(extract_zip(&archive, &target).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
