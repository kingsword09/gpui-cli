//! BuildKey-scoped local artifact reuse primitives.

use super::build_key::BuildKey;
use super::build_manifest::BuildArtifactManifest;
use super::output_layout::BuildOutputLayout;
use anyhow::{Context, Result, bail};
use std::fs::TryLockError;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_PLATFORMS: &[&str] = &["desktop", "android", "ios"];

/// A per-output-root advisory lock. The lock file is intentionally persistent:
/// the operating system releases the lock when the process exits or crashes.
pub struct BuildOutputLock {
    _file: File,
    path: PathBuf,
}

impl BuildOutputLock {
    /// Serializes builders and cache readers for one platform/BuildKey.
    /// Callers keep the guard through validation, build, and manifest publish.
    pub fn acquire(layout: &BuildOutputLayout) -> Result<Self> {
        fs::create_dir_all(&layout.root)
            .with_context(|| format!("creating BuildKey output root {}", layout.root.display()))?;
        let path = layout.lock_file_path();
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
            .with_context(|| format!("locking BuildKey output {}", layout.key_hash))?;
        layout
            .prepare()
            .with_context(|| format!("preparing locked BuildKey output {}", layout.key_hash))?;
        Ok(Self { _file: file, path })
    }

    fn try_acquire_at(root: &Path, create_lock: bool) -> Result<LockAttempt> {
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
            Ok(()) => Ok(LockAttempt::Acquired(Self {
                _file: file,
                path: lock_path,
            })),
            Err(TryLockError::WouldBlock) => Ok(LockAttempt::Busy),
            Err(TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("trying BuildKey output lock {}", root.display())),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
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

        let attempt = BuildOutputLock::try_acquire_at(&candidate.root, !dry_run)?;
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
    let lock_path = root.join(super::output_layout::BUILD_OUTPUT_LOCK_FILE);
    let mut removed_bytes = 0u64;
    for entry in
        fs::read_dir(root).with_context(|| format!("cleaning cache key {}", root.display()))?
    {
        let path = entry?.path();
        if path == lock_path {
            continue;
        }
        removed_bytes = removed_bytes.saturating_add(remove_cache_entry(&path)?);
    }
    Ok(removed_bytes)
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
    let path = layout.artifact_manifest_path();
    match fs::symlink_metadata(&path) {
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

    match BuildArtifactManifest::read_verified(&path, &layout.root, layout.platform, key) {
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
