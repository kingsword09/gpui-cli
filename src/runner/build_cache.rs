//! BuildKey-scoped local artifact reuse primitives.

use super::build_key::BuildKey;
use super::build_manifest::BuildArtifactManifest;
use super::output_layout::{BUILD_OUTPUT_OWNER_FILE, BuildOutputLayout, BuildPlatform};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::TryLockError;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;

const CACHE_PLATFORMS: &[&str] = &["desktop", "android", "ios"];
pub const BUILD_OUTPUT_OWNER_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BuildOutputOwner {
    pub schema_version: u32,
    pub owner_id: String,
    pub pid: u32,
    pub started_at_ms: u64,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_hash: Option<String>,
}

/// A per-output-root advisory lock. The lock file is intentionally persistent:
/// the operating system releases the lock when the process exits or crashes.
pub struct BuildOutputLock {
    _file: File,
    path: PathBuf,
    owner_path: PathBuf,
    owner_id: String,
}

impl BuildOutputLock {
    /// Serializes builders and cache readers for one platform/BuildKey.
    /// Callers keep the guard through validation, build, and manifest publish.
    pub fn acquire(layout: &BuildOutputLayout) -> Result<Self> {
        fs::create_dir_all(&layout.root)
            .with_context(|| format!("creating BuildKey output root {}", layout.root.display()))?;
        let lock = Self::acquire_at_root_with_key(&layout.root, Some(layout.key_hash.clone()))?;
        layout
            .prepare()
            .with_context(|| format!("preparing locked BuildKey output {}", layout.key_hash))?;
        Ok(lock)
    }

    /// Acquires the persistent lock for an already planned BuildKey output
    /// root. Preview builders use this form because their source workspace is
    /// frozen elsewhere while outputs live under the source project.
    pub fn acquire_at_root(root: &Path) -> Result<Self> {
        Self::acquire_at_root_with_key(root, None)
    }

    fn acquire_at_root_with_key(root: &Path, key_hash: Option<String>) -> Result<Self> {
        fs::create_dir_all(root)
            .with_context(|| format!("creating BuildKey output root {}", root.display()))?;
        let path = root.join(super::output_layout::BUILD_OUTPUT_LOCK_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                bail!(
                    "BuildKey output lock is not a regular file: {}",
                    path.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("checking BuildKey output lock {}", path.display()));
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening BuildKey output lock {}", path.display()))?;
        file.lock()
            .with_context(|| format!("locking BuildKey output root {}", root.display()))?;
        Self::from_locked_file(root, path, file, key_hash)
    }

    fn from_locked_file(
        root: &Path,
        path: PathBuf,
        file: File,
        key_hash: Option<String>,
    ) -> Result<Self> {
        let owner_id = format!("{}-{}", process::id(), owner_timestamp_nanos());
        let owner_path = root.join(BUILD_OUTPUT_OWNER_FILE);
        let owner = BuildOutputOwner {
            schema_version: BUILD_OUTPUT_OWNER_SCHEMA_VERSION,
            owner_id: owner_id.clone(),
            pid: process::id(),
            started_at_ms: owner_timestamp_ms(),
            state: "building".into(),
            key_hash,
        };
        write_owner_record(&owner_path, &owner)?;
        Ok(Self {
            _file: file,
            path,
            owner_path,
            owner_id,
        })
    }

    /// Tries to become the coordinator leader for one BuildKey output root.
    ///
    /// This is deliberately non-blocking. A caller that observes `None` can
    /// subscribe to the coordinator record and wait for the current leader.
    pub fn try_acquire(layout: &BuildOutputLayout) -> Result<Option<Self>> {
        fs::create_dir_all(&layout.root)
            .with_context(|| format!("creating BuildKey output root {}", layout.root.display()))?;
        match Self::try_acquire_at(&layout.root, true, Some(layout.key_hash.clone()))? {
            LockAttempt::Acquired(lock) => {
                layout.prepare().with_context(|| {
                    format!("preparing locked BuildKey output {}", layout.key_hash)
                })?;
                Ok(Some(lock))
            }
            LockAttempt::Busy => Ok(None),
            LockAttempt::Missing => Ok(None),
        }
    }

    fn try_acquire_at(
        root: &Path,
        create_lock: bool,
        key_hash: Option<String>,
    ) -> Result<LockAttempt> {
        let lock_path = root.join(super::output_layout::BUILD_OUTPUT_LOCK_FILE);
        match fs::symlink_metadata(root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "BuildKey cache root is not a regular directory: {}",
                    root.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LockAttempt::Missing);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("checking BuildKey cache root {}", root.display()));
            }
        }
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                bail!(
                    "BuildKey output lock is not a regular file: {}",
                    lock_path.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create_lock => {
                return Ok(LockAttempt::Missing);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("checking BuildKey output lock {}", lock_path.display())
                });
            }
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create_lock)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("opening BuildKey output lock {}", lock_path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(LockAttempt::Acquired(Self::from_locked_file(
                root, lock_path, file, key_hash,
            )?)),
            Err(TryLockError::WouldBlock) => Ok(LockAttempt::Busy),
            Err(TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("trying BuildKey output lock {}", root.display())),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn owner_path(&self) -> &std::path::Path {
        &self.owner_path
    }
}

impl Drop for BuildOutputLock {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.owner_path) else {
            return;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return;
        }
        let Ok(bytes) = fs::read(&self.owner_path) else {
            return;
        };
        let Ok(owner) = serde_json::from_slice::<BuildOutputOwner>(&bytes) else {
            return;
        };
        if owner.owner_id == self.owner_id {
            let _ = fs::remove_file(&self.owner_path);
        }
    }
}

fn owner_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn owner_timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn write_owner_record(path: &Path, owner: &BuildOutputOwner) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!(
                "BuildKey output owner record is not a regular file: {}",
                path.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("checking BuildKey output owner record {}", path.display())
            });
        }
    }
    let parent = path
        .parent()
        .context("BuildKey output owner record has no parent")?;
    let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "creating temporary BuildKey owner record in {}",
            parent.display()
        )
    })?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), owner)
        .context("serializing BuildKey output owner record")?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| {
        anyhow::anyhow!(
            "publishing BuildKey output owner record {}: {}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

enum LockAttempt {
    Acquired(BuildOutputLock),
    Busy,
    Missing,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildCacheCleanReport {
    pub initial_bytes: u64,
    pub remaining_bytes: u64,
    pub evicted_bytes: u64,
    pub keys_cleaned: usize,
    pub active_keys_skipped: usize,
    pub unsafe_keys_skipped: usize,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
struct CacheCandidate {
    root: PathBuf,
    key_hash: String,
    bytes: u64,
    modified: SystemTime,
    unsafe_entries: bool,
}

#[derive(Clone, Debug)]
struct TreeStats {
    bytes: u64,
    modified: SystemTime,
    unsafe_entries: bool,
}

impl Default for TreeStats {
    fn default() -> Self {
        Self {
            bytes: 0,
            modified: UNIX_EPOCH,
            unsafe_entries: false,
        }
    }
}

/// Evicts the oldest recognized BuildKey output contents until the size budget
/// is met. Active keys and trees containing symlinks/special files are skipped.
/// Key roots and their lock files remain so waiters can safely acquire the same
/// lock after cleanup.
pub fn clean_build_cache(
    builds_root: &Path,
    max_bytes: u64,
    dry_run: bool,
) -> Result<BuildCacheCleanReport> {
    if !builds_root.is_absolute() {
        bail!("build cache root must be an absolute path");
    }
    let metadata = match fs::symlink_metadata(builds_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BuildCacheCleanReport {
                dry_run,
                ..BuildCacheCleanReport::default()
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("checking build cache root {}", builds_root.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "build cache root is not a regular directory: {}",
            builds_root.display()
        );
    }

    let mut candidates = scan_candidates(builds_root)?;
    candidates.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.key_hash.cmp(&right.key_hash))
            .then_with(|| left.root.cmp(&right.root))
    });
    let initial_bytes = candidates.iter().fold(0u64, |total, candidate| {
        total.saturating_add(candidate.bytes)
    });
    let mut report = BuildCacheCleanReport {
        initial_bytes,
        remaining_bytes: initial_bytes,
        dry_run,
        ..BuildCacheCleanReport::default()
    };

    for candidate in candidates {
        if report.remaining_bytes <= max_bytes {
            break;
        }
        if candidate.unsafe_entries {
            report.unsafe_keys_skipped += 1;
            continue;
        }

        let attempt = BuildOutputLock::try_acquire_at(&candidate.root, !dry_run, None)?;
        let lock = match attempt {
            LockAttempt::Acquired(lock) => Some(lock),
            LockAttempt::Busy => {
                report.active_keys_skipped += 1;
                continue;
            }
            LockAttempt::Missing if dry_run => None,
            LockAttempt::Missing => {
                report.active_keys_skipped += 1;
                continue;
            }
        };

        let current = inspect_tree(&candidate.root)?;
        report.remaining_bytes = report
            .remaining_bytes
            .saturating_sub(candidate.bytes)
            .saturating_add(current.bytes);
        if current.unsafe_entries {
            report.unsafe_keys_skipped += 1;
            drop(lock);
            continue;
        }
        if coordinator_is_active(&candidate.root)? {
            report.active_keys_skipped += 1;
            drop(lock);
            continue;
        }

        if dry_run {
            let lock_size = fs::symlink_metadata(
                candidate
                    .root
                    .join(super::output_layout::BUILD_OUTPUT_LOCK_FILE),
            )
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
            let evictable = current.bytes.saturating_sub(lock_size);
            report.evicted_bytes = report.evicted_bytes.saturating_add(evictable);
            report.remaining_bytes = report.remaining_bytes.saturating_sub(evictable);
            if evictable > 0 {
                report.keys_cleaned += 1;
            }
            drop(lock);
            continue;
        }

        let removed = clear_candidate_contents(&candidate.root)?;
        let after = inspect_tree(&candidate.root)?;
        report.evicted_bytes = report
            .evicted_bytes
            .saturating_add(current.bytes.saturating_sub(after.bytes));
        report.remaining_bytes = report
            .remaining_bytes
            .saturating_sub(current.bytes)
            .saturating_add(after.bytes);
        if removed > 0 {
            report.keys_cleaned += 1;
        }
        report.unsafe_keys_skipped += usize::from(after.unsafe_entries);
        drop(lock);
    }

    Ok(report)
}

fn scan_candidates(builds_root: &Path) -> Result<Vec<CacheCandidate>> {
    let mut candidates = Vec::new();
    for platform in CACHE_PLATFORMS {
        let platform_root = builds_root.join(platform);
        let metadata = match fs::symlink_metadata(&platform_root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading build cache platform {}", platform_root.display())
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&platform_root)
            .with_context(|| format!("scanning build cache {}", platform_root.display()))?
        {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_key_hash(&name) {
                continue;
            }
            let root = entry.path();
            let metadata = fs::symlink_metadata(&root)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let stats = inspect_tree(&root)?;
            candidates.push(CacheCandidate {
                root,
                key_hash: name,
                bytes: stats.bytes,
                modified: stats.modified,
                unsafe_entries: stats.unsafe_entries,
            });
        }
    }
    Ok(candidates)
}

fn inspect_tree(root: &Path) -> Result<TreeStats> {
    let mut stats = TreeStats::default();
    inspect_entry(root, &mut stats)?;
    Ok(stats)
}

fn inspect_entry(path: &Path, stats: &mut TreeStats) -> Result<()> {
    if matches!(
        path.file_name(),
        Some(
            name
        ) if name == std::ffi::OsStr::new(BUILD_OUTPUT_OWNER_FILE)
            || name == std::ffi::OsStr::new(super::output_layout::BUILD_COORDINATOR_STATE_FILE)
            || name == std::ffi::OsStr::new(super::output_layout::BUILD_COORDINATOR_LOCK_FILE)
            || name == std::ffi::OsStr::new(super::output_layout::BUILD_COORDINATOR_SUBSCRIBERS_DIR)
    ) {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspecting cached build output {}", path.display()))?;
    stats.modified = stats
        .modified
        .max(metadata.modified().unwrap_or(UNIX_EPOCH));
    if metadata.file_type().is_symlink() {
        stats.unsafe_entries = true;
    } else if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .with_context(|| format!("scanning cached build output {}", path.display()))?
        {
            inspect_entry(&entry?.path(), stats)?;
        }
    } else if !metadata.is_file() {
        stats.unsafe_entries = true;
        stats.bytes = stats.bytes.saturating_add(metadata.len());
    } else {
        stats.bytes = stats.bytes.saturating_add(metadata.len());
    }
    Ok(())
}

fn clear_candidate_contents(root: &Path) -> Result<u64> {
    let _state_lock = super::build_coordinator::lock_coordinator_state(root, false)?;
    let lock_path = root.join(super::output_layout::BUILD_OUTPUT_LOCK_FILE);
    let coordinator_lock_path = root.join(super::output_layout::BUILD_COORDINATOR_LOCK_FILE);
    let mut removed_bytes = 0u64;
    for entry in
        fs::read_dir(root).with_context(|| format!("cleaning cache key {}", root.display()))?
    {
        let path = entry?.path();
        if path == lock_path || path == coordinator_lock_path {
            continue;
        }
        removed_bytes = removed_bytes.saturating_add(remove_cache_entry(&path)?);
    }
    Ok(removed_bytes)
}

fn coordinator_is_active(root: &Path) -> Result<bool> {
    // Subscriber registration and coordinator-side counts share this short
    // lock. Acquiring it before probing prevents the cache cleaner from
    // observing a just-created, not-yet-locked subscriber file as stale.
    let _state_lock = super::build_coordinator::lock_coordinator_state(root, false)?;
    let directory = root.join(super::output_layout::BUILD_COORDINATOR_SUBSCRIBERS_DIR);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "checking BuildKey coordinator subscribers {}",
                    directory.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(true);
    }

    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading coordinator subscriber {}", path.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Ok(true),
        };
        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = file.unlock();
                let _ = fs::remove_file(&path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(true),
            Err(_) => return Ok(true),
        }
    }
    Ok(false)
}

fn remove_cache_entry(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspecting cache entry {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        let size = metadata.len();
        fs::remove_file(path).with_context(|| format!("removing cache file {}", path.display()))?;
        return Ok(size);
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut removed_bytes = 0u64;
    for entry in fs::read_dir(path)
        .with_context(|| format!("cleaning cache directory {}", path.display()))?
    {
        removed_bytes = removed_bytes.saturating_add(remove_cache_entry(&entry?.path())?);
    }
    match fs::remove_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("removing cache directory {}", path.display()));
        }
    }
    Ok(removed_bytes)
}

fn is_key_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug)]
pub enum BuildCacheLookup {
    Hit(BuildArtifactManifest),
    Miss(String),
}

/// Treats any absent, malformed, key-mismatched, or partial manifest as a
/// cache miss. The caller must hold BuildOutputLock while looking up and
/// rebuilding so another process cannot publish the same key concurrently.
pub fn lookup_verified(layout: &BuildOutputLayout, key: &BuildKey) -> BuildCacheLookup {
    lookup_verified_at_path(
        &layout.artifact_manifest_path(),
        &layout.root,
        layout.platform,
        key.key_hash(),
    )
}

/// Verifies a manifest whose expected key is represented by a trusted output
/// layout or preview environment. The caller must hold BuildOutputLock for the
/// same root while checking and rebuilding.
pub fn lookup_verified_at_path(
    path: &Path,
    root: &Path,
    platform: BuildPlatform,
    key_hash: &str,
) -> BuildCacheLookup {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return BuildCacheLookup::Miss("artifact manifest is missing".into());
        }
        Err(error) => {
            return BuildCacheLookup::Miss(format!("cannot inspect artifact manifest: {error}"));
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return BuildCacheLookup::Miss("artifact manifest is not a regular file".into());
        }
        Ok(_) => {}
    }

    match BuildArtifactManifest::read(path).and_then(|manifest| {
        if manifest.platform != platform {
            bail!(
                "build artifact manifest platform mismatch: expected {}, found {}",
                platform.label(),
                manifest.platform.label()
            );
        }
        if manifest.key_hash != key_hash {
            bail!(
                "build artifact manifest BuildKey mismatch: expected {}, found {}",
                key_hash,
                manifest.key_hash
            );
        }
        manifest.verify(root)?;
        Ok(manifest)
    }) {
        Ok(manifest) => BuildCacheLookup::Hit(manifest),
        Err(error) => BuildCacheLookup::Miss(format!("artifact manifest rejected: {error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::build_key::BuildKeyMaterial;
    use crate::runner::output_layout::BuildPlatform;
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn key() -> BuildKey {
        BuildKey::new(BuildKeyMaterial {
            source_manifest_hash: "source".into(),
            cargo_lock_hash: "lock".into(),
            target_triple: "x86_64-unknown-linux-gnu".into(),
            profile: "dev".into(),
            features: Vec::new(),
            abi: None,
            native_config_hash: "native".into(),
            toolchain_fingerprint: "toolchain".into(),
            relevant_env_hash: "env".into(),
            preview_registry_hash: "registry".into(),
        })
        .unwrap()
    }

    fn layout(base: &std::path::Path, key: &BuildKey) -> BuildOutputLayout {
        BuildOutputLayout::for_key(base, key, BuildPlatform::Desktop).unwrap()
    }

    fn key_for_source(source: &str) -> BuildKey {
        BuildKey::new(BuildKeyMaterial {
            source_manifest_hash: source.into(),
            ..key().material().clone()
        })
        .unwrap()
    }

    fn add_cache_file(layout: &BuildOutputLayout, name: &str, bytes: &[u8]) -> PathBuf {
        let path = layout.cargo_target_dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn missing_or_modified_manifest_is_a_cache_miss() {
        let base = tempfile::tempdir().unwrap();
        let key = key();
        let layout = layout(base.path(), &key);

        assert!(matches!(
            lookup_verified(&layout, &key),
            BuildCacheLookup::Miss(reason) if reason.contains("missing")
        ));

        layout.prepare().unwrap();
        let executable = layout.cargo_target_dir.join("debug/app");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"complete binary").unwrap();
        let entry = executable.strip_prefix(&layout.root).unwrap().to_owned();
        let manifest = BuildArtifactManifest::capture(&layout, &[entry]).unwrap();
        manifest
            .write_atomic(&layout.artifact_manifest_path())
            .unwrap();
        assert!(matches!(
            lookup_verified(&layout, &key),
            BuildCacheLookup::Hit(_)
        ));

        fs::write(&executable, b"partial or modified binary").unwrap();
        assert!(matches!(
            lookup_verified(&layout, &key),
            BuildCacheLookup::Miss(reason) if reason.contains("rejected")
        ));
    }

    #[test]
    fn preview_manifest_lookup_binds_platform_and_key_hash() {
        let base = tempfile::tempdir().unwrap();
        let key = key();
        let layout = layout(base.path(), &key);
        layout.prepare().unwrap();
        let executable = layout.cargo_target_dir.join("debug/app");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"preview binary").unwrap();
        let entry = executable.strip_prefix(&layout.root).unwrap().to_owned();
        BuildArtifactManifest::capture(&layout, &[entry])
            .unwrap()
            .write_atomic(&layout.preview_artifact_manifest_path())
            .unwrap();

        assert!(matches!(
            lookup_verified_at_path(
                &layout.preview_artifact_manifest_path(),
                &layout.root,
                BuildPlatform::Desktop,
                key.key_hash(),
            ),
            BuildCacheLookup::Hit(_)
        ));
        assert!(matches!(
            lookup_verified_at_path(
                &layout.preview_artifact_manifest_path(),
                &layout.root,
                BuildPlatform::Android,
                key.key_hash(),
            ),
            BuildCacheLookup::Miss(reason) if reason.contains("platform mismatch")
        ));
        assert!(matches!(
            lookup_verified_at_path(
                &layout.preview_artifact_manifest_path(),
                &layout.root,
                BuildPlatform::Desktop,
                &"0".repeat(64),
            ),
            BuildCacheLookup::Miss(reason) if reason.contains("BuildKey mismatch")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lock_path_is_rejected() {
        use std::os::unix::fs::symlink;

        let base = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let key = key();
        let layout = layout(base.path(), &key);
        fs::create_dir_all(&layout.root).unwrap();
        fs::write(outside.path().join("lock"), b"").unwrap();
        symlink(outside.path().join("lock"), layout.lock_file_path()).unwrap();

        assert!(BuildOutputLock::acquire(&layout).is_err());
    }

    #[test]
    fn output_lock_serializes_same_key_callers_and_survives_owner_exit() {
        let base = tempfile::tempdir().unwrap();
        let key = key();
        let layout = layout(base.path(), &key);
        let first = BuildOutputLock::acquire(&layout).unwrap();
        let lock_path = first.path().to_owned();
        let second_layout = layout.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let second = BuildOutputLock::acquire(&second_layout).unwrap();
            sender.send(()).unwrap();
            drop(second);
        });

        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert!(lock_path.is_file());
        let reacquired = BuildOutputLock::acquire(&layout).unwrap();
        drop(reacquired);
    }

    #[test]
    fn output_lock_publishes_owner_while_held_and_removes_it_on_drop() {
        let base = tempfile::tempdir().unwrap();
        let key = key();
        let layout = layout(base.path(), &key);
        let lock = BuildOutputLock::acquire(&layout).unwrap();
        let owner_path = layout.root.join(BUILD_OUTPUT_OWNER_FILE);
        let owner: BuildOutputOwner =
            serde_json::from_slice(&fs::read(&owner_path).unwrap()).unwrap();
        assert_eq!(owner.schema_version, BUILD_OUTPUT_OWNER_SCHEMA_VERSION);
        assert_eq!(owner.key_hash.as_deref(), Some(key.key_hash()));
        assert_eq!(owner.state, "building");
        assert_eq!(owner.pid, std::process::id());
        assert_eq!(lock.owner_path(), owner_path);
        drop(lock);
        assert!(!owner_path.exists());
    }

    #[test]
    fn output_lock_replaces_a_stale_owner_record_after_acquiring_the_lock() {
        let base = tempfile::tempdir().unwrap();
        let key = key();
        let layout = layout(base.path(), &key);
        layout.prepare().unwrap();
        let owner_path = layout.root.join(BUILD_OUTPUT_OWNER_FILE);
        let stale = BuildOutputOwner {
            schema_version: BUILD_OUTPUT_OWNER_SCHEMA_VERSION,
            owner_id: "stale-owner".into(),
            pid: 1,
            started_at_ms: 1,
            state: "building".into(),
            key_hash: Some(key.key_hash().into()),
        };
        write_owner_record(&owner_path, &stale).unwrap();

        let lock = BuildOutputLock::acquire(&layout).unwrap();
        let current: BuildOutputOwner =
            serde_json::from_slice(&fs::read(&owner_path).unwrap()).unwrap();
        assert_ne!(current.owner_id, stale.owner_id);
        assert_eq!(current.key_hash.as_deref(), Some(key.key_hash()));
        drop(lock);
        assert!(!owner_path.exists());
    }

    #[test]
    fn serialized_same_key_request_observes_the_completed_artifact_manifest() {
        let base = tempfile::tempdir().unwrap();
        let key = key();
        let layout = layout(base.path(), &key);
        let first = BuildOutputLock::acquire(&layout).unwrap();
        let second_layout = layout.clone();
        let second_key = key.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _second = BuildOutputLock::acquire(&second_layout).unwrap();
            sender
                .send(lookup_verified(&second_layout, &second_key))
                .unwrap();
        });

        layout.prepare().unwrap();
        let executable = layout.cargo_target_dir.join("debug/app");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"completed executable").unwrap();
        let relative = executable.strip_prefix(&layout.root).unwrap().to_owned();
        BuildArtifactManifest::capture(&layout, &[relative])
            .unwrap()
            .write_atomic(&layout.artifact_manifest_path())
            .unwrap();
        drop(first);

        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            BuildCacheLookup::Hit(_)
        ));
        worker.join().unwrap();
    }

    #[test]
    fn preview_root_lock_is_released_after_the_guard_drops() {
        let root = tempfile::tempdir().unwrap();
        let first = BuildOutputLock::acquire_at_root(root.path()).unwrap();
        assert!(
            first
                .path()
                .ends_with(super::super::output_layout::BUILD_OUTPUT_LOCK_FILE)
        );
        let root_path = root.path().to_owned();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let second = BuildOutputLock::acquire_at_root(&root_path).unwrap();
            sender.send(()).unwrap();
            drop(second);
        });
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert!(BuildOutputLock::acquire_at_root(root.path()).is_ok());
    }

    #[test]
    fn cache_clean_evicts_old_key_contents_to_the_requested_budget_and_keeps_lock_root() {
        let base = tempfile::tempdir().unwrap();
        let builds_root = base.path().join(".gpui/builds");
        let first = layout(&builds_root, &key_for_source("first"));
        let second = layout(&builds_root, &key_for_source("second"));
        first.prepare().unwrap();
        second.prepare().unwrap();
        let first_file = add_cache_file(&first, "debug/app", b"1234");
        let second_file = add_cache_file(&second, "debug/app", b"56789");

        let report = clean_build_cache(&builds_root, 5, false).unwrap();

        assert_eq!(report.initial_bytes, 9);
        assert!(report.remaining_bytes <= 5);
        assert_eq!(
            report.evicted_bytes,
            report.initial_bytes - report.remaining_bytes
        );
        assert!(report.keys_cleaned >= 1);
        assert!(first_file.exists() ^ second_file.exists());
        let cleaned = if first_file.exists() { &second } else { &first };
        assert!(cleaned.root.is_dir());
        assert!(cleaned.lock_file_path().is_file());
        let _lock = BuildOutputLock::acquire(cleaned).unwrap();
        assert!(cleaned.cargo_target_dir.is_dir());
    }

    #[test]
    fn cache_clean_dry_run_reports_eviction_without_creating_locks_or_deleting_files() {
        let base = tempfile::tempdir().unwrap();
        let builds_root = base.path().join(".gpui/builds");
        let layout = layout(&builds_root, &key());
        layout.prepare().unwrap();
        let payload = add_cache_file(&layout, "debug/app", b"cached output");
        let expected_size = fs::metadata(&payload).unwrap().len();

        let report = clean_build_cache(&builds_root, 0, true).unwrap();

        assert!(report.dry_run);
        assert_eq!(report.initial_bytes, expected_size);
        assert_eq!(report.evicted_bytes, expected_size);
        assert_eq!(report.remaining_bytes, 0);
        assert_eq!(report.keys_cleaned, 1);
        assert!(payload.is_file());
        assert!(!layout.lock_file_path().exists());
    }

    #[test]
    fn cache_clean_excludes_coordinator_metadata_from_size() {
        let base = tempfile::tempdir().unwrap();
        let builds_root = base.path().join(".gpui/builds");
        let layout = layout(&builds_root, &key());
        layout.prepare().unwrap();
        let payload = add_cache_file(&layout, "debug/app", b"cached output");
        fs::write(
            layout
                .root
                .join(super::super::output_layout::BUILD_OUTPUT_OWNER_FILE),
            b"owner metadata",
        )
        .unwrap();
        fs::write(
            layout
                .root
                .join(super::super::output_layout::BUILD_COORDINATOR_STATE_FILE),
            b"coordinator state",
        )
        .unwrap();
        fs::write(
            layout
                .root
                .join(super::super::output_layout::BUILD_COORDINATOR_LOCK_FILE),
            b"coordinator lock",
        )
        .unwrap();
        let subscribers = layout
            .root
            .join(super::super::output_layout::BUILD_COORDINATOR_SUBSCRIBERS_DIR);
        fs::create_dir_all(&subscribers).unwrap();
        fs::write(subscribers.join("subscriber.json"), b"subscriber").unwrap();

        let report = clean_build_cache(&builds_root, 0, false).unwrap();

        assert_eq!(report.initial_bytes, b"cached output".len() as u64);
        assert_eq!(report.evicted_bytes, report.initial_bytes);
        assert!(!payload.exists());
        assert!(layout.lock_file_path().is_file());
        assert!(
            layout
                .root
                .join(super::super::output_layout::BUILD_COORDINATOR_LOCK_FILE)
                .is_file()
        );
    }

    #[test]
    fn cache_clean_skips_a_key_with_an_active_coordinator_subscriber() {
        use std::sync::mpsc;

        let base = tempfile::tempdir().unwrap();
        let builds_root = base.path().join(".gpui/builds");
        let layout = layout(&builds_root, &key());
        layout.prepare().unwrap();
        let payload = add_cache_file(&layout, "debug/app", b"active output");
        let subscribers = layout
            .root
            .join(super::super::output_layout::BUILD_COORDINATOR_SUBSCRIBERS_DIR);
        fs::create_dir_all(&subscribers).unwrap();
        let subscriber = subscribers.join("subscriber.json");
        fs::write(&subscriber, br#"{"attempt_id":"attempt"}"#).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let thread_subscriber = subscriber.clone();
        let holder = thread::spawn(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(thread_subscriber)
                .unwrap();
            file.lock_exclusive().unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        // The channel guarantees the file lock is held before cleanup starts.
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let report = clean_build_cache(&builds_root, 0, false).unwrap();
        release_tx.send(()).unwrap();
        holder.join().unwrap();

        assert_eq!(report.active_keys_skipped, 1);
        assert_eq!(report.keys_cleaned, 0);
        assert!(payload.is_file());
    }

    #[test]
    fn cache_clean_skips_a_key_held_by_an_active_builder() {
        let base = tempfile::tempdir().unwrap();
        let builds_root = base.path().join(".gpui/builds");
        let layout = layout(&builds_root, &key());
        layout.prepare().unwrap();
        let payload = add_cache_file(&layout, "debug/app", b"active output");
        let lock = BuildOutputLock::acquire(&layout).unwrap();

        let report = clean_build_cache(&builds_root, 0, false).unwrap();

        assert_eq!(report.active_keys_skipped, 1);
        assert_eq!(report.keys_cleaned, 0);
        assert!(report.remaining_bytes > 0);
        assert!(payload.is_file());
        drop(lock);
    }

    #[cfg(unix)]
    #[test]
    fn cache_clean_skips_symlinked_key_contents_without_following_them() {
        use std::os::unix::fs::symlink;

        let base = tempfile::tempdir().unwrap();
        let builds_root = base.path().join(".gpui/builds");
        let layout = layout(&builds_root, &key());
        layout.prepare().unwrap();
        let payload = add_cache_file(&layout, "debug/app", b"cached output");
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("keep");
        fs::write(&secret, b"outside target").unwrap();
        symlink(&secret, layout.root.join("outside-link")).unwrap();

        let report = clean_build_cache(&builds_root, 0, false).unwrap();

        assert_eq!(report.unsafe_keys_skipped, 1);
        assert_eq!(report.keys_cleaned, 0);
        assert!(payload.is_file());
        assert_eq!(fs::read(secret).unwrap(), b"outside target");
    }

    #[cfg(unix)]
    #[test]
    fn cache_clean_refuses_a_symlinked_builds_root() {
        use std::os::unix::fs::symlink;

        let base = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let builds_root = base.path().join(".gpui/builds");
        fs::create_dir_all(builds_root.parent().unwrap()).unwrap();
        symlink(outside.path(), &builds_root).unwrap();

        assert!(clean_build_cache(&builds_root, 0, false).is_err());
        assert!(outside.path().is_dir());
    }
}
