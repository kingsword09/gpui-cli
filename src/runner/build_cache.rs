//! BuildKey-scoped local artifact reuse primitives.

use super::build_key::BuildKey;
use super::build_manifest::BuildArtifactManifest;
use super::output_layout::BuildOutputLayout;
use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

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
        Ok(Self { _file: file, path })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
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
}
