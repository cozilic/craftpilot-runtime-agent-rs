use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use hkdf::Hkdf;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::info;
use x25519_dalek::{x25519, X25519_BASEPOINT_BYTES};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::api::MasterClient;
use crate::docker::{is_uuid, server_id, Docker};
use crate::error::{AgentError, Result};
use crate::model::Command;
use crate::snapshot::{apply_shared_permissions, extract_zip};

/// Samma protokoll och gränser som masterns
/// `DefaultHostRuntimeNodeMigrationAction`.
const PROTOCOL: &str = "craftpilot-server-migration:v1";
const CHUNK_BYTES: usize = 1_048_576;
const MAX_CHUNKS: u64 = 4096;
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILES: u64 = 50_000;
const MAX_WALK_DEPTH: usize = 64;

pub struct Migrations {
    data_root: PathBuf,
    state_dir: PathBuf,
    data_gid: u32,
}

pub struct TransferOutcome {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ExportMarker {
    sha256: String,
    bytes: u64,
    files: u64,
}

struct Manifest {
    archive_bytes: u64,
    chunk_count: u64,
    archive_sha256: String,
    source_public_key: [u8; 32],
    salt: [u8; 32],
    nonce_prefix: [u8; 4],
}

impl Migrations {
    pub fn new(data_root: PathBuf, state_dir: PathBuf, data_gid: u32) -> Self {
        Self {
            data_root,
            state_dir,
            data_gid,
        }
    }

    /// Steg 1: destinationen skapar ett X25519-nyckelpar. Den privata
    /// nyckeln stannar på noden (0600) tills commit är klar; bara den
    /// publika nyckeln returneras till mastern i acken.
    pub fn prepare(&self, payload: &Value, node_id: &str) -> Result<String> {
        let ids = ids(payload)?;
        if ids.destination_node_id != node_id {
            return Err(invalid("the prepare command targets another node"));
        }

        let mut secret = [0u8; 32];
        fill_random(&mut secret)?;
        let public = x25519(secret, X25519_BASEPOINT_BYTES);
        self.write_key(&ids.migration_id, &secret)?;
        info!(migration_id = %ids.migration_id, "migration receiver key created");

        Ok(BASE64.encode(public))
    }

    /// Steg 3 (source): stoppa källan är redan gjort, arkivera, kryptera
    /// och relay:a chunks till mastern.
    pub async fn export(
        &self,
        payload: &Value,
        node_id: &str,
        docker: &Docker,
        api: &MasterClient,
        command: &Command,
    ) -> Result<TransferOutcome> {
        let ids = ids(payload)?;
        if ids.source_node_id != node_id {
            return Err(invalid("the export command targets another node"));
        }
        let receiver_public = decode_key(required(payload, "destination_public_key")?, 32)?;

        let root = self.server_root(&ids.server_id)?;
        if docker.is_running(&ids.server_id).await? {
            return Err(invalid("the server must be offline before export"));
        }

        let staging = self.staging_path(&ids.migration_id)?;
        reset_staging(&staging)?;
        let archive = staging.join("archive.zip");

        let (bytes, files, sha256) = {
            let root = root.clone();
            let archive = archive.clone();
            tokio::task::spawn_blocking(move || archive_server(&root, &archive))
                .await
                .map_err(|_| invalid("migration archive task failed"))??
        };
        if bytes == 0 || bytes > MAX_ARCHIVE_BYTES || files > MAX_FILES {
            return Err(invalid("the migration archive exceeds its limits"));
        }
        let chunk_count = bytes.div_ceil(CHUNK_BYTES as u64);
        if chunk_count > MAX_CHUNKS {
            return Err(invalid("the migration archive needs too many chunks"));
        }

        let mut ephemeral = [0u8; 32];
        fill_random(&mut ephemeral)?;
        let public = x25519(ephemeral, X25519_BASEPOINT_BYTES);
        let mut salt = [0u8; 32];
        fill_random(&mut salt)?;
        let mut nonce_prefix = [0u8; 4];
        fill_random(&mut nonce_prefix)?;
        let shared = x25519(ephemeral, receiver_public);
        let key = derive_key(
            &shared,
            &salt,
            &ids.migration_id,
            &ids.source_node_id,
            &ids.destination_node_id,
        )?;

        let manifest = json!({
            "protocol": 1,
            "source_public_key": BASE64.encode(public),
            "salt": BASE64.encode(salt),
            "nonce_prefix": BASE64.encode(nonce_prefix),
            "chunk_bytes": CHUNK_BYTES,
            "chunk_count": chunk_count,
            "archive_bytes": bytes,
            "archive_sha256": sha256,
            "files": files,
            "server_id": ids.server_id,
            "source_node_id": ids.source_node_id,
            "destination_node_id": ids.destination_node_id,
        });
        api.put_migration_manifest(
            node_id,
            &ids.migration_id,
            &command.id,
            &command.lease_token,
            &manifest,
        )
        .await?;

        let mut input = std::fs::File::open(&archive)?;
        let mut buffer = vec![0u8; CHUNK_BYTES];
        for index in 0..chunk_count {
            let plain = read_chunk(&mut input, &mut buffer)?;
            let expected = expected_plain_bytes(bytes, index);
            if plain.len() != expected {
                return Err(invalid("the migration archive ended unexpectedly"));
            }
            let nonce = chunk_nonce(&nonce_prefix, index);
            let aad = chunk_aad(
                &ids.migration_id,
                &ids.source_node_id,
                &ids.destination_node_id,
                index,
                chunk_count,
            );
            let ciphertext = encrypt_chunk(&key, &nonce, aad.as_bytes(), plain)?;
            api.put_migration_chunk(
                node_id,
                &ids.migration_id,
                &command.id,
                &command.lease_token,
                index,
                &ciphertext,
            )
            .await?;
        }
        if !read_chunk(&mut input, &mut buffer)?.is_empty() {
            return Err(invalid("the migration archive changed during export"));
        }
        drop(input);

        self.write_export(&ids.migration_id, &sha256, bytes, files)?;
        let _ = std::fs::remove_dir_all(&staging);
        info!(
            migration_id = %ids.migration_id,
            bytes,
            files,
            chunks = chunk_count,
            "migration_exported"
        );

        Ok(TransferOutcome { sha256, bytes })
    }

    /// Steg 5 (destination): hämta och dekryptera alla chunks, verifiera
    /// arkivet, extrahera till en privat staging-katalog och lämna ett
    /// verifierat märke som commit kräver.
    pub async fn import(
        &self,
        payload: &Value,
        node_id: &str,
        api: &MasterClient,
        command: &Command,
    ) -> Result<TransferOutcome> {
        let ids = ids(payload)?;
        if ids.destination_node_id != node_id {
            return Err(invalid("the import command targets another node"));
        }
        let secret = self
            .read_key(&ids.migration_id)?
            .ok_or_else(|| invalid("the migration receiver key is missing"))?;

        let manifest = api
            .get_migration_manifest(
                node_id,
                &ids.migration_id,
                &command.id,
                &command.lease_token,
            )
            .await?;
        let manifest = Manifest::parse(&manifest, payload)?;

        let shared = x25519(secret, manifest.source_public_key);
        let key = derive_key(
            &shared,
            &manifest.salt,
            &ids.migration_id,
            &ids.source_node_id,
            &ids.destination_node_id,
        )?;

        let staging = self.staging_path(&ids.migration_id)?;
        reset_staging(&staging)?;
        let archive = staging.join("archive.zip");
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive)?;
        let mut hasher = Sha256::new();
        let mut received: u64 = 0;

        for index in 0..manifest.chunk_count {
            let ciphertext = api
                .get_migration_chunk(
                    node_id,
                    &ids.migration_id,
                    &command.id,
                    &command.lease_token,
                    index,
                )
                .await?;
            let expected = expected_plain_bytes(manifest.archive_bytes, index);
            if ciphertext.len() != expected + 16 {
                return Err(invalid("a relayed migration chunk has an invalid size"));
            }
            let nonce = chunk_nonce(&manifest.nonce_prefix, index);
            let aad = chunk_aad(
                &ids.migration_id,
                &ids.source_node_id,
                &ids.destination_node_id,
                index,
                manifest.chunk_count,
            );
            let plain = decrypt_chunk(&key, &nonce, aad.as_bytes(), &ciphertext)?;
            if plain.len() != expected {
                return Err(invalid("a migration chunk decrypted to the wrong size"));
            }
            received += plain.len() as u64;
            hasher.update(&plain);
            output.write_all(&plain)?;
        }
        output.flush()?;
        output.sync_all()?;
        drop(output);

        let digest = hex(&hasher.finalize());
        if received != manifest.archive_bytes || digest != manifest.archive_sha256 {
            return Err(invalid("the decrypted archive does not match the manifest"));
        }

        let payload_dir = staging.join("payload");
        std::fs::create_dir(&payload_dir)?;
        let extraction = tokio::task::spawn_blocking({
            let archive = archive.clone();
            let payload_dir = payload_dir.clone();
            move || extract_zip(&archive, &payload_dir)
        })
        .await
        .map_err(|_| invalid("migration extraction task failed"))??;
        apply_shared_permissions(&payload_dir, self.data_gid);
        let _ = std::fs::remove_file(&archive);

        let marker = json!({
            "migration_id": ids.migration_id,
            "server_id": ids.server_id,
            "sha256": digest,
            "archive_bytes": received,
            "files": extraction,
        });
        self.write_marker(&ids.migration_id, &marker)?;
        info!(
            migration_id = %ids.migration_id,
            bytes = received,
            files = extraction,
            "migration_imported"
        );

        Ok(TransferOutcome {
            sha256: digest,
            bytes: received,
        })
    }

    /// Steg 6: mastern har verifierat digesten och ber destinationen
    /// installera datan. Först här rörs serverns katalog.
    pub fn commit(&self, payload: &Value, node_id: &str) -> Result<()> {
        let ids = ids(payload)?;
        if ids.destination_node_id != node_id {
            return Err(invalid("the commit command targets another node"));
        }
        let expected_sha = required(payload, "archive_sha256")?.to_string();
        let marker = self
            .read_marker(&ids.migration_id)?
            .ok_or_else(|| invalid("the migration staging was not verified"))?;
        if marker["migration_id"].as_str() != Some(ids.migration_id.as_str())
            || marker["server_id"].as_str() != Some(ids.server_id.as_str())
            || marker["sha256"].as_str() != Some(expected_sha.as_str())
        {
            return Err(invalid("the migration staging does not match this commit"));
        }

        let staging = self.staging_path(&ids.migration_id)?;
        let payload_dir = staging.join("payload");
        if payload_dir.is_symlink() || !payload_dir.is_dir() {
            return Err(invalid("the verified migration payload is missing"));
        }

        let root = self.canonical_root()?;
        let target = root.join(&ids.server_id);
        if target.is_symlink() {
            return Err(invalid("the server data directory is a symbolic link"));
        }
        if target.is_dir() {
            if std::fs::read_dir(&target)?.next().is_some() {
                return Err(invalid("the server data directory is not empty"));
            }
            std::fs::remove_dir(&target)?;
        }
        std::fs::rename(&payload_dir, &target)?;
        apply_shared_permissions(&target, self.data_gid);
        let _ = std::fs::remove_dir_all(&staging);
        self.clear_key(&ids.migration_id)?;
        info!(migration_id = %ids.migration_id, server_id = %ids.server_id, "migration_committed");
        Ok(())
    }

    /// Steg 7 (source): radera källans data först efter att mastern
    /// uttryckligen auktoriserat det och samma arkiv som exporterades finns
    /// kvar i märket.
    pub fn delete_source(&self, payload: &Value, node_id: &str) -> Result<()> {
        let ids = ids(payload)?;
        if ids.source_node_id != node_id {
            return Err(invalid("the cleanup command targets another node"));
        }
        let expected_sha = required(payload, "archive_sha256")?.to_string();
        let marker = self
            .read_export(&ids.migration_id)?
            .ok_or_else(|| invalid("this node has no export for the migration"))?;
        if marker.sha256 != expected_sha {
            return Err(invalid(
                "the cleanup digest does not match this node's export",
            ));
        }

        let root = self.canonical_root()?;
        let target = root.join(&ids.server_id);
        if target.is_symlink() {
            return Err(invalid("the server data directory is a symbolic link"));
        }
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        self.clear_export(&ids.migration_id)?;
        info!(migration_id = %ids.migration_id, server_id = %ids.server_id, "migration_source_deleted");
        Ok(())
    }

    fn canonical_root(&self) -> Result<PathBuf> {
        let root = std::fs::canonicalize(&self.data_root)?;
        Ok(root)
    }

    fn server_root(&self, server_id: &str) -> Result<PathBuf> {
        let root = self.canonical_root()?;
        let directory = root.join(server_id);
        let metadata = std::fs::symlink_metadata(&directory)
            .map_err(|_| invalid("the server data directory does not exist"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid("the server data directory is unsafe"));
        }
        let resolved = std::fs::canonicalize(&directory)?;
        if !resolved.starts_with(&root) {
            return Err(invalid("the server data directory escaped the root"));
        }
        Ok(resolved)
    }

    fn staging_path(&self, migration_id: &str) -> Result<PathBuf> {
        if !is_uuid(migration_id) {
            return Err(invalid("the migration id is invalid"));
        }
        Ok(self
            .canonical_root()?
            .join(".craftpilot-migrations")
            .join(migration_id))
    }

    fn migration_dir(&self) -> PathBuf {
        self.state_dir.join("migrations")
    }

    fn write_key(&self, migration_id: &str, secret: &[u8; 32]) -> Result<()> {
        let path = self.key_path(migration_id)?;
        write_private(&path, BASE64.encode(secret).as_bytes())
    }

    fn read_key(&self, migration_id: &str) -> Result<Option<[u8; 32]>> {
        let path = self.key_path(migration_id)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(decode_key(
                std::str::from_utf8(&bytes)
                    .map_err(|_| invalid("the stored migration key is corrupt"))?
                    .trim(),
                32,
            )?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn clear_key(&self, migration_id: &str) -> Result<()> {
        let _ = std::fs::remove_file(self.key_path(migration_id)?);
        Ok(())
    }

    fn key_path(&self, migration_id: &str) -> Result<PathBuf> {
        if !is_uuid(migration_id) {
            return Err(invalid("the migration id is invalid"));
        }
        Ok(self.migration_dir().join(format!("{migration_id}.key")))
    }

    fn write_export(&self, migration_id: &str, sha256: &str, bytes: u64, files: u64) -> Result<()> {
        let marker = ExportMarker {
            sha256: sha256.to_string(),
            bytes,
            files,
        };
        let encoded = serde_json::to_vec(&marker)?;
        write_private(&self.export_path(migration_id)?, &encoded)
    }

    fn read_export(&self, migration_id: &str) -> Result<Option<ExportMarker>> {
        match std::fs::read(self.export_path(migration_id)?) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn clear_export(&self, migration_id: &str) -> Result<()> {
        let _ = std::fs::remove_file(self.export_path(migration_id)?);
        Ok(())
    }

    fn export_path(&self, migration_id: &str) -> Result<PathBuf> {
        if !is_uuid(migration_id) {
            return Err(invalid("the migration id is invalid"));
        }
        Ok(self
            .migration_dir()
            .join(format!("{migration_id}.export.json")))
    }

    fn write_marker(&self, migration_id: &str, marker: &Value) -> Result<()> {
        let staging = self.staging_path(migration_id)?;
        let encoded = serde_json::to_vec(marker)?;
        write_private(&staging.join("verified.json"), &encoded)
    }

    fn read_marker(&self, migration_id: &str) -> Result<Option<Value>> {
        let path = self.staging_path(migration_id)?.join("verified.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

impl Manifest {
    fn parse(manifest: &Value, payload: &Value) -> Result<Self> {
        let ids = ids(payload)?;
        if manifest["protocol"].as_u64() != Some(1)
            || manifest["chunk_bytes"].as_u64() != Some(CHUNK_BYTES as u64)
            || manifest["server_id"].as_str() != Some(ids.server_id.as_str())
            || manifest["source_node_id"].as_str() != Some(ids.source_node_id.as_str())
            || manifest["destination_node_id"].as_str() != Some(ids.destination_node_id.as_str())
        {
            return Err(invalid(
                "the migration manifest does not match this transfer",
            ));
        }

        let archive_bytes = manifest["archive_bytes"]
            .as_u64()
            .filter(|bytes| *bytes > 0 && *bytes <= MAX_ARCHIVE_BYTES)
            .ok_or_else(|| invalid("the migration manifest has an invalid size"))?;
        let chunk_count = manifest["chunk_count"]
            .as_u64()
            .filter(|count| {
                *count > 0
                    && *count <= MAX_CHUNKS
                    && *count == archive_bytes.div_ceil(CHUNK_BYTES as u64)
            })
            .ok_or_else(|| invalid("the migration manifest has an invalid chunk count"))?;
        manifest["files"]
            .as_u64()
            .filter(|files| *files <= MAX_FILES)
            .ok_or_else(|| invalid("the migration manifest has an invalid file count"))?;
        let archive_sha256 = manifest["archive_sha256"]
            .as_str()
            .filter(|value| is_sha256(value))
            .ok_or_else(|| invalid("the migration manifest has an invalid digest"))?
            .to_string();

        Ok(Self {
            archive_bytes,
            chunk_count,
            archive_sha256,
            source_public_key: decode_key(required(manifest, "source_public_key")?, 32)?,
            salt: decode_key(required(manifest, "salt")?, 32)?,
            nonce_prefix: decode_key(required(manifest, "nonce_prefix")?, 4)?,
        })
    }
}

struct Ids {
    server_id: String,
    migration_id: String,
    source_node_id: String,
    destination_node_id: String,
}

fn ids(payload: &Value) -> Result<Ids> {
    let value = |key: &str| -> Result<String> {
        let raw = required(payload, key)?;
        if !is_uuid(raw) {
            return Err(invalid(&format!("{key} is not a uuid")));
        }
        Ok(raw.to_string())
    };

    Ok(Ids {
        server_id: server_id(payload)?,
        migration_id: value("migration_id")?,
        source_node_id: value("source_node_id")?,
        destination_node_id: value("destination_node_id")?,
    })
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("the migration payload is missing {key}")))
}

fn expected_plain_bytes(archive_bytes: u64, index: u64) -> usize {
    let offset = index * CHUNK_BYTES as u64;
    (archive_bytes - offset).min(CHUNK_BYTES as u64) as usize
}

fn chunk_nonce(prefix: &[u8; 4], index: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(prefix);
    nonce[4..8].copy_from_slice(&((index >> 32) as u32).to_be_bytes());
    nonce[8..].copy_from_slice(&((index & 0xffff_ffff) as u32).to_be_bytes());
    nonce
}

fn chunk_aad(
    migration_id: &str,
    source_node_id: &str,
    destination_node_id: &str,
    index: u64,
    chunk_count: u64,
) -> String {
    format!(
        "{PROTOCOL}|{migration_id}|{source_node_id}|{destination_node_id}|{index}|{chunk_count}"
    )
}

fn derive_key(
    shared: &[u8; 32],
    salt: &[u8; 32],
    migration_id: &str,
    source_node_id: &str,
    destination_node_id: &str,
) -> Result<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), shared);
    let info = format!("{PROTOCOL}|{migration_id}|{source_node_id}|{destination_node_id}");
    let mut key = [0u8; 32];
    hkdf.expand(info.as_bytes(), &mut key)
        .map_err(|_| invalid("the migration key could not be derived"))?;
    Ok(key)
}

fn encrypt_chunk(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| invalid("the migration cipher is invalid"))?;
    cipher
        .encrypt(Nonce::from_slice(nonce), Payload { msg: plain, aad })
        .map_err(|_| invalid("a migration chunk could not be encrypted"))
}

fn decrypt_chunk(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| invalid("the migration cipher is invalid"))?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| invalid("a migration chunk failed authentication"))
}

fn read_chunk<'a>(input: &mut std::fs::File, buffer: &'a mut [u8]) -> Result<&'a [u8]> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = input.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(&buffer[..filled])
}

fn reset_staging(path: &Path) -> Result<()> {
    if path.is_symlink() {
        return Err(invalid("the migration staging path is a symbolic link"));
    }
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    std::fs::create_dir_all(path)?;
    set_mode(path, 0o700)?;
    Ok(())
}

/// Arkiverar serverroten utan omslutande katalog, precis som masterns
/// export: länkar och specialfiler avbryter hela operationen.
fn archive_server(root: &Path, archive: &Path) -> Result<(u64, u64, String)> {
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(archive)?;
    let mut writer = ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut files: u64 = 0;
    let mut bytes: u64 = 0;

    walk(root, "", &mut |relative: &str,
                         path: &Path,
                         is_dir: bool| {
        if is_dir {
            writer
                .add_directory(format!("{relative}/"), options)
                .map_err(|_| invalid("a migration directory could not be archived"))?;
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(invalid("the server data contains a link or special file"));
        }
        files += 1;
        bytes = bytes.saturating_add(metadata.len());
        if files > MAX_FILES || bytes > MAX_ARCHIVE_BYTES {
            return Err(invalid("the server data exceeds migration limits"));
        }
        writer
            .start_file(relative, options)
            .map_err(|_| invalid("a migration file could not be archived"))?;
        let mut file = std::fs::File::open(path)?;
        std::io::copy(&mut file, &mut writer)?;
        Ok(())
    })?;
    writer
        .finish()
        .map_err(|_| invalid("the migration archive could not be finalized"))?
        .flush()?;

    let archive_bytes = std::fs::metadata(archive)?.len();
    if archive_bytes == 0 || archive_bytes > MAX_ARCHIVE_BYTES {
        return Err(invalid("the migration archive has an invalid size"));
    }
    let mut file = std::fs::File::open(archive)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;

    Ok((archive_bytes, files, hex(&hasher.finalize())))
}

fn walk<F>(directory: &Path, prefix: &str, visit: &mut F) -> Result<()>
where
    F: FnMut(&str, &Path, bool) -> Result<()>,
{
    if prefix.matches('/').count() > MAX_WALK_DEPTH {
        return Err(invalid("the server data tree is too deep"));
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("the server data contains a non-UTF-8 name"))?;
        if name.contains('/') || name.contains('\\') || name.chars().any(char::is_control) {
            return Err(invalid("the server data contains an unsafe name"));
        }
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("the server data contains a symbolic link"));
        }
        if metadata.is_dir() {
            visit(&relative, &entry.path(), true)?;
            walk(&entry.path(), &relative, visit)?;
        } else if metadata.is_file() {
            visit(&relative, &entry.path(), false)?;
        } else {
            return Err(invalid("the server data contains a special file"));
        }
    }
    Ok(())
}

fn fill_random(buffer: &mut [u8]) -> Result<()> {
    getrandom::getrandom(buffer).map_err(|_| invalid("no secure randomness is available"))
}

fn decode_key<const N: usize>(encoded: &str, bytes: usize) -> Result<[u8; N]> {
    let decoded = BASE64
        .decode(encoded)
        .map_err(|_| invalid("the migration key material is invalid"))?;
    if decoded.len() != bytes || decoded.len() != N {
        return Err(invalid("the migration key material is invalid"));
    }
    let mut value = [0u8; N];
    value.copy_from_slice(&decoded);
    Ok(value)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f'))
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, contents)?;
    set_mode(&temporary, 0o600)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
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

    const MIGRATION: &str = "d5243ff4-f3d8-46db-bdff-1edd3c055b80";
    const SOURCE: &str = "c7365453-5ff6-4c40-a198-c85befd26c95";
    const DEST: &str = "af1bdd10-56b5-46fc-a1b2-2959ab713b0f";
    const SERVER: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    #[test]
    fn derives_the_same_key_on_both_sides() {
        let mut alice = [1u8; 32];
        let mut bob = [2u8; 32];
        alice[0] = 7;
        bob[0] = 9;
        let alice_public = x25519(alice, X25519_BASEPOINT_BYTES);
        let bob_public = x25519(bob, X25519_BASEPOINT_BYTES);
        let salt = [3u8; 32];

        let from_alice =
            derive_key(&x25519(alice, bob_public), &salt, MIGRATION, SOURCE, DEST).unwrap();
        let from_bob =
            derive_key(&x25519(bob, alice_public), &salt, MIGRATION, SOURCE, DEST).unwrap();
        assert_eq!(from_alice, from_bob);
        assert_ne!(
            from_alice,
            derive_key(&x25519(alice, bob_public), &salt, MIGRATION, DEST, SOURCE).unwrap()
        );
    }

    #[test]
    fn encrypts_and_authenticates_each_chunk() {
        let key = [4u8; 32];
        let prefix = [5u8, 6, 7, 8];
        let index = 4_294_967_301u64;
        let nonce = chunk_nonce(&prefix, index);
        assert_eq!(&nonce[..4], &prefix);
        assert_eq!(&nonce[4..8], &1u32.to_be_bytes());
        assert_eq!(&nonce[8..], &5u32.to_be_bytes());

        let aad = chunk_aad(MIGRATION, SOURCE, DEST, index, 9);
        assert_eq!(
            aad,
            format!("{PROTOCOL}|{MIGRATION}|{SOURCE}|{DEST}|{index}|9")
        );

        let plain = b"level-data".to_vec();
        let ciphertext = encrypt_chunk(&key, &nonce, aad.as_bytes(), &plain).unwrap();
        assert_eq!(ciphertext.len(), plain.len() + 16);
        assert_eq!(
            decrypt_chunk(&key, &nonce, aad.as_bytes(), &ciphertext).unwrap(),
            plain
        );

        // Fel AAD (annan chunk) får aldrig autentisera.
        let other = chunk_aad(MIGRATION, SOURCE, DEST, index + 1, 9);
        assert!(decrypt_chunk(&key, &nonce, other.as_bytes(), &ciphertext).is_err());
    }

    fn manifest() -> Value {
        json!({
            "protocol": 1,
            "source_public_key": BASE64.encode([9u8; 32]),
            "salt": BASE64.encode([8u8; 32]),
            "nonce_prefix": BASE64.encode([1u8; 4]),
            "chunk_bytes": CHUNK_BYTES,
            "chunk_count": 2,
            "archive_bytes": CHUNK_BYTES + 10,
            "archive_sha256": "a".repeat(64),
            "files": 3,
            "server_id": SERVER,
            "source_node_id": SOURCE,
            "destination_node_id": DEST,
        })
    }

    fn payload() -> Value {
        json!({
            "server_id": SERVER,
            "migration_id": MIGRATION,
            "source_node_id": SOURCE,
            "destination_node_id": DEST,
            "archive_sha256": "a".repeat(64),
        })
    }

    #[test]
    fn accepts_a_well_formed_manifest() {
        let manifest = Manifest::parse(&manifest(), &payload()).unwrap();
        assert_eq!(manifest.chunk_count, 2);
        assert_eq!(manifest.archive_bytes, CHUNK_BYTES as u64 + 10);
    }

    #[test]
    fn rejects_a_manifest_for_another_transfer() {
        let mut broken = manifest();
        broken["destination_node_id"] = json!("00000000-0000-4000-8000-000000000999");
        assert!(Manifest::parse(&broken, &payload()).is_err());

        let mut broken = manifest();
        broken["chunk_count"] = json!(3);
        assert!(Manifest::parse(&broken, &payload()).is_err());

        let mut broken = manifest();
        broken["archive_sha256"] = json!("not-a-digest");
        assert!(Manifest::parse(&broken, &payload()).is_err());

        let mut broken = manifest();
        broken["chunk_bytes"] = json!(4096);
        assert!(Manifest::parse(&broken, &payload()).is_err());
    }

    #[test]
    fn rejects_payloads_without_uuids() {
        let mut broken = payload();
        broken["migration_id"] = json!("../../etc");
        assert!(ids(&broken).is_err());
    }

    #[test]
    fn computes_expected_plain_chunk_sizes() {
        assert_eq!(
            expected_plain_bytes(CHUNK_BYTES as u64 + 10, 0),
            CHUNK_BYTES
        );
        assert_eq!(expected_plain_bytes(CHUNK_BYTES as u64 + 10, 1), 10);
    }
}
