//! Bounded, content-addressed observation artifacts.
//!
//! Artifacts are addressed by an opaque artifact_id, never by a path supplied
//! by a client. A transfer is written to a session-local temporary file,
//! verified against its declared size/hash and kind-specific limits, and only
//! then atomically renamed to its published file.

use super::events::now_ms;
use gpui_dev_protocol::{ARTIFACT_CHUNK_BYTES, ArtifactKind, ArtifactManifest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub const MAX_PNG_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_PNG_PIXELS: u64 = 20 * 1024 * 1024;
pub const MAX_TREE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_TREE_NODES: usize = 50_000;
pub const DEFAULT_SESSION_QUOTA_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_PROJECT_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const DEFAULT_MAX_ACTIVE_TRANSFERS_PER_RUN: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactErrorCode {
    InvalidRequest,
    Busy,
    QuotaExceeded,
    NotFound,
    Expired,
    InvalidState,
    TransferMismatch,
    ChunkTooLarge,
    InvalidOffset,
    SizeExceeded,
    ChecksumMismatch,
    ValidationFailed,
    DestinationExists,
    UnsafePath,
    Io,
}

impl ArtifactErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Busy => "busy",
            Self::QuotaExceeded => "quota_exceeded",
            Self::NotFound => "artifact_not_found",
            Self::Expired => "artifact_expired",
            Self::InvalidState => "invalid_artifact_state",
            Self::TransferMismatch => "transfer_mismatch",
            Self::ChunkTooLarge => "chunk_too_large",
            Self::InvalidOffset => "invalid_chunk_offset",
            Self::SizeExceeded => "artifact_size_exceeded",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::ValidationFailed => "artifact_validation_failed",
            Self::DestinationExists => "destination_exists",
            Self::UnsafePath => "unsafe_path",
            Self::Io => "artifact_io",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactError {
    pub code: ArtifactErrorCode,
    pub message: String,
}

impl ArtifactError {
    fn new(code: ArtifactErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn io(context: &str, error: io::Error) -> Self {
        Self::new(ArtifactErrorCode::Io, format!("{context}: {error}"))
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Error for ArtifactError {}

pub type ArtifactResult<T> = Result<T, ArtifactError>;

#[derive(Clone, Debug)]
pub struct ArtifactLimits {
    pub session_quota_bytes: u64,
    pub project_quota_bytes: u64,
    pub ttl: Duration,
    pub max_active_transfers_per_run: usize,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            session_quota_bytes: DEFAULT_SESSION_QUOTA_BYTES,
            project_quota_bytes: DEFAULT_PROJECT_QUOTA_BYTES,
            ttl: DEFAULT_TTL,
            max_active_transfers_per_run: DEFAULT_MAX_ACTIVE_TRANSFERS_PER_RUN,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStatus {
    Receiving,
    Verified,
    Published,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactInfo {
    #[serde(flatten)]
    pub manifest: ArtifactManifest,
    pub status: ArtifactStatus,
    pub received_bytes: u64,
    pub pinned: bool,
    pub active_references: u32,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactChunk {
    pub artifact_id: String,
    pub offset: u64,
    pub data: Vec<u8>,
    pub eof: bool,
}

#[derive(Clone, Debug)]
struct ArtifactRecord {
    info: ArtifactInfo,
    part_path: PathBuf,
    data_path: PathBuf,
}

/// A session-scoped artifact catalog. The project quota is calculated across
/// all session directories below the same project root, so a new session does
/// not get a second project-sized allowance by construction.
pub struct ArtifactStore {
    artifact_root: PathBuf,
    session_dir: PathBuf,
    limits: ArtifactLimits,
    records: Mutex<BTreeMap<String, ArtifactRecord>>,
}

impl ArtifactStore {
    pub fn new(
        project_root: &Path,
        session_id: &str,
        limits: ArtifactLimits,
    ) -> ArtifactResult<Self> {
        validate_identifier(session_id, "session_id")?;
        let project_root = project_root
            .canonicalize()
            .map_err(|error| ArtifactError::io("resolving project root", error))?;
        let state_dir = project_root.join(".gpui");
        let artifact_root = project_root.join(".gpui").join("artifacts");
        let session_dir = artifact_root.join(session_id);
        ensure_store_directory(&state_dir)?;
        ensure_store_directory(&artifact_root)?;
        ensure_store_directory(&session_dir)?;
        set_private_permissions(&artifact_root)?;
        set_private_permissions(&session_dir)?;
        let store = Self {
            artifact_root,
            session_dir,
            limits,
            records: Mutex::new(BTreeMap::new()),
        };
        store.load_existing()?;
        Ok(store)
    }

    pub fn limits(&self) -> &ArtifactLimits {
        &self.limits
    }

    pub fn begin(&self, mut manifest: ArtifactManifest) -> ArtifactResult<ArtifactInfo> {
        normalize_manifest(&mut manifest)?;
        self.validate_declared_limits(&manifest)?;
        let _quota_lock = self.acquire_quota_lock()?;
        self.expire_stale_other_sessions(now_ms())?;
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        self.expire_stale_records(&mut records, now_ms())?;

        if let Some(existing) = records.get(&manifest.artifact_id) {
            if existing.info.manifest == manifest
                && matches!(
                    existing.info.status,
                    ArtifactStatus::Receiving | ArtifactStatus::Published
                )
            {
                return Ok(existing.info.clone());
            }
            if existing.info.status != ArtifactStatus::Expired {
                return Err(ArtifactError::new(
                    ArtifactErrorCode::InvalidRequest,
                    "artifact_id is already in use",
                ));
            }
        }

        let active_for_run = records
            .values()
            .filter(|record| {
                matches!(
                    record.info.status,
                    ArtifactStatus::Receiving | ArtifactStatus::Verified
                ) && record.info.manifest.run_id == manifest.run_id
            })
            .count();
        if active_for_run >= self.limits.max_active_transfers_per_run {
            return Err(ArtifactError::new(
                ArtifactErrorCode::Busy,
                "the run already has the maximum number of active artifact transfers",
            ));
        }

        let session_usage = records
            .values()
            .filter(|record| record.info.status != ArtifactStatus::Expired)
            .map(|record| record.info.manifest.declared_bytes)
            .sum::<u64>();
        if session_usage.saturating_add(manifest.declared_bytes) > self.limits.session_quota_bytes {
            return Err(ArtifactError::new(
                ArtifactErrorCode::QuotaExceeded,
                "session artifact quota exceeded",
            ));
        }
        let project_usage = self.project_reserved_bytes()?;
        if project_usage.saturating_add(manifest.declared_bytes) > self.limits.project_quota_bytes {
            return Err(ArtifactError::new(
                ArtifactErrorCode::QuotaExceeded,
                "project artifact quota exceeded",
            ));
        }

        if let Some(existing) = records.remove(&manifest.artifact_id) {
            let _ = remove_if_present(&existing.part_path);
            let _ = remove_if_present(&existing.data_path);
            let _ = remove_if_present(&self.manifest_path(&manifest.artifact_id));
        }

        let part_path = self.part_path(&manifest.artifact_id);
        let data_path = self.data_path(&manifest.artifact_id);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part_path)
            .map_err(|error| ArtifactError::io("creating artifact transfer", error))?;
        drop(file);

        let now = now_ms();
        let info = ArtifactInfo {
            manifest,
            status: ArtifactStatus::Receiving,
            received_bytes: 0,
            pinned: false,
            active_references: 0,
            created_at_ms: now,
            updated_at_ms: now,
        };
        if let Err(error) = self.persist_info(&info) {
            let _ = remove_if_present(&part_path);
            return Err(error);
        }
        records.insert(
            info.manifest.artifact_id.clone(),
            ArtifactRecord {
                info: info.clone(),
                part_path,
                data_path,
            },
        );
        Ok(info)
    }

    pub fn write_chunk(
        &self,
        artifact_id: &str,
        transfer_id: &str,
        offset: u64,
        data: &[u8],
    ) -> ArtifactResult<u64> {
        let _quota_lock = self.acquire_quota_lock()?;
        if data.is_empty() {
            return Err(ArtifactError::new(
                ArtifactErrorCode::InvalidRequest,
                "artifact chunks cannot be empty",
            ));
        }
        if data.len() > ARTIFACT_CHUNK_BYTES {
            return Err(ArtifactError::new(
                ArtifactErrorCode::ChunkTooLarge,
                format!("artifact chunks are limited to {ARTIFACT_CHUNK_BYTES} bytes"),
            ));
        }
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let record = records
            .get_mut(artifact_id)
            .ok_or_else(|| ArtifactError::new(ArtifactErrorCode::NotFound, "artifact not found"))?;
        ensure_transfer(record, transfer_id)?;
        if record.info.status != ArtifactStatus::Receiving {
            return Err(state_error(record.info.status));
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| ArtifactError::new(ArtifactErrorCode::SizeExceeded, "chunk overflow"))?;
        if offset != record.info.received_bytes {
            if offset < record.info.received_bytes && end <= record.info.received_bytes {
                let mut file = File::open(&record.part_path)
                    .map_err(|error| ArtifactError::io("opening artifact transfer", error))?;
                file.seek(SeekFrom::Start(offset))
                    .map_err(|error| ArtifactError::io("seeking artifact transfer", error))?;
                let mut existing = vec![0; data.len()];
                file.read_exact(&mut existing)
                    .map_err(|error| ArtifactError::io("reading artifact transfer", error))?;
                if existing == data {
                    return Ok(record.info.received_bytes);
                }
            }
            return Err(ArtifactError::new(
                ArtifactErrorCode::InvalidOffset,
                format!(
                    "expected offset {}, received {offset}",
                    record.info.received_bytes
                ),
            ));
        }
        if end > record.info.manifest.declared_bytes {
            return Err(ArtifactError::new(
                ArtifactErrorCode::SizeExceeded,
                "artifact chunk exceeds its declared size",
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .open(&record.part_path)
            .map_err(|error| ArtifactError::io("opening artifact transfer", error))?;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(data))
            .map_err(|error| ArtifactError::io("writing artifact chunk", error))?;
        record.info.received_bytes = end;
        record.info.updated_at_ms = now_ms();
        let info = record.info.clone();
        self.persist_info(&info)?;
        Ok(end)
    }

    pub fn finish(&self, artifact_id: &str, transfer_id: &str) -> ArtifactResult<ArtifactInfo> {
        let _quota_lock = self.acquire_quota_lock()?;
        let (part_path, data_path, manifest) = {
            let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
            let record = records.get(artifact_id).ok_or_else(|| {
                ArtifactError::new(ArtifactErrorCode::NotFound, "artifact not found")
            })?;
            ensure_transfer(record, transfer_id)?;
            if record.info.status == ArtifactStatus::Published {
                return Ok(record.info.clone());
            }
            if record.info.status != ArtifactStatus::Receiving {
                return Err(state_error(record.info.status));
            }
            if record.info.received_bytes != record.info.manifest.declared_bytes {
                return Err(ArtifactError::new(
                    ArtifactErrorCode::InvalidState,
                    format!(
                        "artifact is incomplete: received {} of {} bytes",
                        record.info.received_bytes, record.info.manifest.declared_bytes
                    ),
                ));
            }
            (
                record.part_path.clone(),
                record.data_path.clone(),
                record.info.manifest.clone(),
            )
        };

        let actual_hash = hash_file(&part_path)?;
        if actual_hash != manifest.sha256 {
            self.discard(artifact_id);
            return Err(ArtifactError::new(
                ArtifactErrorCode::ChecksumMismatch,
                format!("declared {}, computed {actual_hash}", manifest.sha256),
            ));
        }
        if let Err(error) = validate_content(&manifest.kind, manifest.declared_bytes, &part_path) {
            self.discard(artifact_id);
            return Err(error);
        }

        {
            let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
            let record = records.get_mut(artifact_id).ok_or_else(|| {
                ArtifactError::new(ArtifactErrorCode::NotFound, "artifact disappeared")
            })?;
            record.info.status = ArtifactStatus::Verified;
            record.info.updated_at_ms = now_ms();
            let info = record.info.clone();
            self.persist_info(&info)?;
        }
        if let Err(error) = fs::rename(&part_path, &data_path) {
            self.discard(artifact_id);
            return Err(ArtifactError::io("publishing artifact", error));
        }

        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let record = records.get_mut(artifact_id).ok_or_else(|| {
            ArtifactError::new(ArtifactErrorCode::NotFound, "artifact disappeared")
        })?;
        record.info.status = ArtifactStatus::Published;
        record.info.received_bytes = record.info.manifest.declared_bytes;
        record.info.updated_at_ms = now_ms();
        let info = record.info.clone();
        self.persist_info(&info)?;
        Ok(info)
    }

    pub fn abort(&self, artifact_id: &str, transfer_id: &str) -> ArtifactResult<()> {
        let _quota_lock = self.acquire_quota_lock()?;
        let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let record = records
            .get(artifact_id)
            .ok_or_else(|| ArtifactError::new(ArtifactErrorCode::NotFound, "artifact not found"))?;
        ensure_transfer(record, transfer_id)?;
        if record.info.status == ArtifactStatus::Published {
            return Err(ArtifactError::new(
                ArtifactErrorCode::InvalidState,
                "published artifacts cannot be aborted",
            ));
        }
        drop(records);
        self.discard(artifact_id);
        Ok(())
    }

    pub fn info(&self, artifact_id: &str) -> ArtifactResult<ArtifactInfo> {
        let _quota_lock = self.acquire_quota_lock()?;
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let record = records
            .get_mut(artifact_id)
            .ok_or_else(|| ArtifactError::new(ArtifactErrorCode::NotFound, "artifact not found"))?;
        if record.info.status == ArtifactStatus::Published && !is_regular_file(&record.data_path) {
            record.info.status = ArtifactStatus::Expired;
            record.info.received_bytes = 0;
            record.info.updated_at_ms = now_ms();
            let info = record.info.clone();
            self.persist_info(&info)?;
        }
        if record.info.status == ArtifactStatus::Expired {
            return Err(ArtifactError::new(
                ArtifactErrorCode::Expired,
                "artifact has expired",
            ));
        }
        Ok(record.info.clone())
    }

    pub fn read_chunk(
        &self,
        artifact_id: &str,
        offset: u64,
        length: usize,
    ) -> ArtifactResult<ArtifactChunk> {
        if length > ARTIFACT_CHUNK_BYTES {
            return Err(ArtifactError::new(
                ArtifactErrorCode::ChunkTooLarge,
                format!("artifact reads are limited to {ARTIFACT_CHUNK_BYTES} bytes"),
            ));
        }
        let (path, size) = {
            let _quota_lock = self.acquire_quota_lock()?;
            self.acquire_path(artifact_id)?
        };
        let result = (|| {
            if offset > size {
                return Err(ArtifactError::new(
                    ArtifactErrorCode::InvalidOffset,
                    "read offset exceeds artifact size",
                ));
            }
            let mut file = File::open(&path)
                .map_err(|error| ArtifactError::io("opening published artifact", error))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| ArtifactError::io("seeking published artifact", error))?;
            let remaining = size - offset;
            let to_read = remaining.min(length as u64) as usize;
            let mut data = vec![0; to_read];
            file.read_exact(&mut data)
                .map_err(|error| ArtifactError::io("reading published artifact", error))?;
            Ok(ArtifactChunk {
                artifact_id: artifact_id.to_owned(),
                offset,
                eof: offset + data.len() as u64 == size,
                data,
            })
        })();
        self.release(artifact_id);
        result
    }

    pub fn pin(&self, artifact_id: &str, pinned: bool) -> ArtifactResult<ArtifactInfo> {
        let _quota_lock = self.acquire_quota_lock()?;
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let record = records
            .get_mut(artifact_id)
            .ok_or_else(|| ArtifactError::new(ArtifactErrorCode::NotFound, "artifact not found"))?;
        if record.info.status != ArtifactStatus::Published {
            return Err(state_error(record.info.status));
        }
        record.info.pinned = pinned;
        record.info.updated_at_ms = now_ms();
        let info = record.info.clone();
        self.persist_info(&info)?;
        Ok(info)
    }

    pub fn retain(&self, artifact_id: &str) -> ArtifactResult<()> {
        let _quota_lock = self.acquire_quota_lock()?;
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let record = records
            .get_mut(artifact_id)
            .ok_or_else(|| ArtifactError::new(ArtifactErrorCode::NotFound, "artifact not found"))?;
        if record.info.status != ArtifactStatus::Published {
            return Err(state_error(record.info.status));
        }
        if !is_regular_file(&record.data_path) {
            record.info.status = ArtifactStatus::Expired;
            record.info.received_bytes = 0;
            record.info.updated_at_ms = now_ms();
            let info = record.info.clone();
            self.persist_info(&info)?;
            return Err(state_error(ArtifactStatus::Expired));
        }
        record.info.active_references = record.info.active_references.saturating_add(1);
        record.info.updated_at_ms = now_ms();
        let info = record.info.clone();
        self.persist_info(&info)?;
        Ok(())
    }

    pub fn release(&self, artifact_id: &str) {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = records.get_mut(artifact_id) {
            record.info.active_references = record.info.active_references.saturating_sub(1);
        }
    }

    pub fn expire(&self, now: u64) -> ArtifactResult<Vec<String>> {
        let _quota_lock = self.acquire_quota_lock()?;
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let mut expired = self.expire_stale_records(&mut records, now)?;
        drop(records);
        expired.extend(self.expire_stale_other_sessions(now)?);
        Ok(expired)
    }

    /// Copies a verified artifact to a caller-selected output using a sibling
    /// temporary file and rename. Existing output is rejected unless the
    /// caller explicitly opts into replacement.
    pub fn export(
        &self,
        artifact_id: &str,
        destination: &Path,
        overwrite: bool,
    ) -> ArtifactResult<()> {
        let destination_root = destination
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .and_then(|parent| destination.file_name().map(|name| parent.join(name)))
            .unwrap_or_else(|| destination.to_path_buf());
        if destination_root.starts_with(&self.artifact_root) {
            return Err(ArtifactError::new(
                ArtifactErrorCode::UnsafePath,
                "artifact output cannot be inside the artifact store",
            ));
        }
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(ArtifactError::new(
                ArtifactErrorCode::InvalidRequest,
                "artifact output parent does not exist",
            ));
        }
        if let Ok(metadata) = fs::symlink_metadata(destination) {
            if metadata.file_type().is_symlink() {
                return Err(ArtifactError::new(
                    ArtifactErrorCode::UnsafePath,
                    "artifact output cannot be a symbolic link",
                ));
            }
            if !overwrite {
                return Err(ArtifactError::new(
                    ArtifactErrorCode::DestinationExists,
                    "artifact output already exists",
                ));
            }
        }

        let (source, _) = {
            let _quota_lock = self.acquire_quota_lock()?;
            self.acquire_path(artifact_id)?
        };
        let result = (|| {
            let mut input = File::open(&source)
                .map_err(|error| ArtifactError::io("opening artifact for export", error))?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)
                .map_err(|error| ArtifactError::io("creating artifact export", error))?;
            io::copy(&mut input, temporary.as_file_mut())
                .map_err(|error| ArtifactError::io("copying artifact export", error))?;
            temporary
                .as_file_mut()
                .sync_all()
                .map_err(|error| ArtifactError::io("flushing artifact export", error))?;
            match fs::rename(temporary.path(), destination) {
                Ok(()) => Ok(()),
                Err(error) if overwrite && error.kind() == io::ErrorKind::AlreadyExists => {
                    // Windows does not replace an existing file with rename.
                    // The temporary file is complete before this explicit
                    // overwrite fallback removes the old destination.
                    fs::remove_file(destination)
                        .map_err(|error| ArtifactError::io("replacing artifact output", error))?;
                    fs::rename(temporary.path(), destination)
                        .map_err(|error| ArtifactError::io("publishing artifact output", error))
                }
                Err(error) => Err(ArtifactError::io("publishing artifact output", error)),
            }
        })();
        self.release(artifact_id);
        result
    }

    fn acquire_path(&self, artifact_id: &str) -> ArtifactResult<(PathBuf, u64)> {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let record = records
            .get_mut(artifact_id)
            .ok_or_else(|| ArtifactError::new(ArtifactErrorCode::NotFound, "artifact not found"))?;
        if record.info.status == ArtifactStatus::Expired {
            return Err(ArtifactError::new(
                ArtifactErrorCode::Expired,
                "artifact has expired",
            ));
        }
        if record.info.status != ArtifactStatus::Published {
            return Err(state_error(record.info.status));
        }
        if !is_regular_file(&record.data_path) {
            record.info.status = ArtifactStatus::Expired;
            record.info.received_bytes = 0;
            record.info.updated_at_ms = now_ms();
            let info = record.info.clone();
            self.persist_info(&info)?;
            return Err(state_error(ArtifactStatus::Expired));
        }
        record.info.active_references = record.info.active_references.saturating_add(1);
        record.info.updated_at_ms = now_ms();
        let info = record.info.clone();
        self.persist_info(&info)?;
        Ok((
            record.data_path.clone(),
            record.info.manifest.declared_bytes,
        ))
    }

    fn discard(&self, artifact_id: &str) {
        let record = self
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(artifact_id);
        if let Some(record) = record {
            let _ = remove_if_present(&record.part_path);
            let _ = remove_if_present(&record.data_path);
            let _ = remove_if_present(&self.manifest_path(artifact_id));
        }
    }

    fn validate_declared_limits(&self, manifest: &ArtifactManifest) -> ArtifactResult<()> {
        let limit = match manifest.kind {
            ArtifactKind::Png => MAX_PNG_BYTES,
            ArtifactKind::Tree => MAX_TREE_BYTES,
            ArtifactKind::Blob => self.limits.session_quota_bytes,
        };
        if manifest.declared_bytes > limit {
            return Err(ArtifactError::new(
                ArtifactErrorCode::SizeExceeded,
                format!("declared artifact size exceeds the {limit} byte kind limit"),
            ));
        }
        Ok(())
    }

    fn load_existing(&self) -> ArtifactResult<()> {
        let entries = fs::read_dir(&self.session_dir)
            .map_err(|error| ArtifactError::io("reading artifact directory", error))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("part") {
                remove_if_present(&path)?;
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(mut info) = serde_json::from_slice::<ArtifactInfo>(&bytes) else {
                let _ = remove_if_present(&path);
                continue;
            };
            if validate_identifier(&info.manifest.artifact_id, "artifact_id").is_err() {
                let _ = remove_if_present(&path);
                continue;
            }
            let part_path = self.part_path(&info.manifest.artifact_id);
            let data_path = self.data_path(&info.manifest.artifact_id);
            match info.status {
                ArtifactStatus::Receiving => {
                    let _ = remove_if_present(&part_path);
                    let _ = remove_if_present(&path);
                }
                ArtifactStatus::Verified => {
                    if is_regular_file(&data_path) {
                        info.status = ArtifactStatus::Published;
                        info.active_references = 0;
                        self.persist_info(&info)?;
                        self.insert_loaded(info, part_path, data_path);
                    } else {
                        let _ = remove_if_present(&part_path);
                        let _ = remove_if_present(&path);
                    }
                }
                ArtifactStatus::Published => {
                    if is_regular_file(&data_path) {
                        info.active_references = 0;
                        self.insert_loaded(info, part_path, data_path);
                    } else {
                        let _ = remove_if_present(&path);
                    }
                }
                ArtifactStatus::Expired => {
                    info.active_references = 0;
                    self.insert_loaded(info, part_path, data_path);
                }
            }
        }
        Ok(())
    }

    fn insert_loaded(&self, info: ArtifactInfo, part_path: PathBuf, data_path: PathBuf) {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                info.manifest.artifact_id.clone(),
                ArtifactRecord {
                    info,
                    part_path,
                    data_path,
                },
            );
    }

    fn project_reserved_bytes(&self) -> ArtifactResult<u64> {
        let mut total = 0u64;
        let sessions = fs::read_dir(&self.artifact_root)
            .map_err(|error| ArtifactError::io("reading project artifact quota", error))?;
        for session in sessions {
            let session = session
                .map_err(|error| ArtifactError::io("reading project artifact quota", error))?;
            let path = session.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| ArtifactError::io("checking project artifact quota", error))?;
            if !metadata.file_type().is_dir() {
                continue;
            }
            let entries = fs::read_dir(path)
                .map_err(|error| ArtifactError::io("reading project artifact quota", error))?;
            for entry in entries {
                let entry = entry
                    .map_err(|error| ArtifactError::io("reading project artifact quota", error))?;
                if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                    ArtifactError::io("checking artifact quota metadata", error)
                })?;
                if !metadata.file_type().is_file() {
                    return Err(ArtifactError::new(
                        ArtifactErrorCode::UnsafePath,
                        "artifact metadata path is not a regular file",
                    ));
                }
                let bytes = fs::read(entry.path())
                    .map_err(|error| ArtifactError::io("reading artifact quota record", error))?;
                let info = serde_json::from_slice::<ArtifactInfo>(&bytes).map_err(|error| {
                    ArtifactError::new(
                        ArtifactErrorCode::InvalidState,
                        format!("artifact quota metadata is corrupt: {error}"),
                    )
                })?;
                if info.status != ArtifactStatus::Expired {
                    total = total.saturating_add(info.manifest.declared_bytes);
                }
            }
        }
        Ok(total)
    }

    fn expire_stale_other_sessions(&self, now: u64) -> ArtifactResult<Vec<String>> {
        let age = self.limits.ttl.as_millis().try_into().unwrap_or(u64::MAX);
        let mut expired = Vec::new();
        let sessions = fs::read_dir(&self.artifact_root)
            .map_err(|error| ArtifactError::io("scanning project artifact retention", error))?;
        for session in sessions {
            let session = session
                .map_err(|error| ArtifactError::io("scanning project artifact retention", error))?;
            let session_dir = session.path();
            if session_dir == self.session_dir {
                continue;
            }
            let metadata = fs::symlink_metadata(&session_dir).map_err(|error| {
                ArtifactError::io("checking project artifact retention directory", error)
            })?;
            if !metadata.file_type().is_dir() {
                continue;
            }
            let session_id = session.file_name().to_string_lossy().into_owned();
            let entries = fs::read_dir(&session_dir)
                .map_err(|error| ArtifactError::io("scanning project artifact retention", error))?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    ArtifactError::io("scanning project artifact retention", error)
                })?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    ArtifactError::io("checking artifact retention metadata", error)
                })?;
                if !metadata.file_type().is_file() {
                    return Err(ArtifactError::new(
                        ArtifactErrorCode::UnsafePath,
                        "artifact retention metadata path is not a regular file",
                    ));
                }
                let bytes = fs::read(&path)
                    .map_err(|error| ArtifactError::io("reading artifact metadata", error))?;
                let mut info: ArtifactInfo = serde_json::from_slice(&bytes).map_err(|error| {
                    ArtifactError::new(
                        ArtifactErrorCode::InvalidState,
                        format!("artifact retention metadata is corrupt: {error}"),
                    )
                })?;
                if info.status == ArtifactStatus::Expired
                    || info.pinned
                    || info.active_references != 0
                    || now.saturating_sub(info.updated_at_ms) < age
                {
                    continue;
                }
                validate_identifier(&info.manifest.artifact_id, "artifact_id")?;
                let artifact_id = info.manifest.artifact_id.clone();
                remove_if_present(&session_dir.join(format!("{artifact_id}.part")))?;
                remove_if_present(&session_dir.join(format!("{artifact_id}.artifact")))?;
                info.status = ArtifactStatus::Expired;
                info.received_bytes = 0;
                info.updated_at_ms = now;
                persist_info_at(&session_dir, &info)?;
                expired.push(format!("{session_id}/{artifact_id}"));
            }
        }
        Ok(expired)
    }

    fn expire_stale_records(
        &self,
        records: &mut BTreeMap<String, ArtifactRecord>,
        now: u64,
    ) -> ArtifactResult<Vec<String>> {
        let age = self.limits.ttl.as_millis().try_into().unwrap_or(u64::MAX);
        let mut expired = Vec::new();
        for (artifact_id, record) in records.iter_mut() {
            if record.info.status == ArtifactStatus::Expired
                || record.info.pinned
                || record.info.active_references != 0
                || now.saturating_sub(record.info.updated_at_ms) < age
            {
                continue;
            }
            remove_if_present(&record.part_path)?;
            remove_if_present(&record.data_path)?;
            record.info.status = ArtifactStatus::Expired;
            record.info.received_bytes = 0;
            record.info.updated_at_ms = now;
            let info = record.info.clone();
            self.persist_info(&info)?;
            expired.push(artifact_id.clone());
        }
        Ok(expired)
    }

    fn acquire_quota_lock(&self) -> ArtifactResult<File> {
        let path = self.artifact_root.join(".quota.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| ArtifactError::io("opening project artifact quota lock", error))?;
        file.lock()
            .map_err(|error| ArtifactError::io("locking project artifact quota", error))?;
        Ok(file)
    }

    fn part_path(&self, artifact_id: &str) -> PathBuf {
        self.session_dir.join(format!("{artifact_id}.part"))
    }

    fn data_path(&self, artifact_id: &str) -> PathBuf {
        self.session_dir.join(format!("{artifact_id}.artifact"))
    }

    fn manifest_path(&self, artifact_id: &str) -> PathBuf {
        self.session_dir.join(format!("{artifact_id}.json"))
    }

    fn persist_info(&self, info: &ArtifactInfo) -> ArtifactResult<()> {
        persist_info_at(&self.session_dir, info)
    }
}

fn persist_info_at(directory: &Path, info: &ArtifactInfo) -> ArtifactResult<()> {
    let path = directory.join(format!("{}.json", info.manifest.artifact_id));
    let mut persisted = info.clone();
    persisted.active_references = 0;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| ArtifactError::io("creating artifact manifest", error))?;
    serde_json::to_writer(&mut temporary, &persisted).map_err(|error| {
        ArtifactError::new(
            ArtifactErrorCode::Io,
            format!("serializing artifact manifest: {error}"),
        )
    })?;
    temporary
        .write_all(b"\n")
        .and_then(|_| temporary.as_file_mut().sync_all())
        .map_err(|error| ArtifactError::io("flushing artifact manifest", error))?;
    temporary
        .persist(&path)
        .map_err(|error| ArtifactError::io("publishing artifact manifest", error.error))?;
    Ok(())
}

impl Drop for ArtifactStore {
    fn drop(&mut self) {
        let session_dir = self.session_dir.clone();
        let records = self.records.get_mut().unwrap_or_else(|e| e.into_inner());
        for record in records.values() {
            if record.info.status == ArtifactStatus::Receiving
                || (record.info.status == ArtifactStatus::Verified
                    && !is_regular_file(&record.data_path))
            {
                let _ = remove_if_present(&record.part_path);
                let _ = remove_if_present(
                    &session_dir.join(format!("{}.json", record.info.manifest.artifact_id)),
                );
            }
        }
    }
}

fn normalize_manifest(manifest: &mut ArtifactManifest) -> ArtifactResult<()> {
    validate_identifier(&manifest.artifact_id, "artifact_id")?;
    validate_identifier(&manifest.transfer_id, "transfer_id")?;
    if let Some(run_id) = &manifest.run_id {
        validate_identifier(run_id, "run_id")?;
    }
    if manifest.mime.is_empty()
        || manifest.mime.len() > 128
        || manifest.mime.bytes().any(|byte| byte < 0x20)
    {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidRequest,
            "mime must be a bounded printable value",
        ));
    }
    if manifest.declared_bytes == 0 {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidRequest,
            "artifacts must declare a positive size",
        ));
    }
    let hash = manifest
        .sha256
        .strip_prefix("sha256:")
        .unwrap_or(manifest.sha256.as_str());
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidRequest,
            "sha256 must contain exactly 64 hexadecimal digits",
        ));
    }
    manifest.sha256 = format!("sha256:{}", hash.to_ascii_lowercase());
    Ok(())
}

fn validate_identifier(value: &str, field: &str) -> ArtifactResult<()> {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidRequest,
            format!("{field} must be a bounded ASCII identifier"),
        ));
    }
    Ok(())
}

fn ensure_transfer(record: &ArtifactRecord, transfer_id: &str) -> ArtifactResult<()> {
    if record.info.manifest.transfer_id != transfer_id {
        return Err(ArtifactError::new(
            ArtifactErrorCode::TransferMismatch,
            "transfer_id does not own this artifact",
        ));
    }
    Ok(())
}

fn state_error(status: ArtifactStatus) -> ArtifactError {
    if status == ArtifactStatus::Expired {
        ArtifactError::new(ArtifactErrorCode::Expired, "artifact has expired")
    } else {
        ArtifactError::new(
            ArtifactErrorCode::InvalidState,
            format!("artifact is in the {status:?} state"),
        )
    }
}

fn remove_if_present(path: &Path) -> ArtifactResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ArtifactError::io("removing artifact file", error)),
    }
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn ensure_store_directory(path: &Path) -> ArtifactResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(ArtifactError::new(
            ArtifactErrorCode::UnsafePath,
            format!(
                "artifact store directory is not a real directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(ArtifactError::io(
                        "creating artifact store directory",
                        error,
                    ));
                }
            }
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| ArtifactError::io("checking artifact store directory", error))?;
            if metadata.file_type().is_dir() {
                Ok(())
            } else {
                Err(ArtifactError::new(
                    ArtifactErrorCode::UnsafePath,
                    "artifact store path changed while it was created",
                ))
            }
        }
        Err(error) => Err(ArtifactError::io(
            "checking artifact store directory",
            error,
        )),
    }
}

fn set_private_permissions(path: &Path) -> ArtifactResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            ArtifactError::io("restricting artifact directory permissions", error)
        })?;
    }
    let _ = path;
    Ok(())
}

fn hash_file(path: &Path) -> ArtifactResult<String> {
    let mut file = File::open(path)
        .map_err(|error| ArtifactError::io("opening artifact for hashing", error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let bytes = file
            .read(&mut buffer)
            .map_err(|error| ArtifactError::io("hashing artifact", error))?;
        if bytes == 0 {
            break;
        }
        hasher.update(&buffer[..bytes]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn validate_content(kind: &ArtifactKind, declared_bytes: u64, path: &Path) -> ArtifactResult<()> {
    match kind {
        ArtifactKind::Png => validate_png(declared_bytes, path),
        ArtifactKind::Tree => validate_tree(declared_bytes, path),
        ArtifactKind::Blob => Ok(()),
    }
}

fn validate_png(declared_bytes: u64, path: &Path) -> ArtifactResult<()> {
    if declared_bytes > MAX_PNG_BYTES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::SizeExceeded,
            "PNG exceeds the 16 MiB limit",
        ));
    }
    let mut file =
        File::open(path).map_err(|error| ArtifactError::io("opening PNG artifact", error))?;
    let mut header = [0; 33];
    file.read_exact(&mut header).map_err(|error| {
        ArtifactError::new(
            ArtifactErrorCode::ValidationFailed,
            format!("PNG header is incomplete: {error}"),
        )
    })?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n"
        || u32::from_be_bytes(header[8..12].try_into().unwrap()) != 13
        || &header[12..16] != b"IHDR"
    {
        return Err(ArtifactError::new(
            ArtifactErrorCode::ValidationFailed,
            "artifact is not a valid PNG with an IHDR header",
        ));
    }
    let width = u32::from_be_bytes(header[16..20].try_into().unwrap()) as u64;
    let height = u32::from_be_bytes(header[20..24].try_into().unwrap()) as u64;
    if width == 0 || height == 0 || width.saturating_mul(height) > MAX_PNG_PIXELS {
        return Err(ArtifactError::new(
            ArtifactErrorCode::ValidationFailed,
            "PNG dimensions exceed the 20 megapixel limit",
        ));
    }
    Ok(())
}

fn validate_tree(declared_bytes: u64, path: &Path) -> ArtifactResult<()> {
    if declared_bytes > MAX_TREE_BYTES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::SizeExceeded,
            "tree exceeds the 16 MiB limit",
        ));
    }
    let bytes =
        fs::read(path).map_err(|error| ArtifactError::io("reading tree artifact", error))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ArtifactError::new(
            ArtifactErrorCode::ValidationFailed,
            format!("tree is not valid JSON: {error}"),
        )
    })?;
    let mut nodes = 0;
    count_tree_nodes(&value, &mut nodes);
    if nodes > MAX_TREE_NODES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::ValidationFailed,
            format!("tree contains more than {MAX_TREE_NODES} nodes"),
        ));
    }
    Ok(())
}

fn count_tree_nodes(value: &Value, count: &mut usize) {
    match value {
        Value::Object(fields) => {
            *count = count.saturating_add(1);
            for child in fields.values() {
                count_tree_nodes(child, count);
            }
        }
        Value::Array(values) => {
            for child in values {
                count_tree_nodes(child, count);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, transfer: &str, kind: ArtifactKind, bytes: &[u8]) -> ArtifactManifest {
        let digest = Sha256::digest(bytes);
        ArtifactManifest {
            artifact_id: id.into(),
            transfer_id: transfer.into(),
            run_id: Some("r1".into()),
            kind,
            mime: "application/octet-stream".into(),
            declared_bytes: bytes.len() as u64,
            sha256: format!("sha256:{digest:x}"),
        }
    }

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let root = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(root.path(), "s1", ArtifactLimits::default()).unwrap();
        (root, store)
    }

    #[test]
    fn artifact_is_not_published_before_complete_verified_transfer() {
        let (_root, store) = store();
        let bytes = b"hello artifact";
        store
            .begin(manifest("a1", "t1", ArtifactKind::Blob, bytes))
            .unwrap();
        store.write_chunk("a1", "t1", 0, b"hello").unwrap();
        assert_eq!(store.info("a1").unwrap().status, ArtifactStatus::Receiving);
        assert!(matches!(
            store.read_chunk("a1", 0, 4),
            Err(ArtifactError {
                code: ArtifactErrorCode::InvalidState,
                ..
            })
        ));
        store.write_chunk("a1", "t1", 5, b" artifact").unwrap();
        let info = store.finish("a1", "t1").unwrap();
        assert_eq!(info.status, ArtifactStatus::Published);
        assert_eq!(store.read_chunk("a1", 0, 128).unwrap().data, bytes);
    }

    #[test]
    fn chunks_are_bounded_sequential_and_transfer_fenced() {
        let (_root, store) = store();
        let bytes = vec![7; ARTIFACT_CHUNK_BYTES + 1];
        let full = manifest("a2", "t2", ArtifactKind::Blob, &bytes);
        store.begin(full).unwrap();
        assert!(matches!(
            store.write_chunk("a2", "wrong", 0, &[1]),
            Err(ArtifactError {
                code: ArtifactErrorCode::TransferMismatch,
                ..
            })
        ));
        assert!(matches!(
            store.write_chunk("a2", "t2", 1, &[1]),
            Err(ArtifactError {
                code: ArtifactErrorCode::InvalidOffset,
                ..
            })
        ));
        assert!(matches!(
            store.write_chunk("a2", "t2", 0, &bytes),
            Err(ArtifactError {
                code: ArtifactErrorCode::ChunkTooLarge,
                ..
            })
        ));
        assert_eq!(store.write_chunk("a2", "t2", 0, &bytes[..4]).unwrap(), 4);
        assert_eq!(store.write_chunk("a2", "t2", 0, &bytes[..4]).unwrap(), 4);
        assert!(matches!(
            store.write_chunk("a2", "t2", 0, b"diff"),
            Err(ArtifactError {
                code: ArtifactErrorCode::InvalidOffset,
                ..
            })
        ));
    }

    #[test]
    fn checksum_and_kind_validation_reject_untrusted_content() {
        let (_root, store) = store();
        let bytes = b"not a png";
        store
            .begin(manifest("png", "tpng", ArtifactKind::Png, bytes))
            .unwrap();
        store.write_chunk("png", "tpng", 0, bytes).unwrap();
        assert!(matches!(
            store.finish("png", "tpng"),
            Err(ArtifactError {
                code: ArtifactErrorCode::ValidationFailed,
                ..
            })
        ));
        assert!(matches!(
            store.info("png"),
            Err(ArtifactError {
                code: ArtifactErrorCode::NotFound,
                ..
            })
        ));
    }

    #[test]
    fn checksum_mismatch_and_oversized_png_dimensions_are_rejected() {
        let (_root, store) = store();
        let bytes = b"payload with incorrect declared digest";
        let mut bad_hash = manifest("bad-hash", "t-hash", ArtifactKind::Blob, bytes);
        bad_hash.sha256 = format!("sha256:{}", "0".repeat(64));
        store.begin(bad_hash).unwrap();
        store.write_chunk("bad-hash", "t-hash", 0, bytes).unwrap();
        assert!(matches!(
            store.finish("bad-hash", "t-hash"),
            Err(ArtifactError {
                code: ArtifactErrorCode::ChecksumMismatch,
                ..
            })
        ));

        let mut png = vec![0; 33];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[8..12].copy_from_slice(&13u32.to_be_bytes());
        png[12..16].copy_from_slice(b"IHDR");
        png[16..20].copy_from_slice(&5000u32.to_be_bytes());
        png[20..24].copy_from_slice(&5000u32.to_be_bytes());
        let digest = Sha256::digest(&png);
        store
            .begin(ArtifactManifest {
                artifact_id: "large-png".into(),
                transfer_id: "t-png".into(),
                run_id: Some("r1".into()),
                kind: ArtifactKind::Png,
                mime: "image/png".into(),
                declared_bytes: png.len() as u64,
                sha256: format!("sha256:{digest:x}"),
            })
            .unwrap();
        store.write_chunk("large-png", "t-png", 0, &png).unwrap();
        assert!(matches!(
            store.finish("large-png", "t-png"),
            Err(ArtifactError {
                code: ArtifactErrorCode::ValidationFailed,
                ..
            })
        ));
    }

    #[test]
    fn tree_artifact_node_count_is_bounded_before_publication() {
        let (_root, store) = store();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "nodes": vec![serde_json::json!({"id": "node"}); MAX_TREE_NODES + 1]
        }))
        .unwrap();
        let digest = Sha256::digest(&bytes);
        store
            .begin(ArtifactManifest {
                artifact_id: "large-tree".into(),
                transfer_id: "t-tree".into(),
                run_id: Some("r1".into()),
                kind: ArtifactKind::Tree,
                mime: "application/json".into(),
                declared_bytes: bytes.len() as u64,
                sha256: format!("sha256:{digest:x}"),
            })
            .unwrap();
        for chunk in bytes.chunks(ARTIFACT_CHUNK_BYTES) {
            let offset = store.info("large-tree").unwrap().received_bytes;
            store
                .write_chunk("large-tree", "t-tree", offset, chunk)
                .unwrap();
        }
        assert!(matches!(
            store.finish("large-tree", "t-tree"),
            Err(ArtifactError {
                code: ArtifactErrorCode::ValidationFailed,
                ..
            })
        ));
    }

    #[test]
    fn tree_limit_and_project_quota_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let limits = ArtifactLimits {
            session_quota_bytes: 8,
            project_quota_bytes: 10,
            ..ArtifactLimits::default()
        };
        let first = ArtifactStore::new(root.path(), "s1", limits.clone()).unwrap();
        let first_bytes = b"123456";
        first
            .begin(manifest("a1", "t1", ArtifactKind::Blob, first_bytes))
            .unwrap();
        for (offset, chunk) in [(0, b"123".as_slice()), (3, b"456".as_slice())] {
            first.write_chunk("a1", "t1", offset, chunk).unwrap();
        }
        first.finish("a1", "t1").unwrap();
        let second = ArtifactStore::new(root.path(), "s2", limits).unwrap();
        assert!(matches!(
            second.begin(manifest("a2", "t2", ArtifactKind::Blob, b"12345")),
            Err(ArtifactError {
                code: ArtifactErrorCode::QuotaExceeded,
                ..
            })
        ));
    }

    #[test]
    fn project_quota_lock_serializes_reservations_across_sessions() {
        let root = tempfile::tempdir().unwrap();
        let limits = ArtifactLimits {
            session_quota_bytes: 8,
            project_quota_bytes: 8,
            ..ArtifactLimits::default()
        };
        let first =
            std::sync::Arc::new(ArtifactStore::new(root.path(), "s1", limits.clone()).unwrap());
        let second = std::sync::Arc::new(ArtifactStore::new(root.path(), "s2", limits).unwrap());
        let first_manifest = manifest("a1", "t1", ArtifactKind::Blob, b"123456");
        let second_manifest = manifest("a2", "t2", ArtifactKind::Blob, b"abcdef");
        let (first_result, second_result) = std::thread::scope(|scope| {
            let a = scope.spawn(|| first.begin(first_manifest));
            let b = scope.spawn(|| second.begin(second_manifest));
            (a.join().unwrap(), b.join().unwrap())
        });
        assert_ne!(first_result.is_ok(), second_result.is_ok());
        let rejected = match (first_result, second_result) {
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => error,
            (Ok(_), Ok(_)) => panic!("both reservations unexpectedly succeeded"),
            (Err(first), Err(_second)) => first,
        };
        assert_eq!(rejected.code, ArtifactErrorCode::QuotaExceeded);
    }

    #[test]
    fn stale_project_artifacts_expire_before_quota_reservation() {
        let root = tempfile::tempdir().unwrap();
        let limits = ArtifactLimits {
            session_quota_bytes: 8,
            project_quota_bytes: 8,
            ttl: Duration::from_millis(1),
            ..ArtifactLimits::default()
        };
        let old = ArtifactStore::new(root.path(), "old", limits.clone()).unwrap();
        let old_bytes = b"123456";
        old.begin(manifest("old1", "told", ArtifactKind::Blob, old_bytes))
            .unwrap();
        old.write_chunk("old1", "told", 0, old_bytes).unwrap();
        old.finish("old1", "told").unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let next = ArtifactStore::new(root.path(), "next", limits).unwrap();
        next.begin(manifest("new1", "tnew", ArtifactKind::Blob, b"abcdef"))
            .unwrap();
        assert!(matches!(
            old.info("old1"),
            Err(ArtifactError {
                code: ArtifactErrorCode::Expired,
                ..
            })
        ));
    }

    #[test]
    fn interruption_removes_receiving_transfer_on_restart() {
        let root = tempfile::tempdir().unwrap();
        {
            let store = ArtifactStore::new(root.path(), "s1", ArtifactLimits::default()).unwrap();
            let bytes = b"unfinished";
            store
                .begin(manifest("a3", "t3", ArtifactKind::Blob, bytes))
                .unwrap();
            store.write_chunk("a3", "t3", 0, b"un").unwrap();
        }
        let restarted = ArtifactStore::new(root.path(), "s1", ArtifactLimits::default()).unwrap();
        assert!(matches!(
            restarted.info("a3"),
            Err(ArtifactError {
                code: ArtifactErrorCode::NotFound,
                ..
            })
        ));
        assert!(!root.path().join(".gpui/artifacts/s1/a3.part").exists());
    }

    #[test]
    fn pin_and_active_reference_protect_expiration() {
        let (_root, store) = store();
        let bytes = b"keep me";
        store
            .begin(manifest("a4", "t4", ArtifactKind::Blob, bytes))
            .unwrap();
        store.write_chunk("a4", "t4", 0, bytes).unwrap();
        store.finish("a4", "t4").unwrap();
        store.retain("a4").unwrap();
        assert!(store.expire(u64::MAX).unwrap().is_empty());
        store.release("a4");
        store.pin("a4", true).unwrap();
        assert!(store.expire(u64::MAX).unwrap().is_empty());
        store.pin("a4", false).unwrap();
        assert_eq!(store.expire(u64::MAX).unwrap(), vec!["a4"]);
        assert!(matches!(
            store.read_chunk("a4", 0, 1),
            Err(ArtifactError {
                code: ArtifactErrorCode::Expired,
                ..
            })
        ));
    }

    #[test]
    fn export_requires_explicit_overwrite_and_rejects_store_paths() {
        let (root, store) = store();
        let bytes = b"exported";
        store
            .begin(manifest("a5", "t5", ArtifactKind::Blob, bytes))
            .unwrap();
        store.write_chunk("a5", "t5", 0, bytes).unwrap();
        store.finish("a5", "t5").unwrap();
        let output = root.path().join("out.bin");
        store.export("a5", &output, false).unwrap();
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert!(matches!(
            store.export("a5", &output, false),
            Err(ArtifactError {
                code: ArtifactErrorCode::DestinationExists,
                ..
            })
        ));
        assert!(matches!(
            store.export("a5", &root.path().join(".gpui/artifacts/s1/out.bin"), true,),
            Err(ArtifactError {
                code: ArtifactErrorCode::UnsafePath,
                ..
            })
        ));
    }
}
