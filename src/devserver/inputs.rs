//! Content manifests for the files covered by the live project watcher.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

const IGNORED: &[&str] = &[
    "target",
    ".git",
    ".gpui",
    ".gradle",
    "build",
    "jniLibs",
    "node_modules",
    "Pods",
];

pub fn should_trigger(path: &Path) -> bool {
    path.components().all(|part| {
        let name = part.as_os_str().to_string_lossy();
        !IGNORED.contains(&name.as_ref()) && !name.ends_with(".xcodeproj")
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Inputs {
    pub sources: BTreeMap<String, String>,
    pub assets: BTreeMap<String, String>,
    // Directory symlinks are not traversed; the manifest advertises this scope.
    pub untracked_directory_links: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FrozenInputs {
    pub manifest: Inputs,
    pub input_hash: String,
    pub snapshot_path: String,
}

/// Exact asset changes between two input scans. A watcher event can report a
/// deleted directory rather than each file beneath it, so live reload uses
/// this content-manifest diff instead of trusting the raw filesystem paths.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AssetDelta {
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

impl AssetDelta {
    pub fn between(
        previous: &BTreeMap<String, String>,
        current: &BTreeMap<String, String>,
    ) -> Self {
        let changed = current
            .iter()
            .filter(|(path, hash)| previous.get(*path) != Some(*hash))
            .map(|(path, _)| path.clone())
            .collect();
        let removed = previous
            .keys()
            .filter(|path| !current.contains_key(*path))
            .cloned()
            .collect();
        Self { changed, removed }
    }

    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

impl Inputs {
    /// Reads the project manifest until two consecutive content scans agree.
    ///
    /// The live watcher uses this bounded check before binding a build or
    /// observation target. It does not freeze arbitrary build-script inputs,
    /// but it prevents a file edit racing the directory walk from being
    /// silently reported as a coherent revision.
    pub fn scan_stable(root: &Path, max_rescans: usize) -> Result<Self> {
        Self::scan_stable_with(max_rescans, || Self::scan(root))
    }

    /// Copies a stable manifest into a new directory and verifies both the
    /// copy and the source once more before returning. The destination must
    /// not exist and must be outside the source root; callers own its
    /// lifetime and cleanup.
    pub fn freeze_to(root: &Path, destination: &Path, max_rescans: usize) -> Result<FrozenInputs> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving input root for snapshot: {}", root.display()))?;
        let destination = if destination.is_absolute() {
            destination.to_owned()
        } else {
            std::env::current_dir()?.join(destination)
        };
        if destination.exists() {
            bail!(
                "snapshot destination already exists: {}",
                destination.display()
            );
        }
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow::anyhow!("snapshot destination has no parent"))?;
        fs::create_dir_all(parent)?;
        let parent = fs::canonicalize(parent)?;
        let destination = parent.join(
            destination
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("snapshot destination has no file name"))?,
        );
        if destination.starts_with(&root) {
            bail!("snapshot destination must be outside the input root");
        }

        let manifest = Self::scan_stable(&root, max_rescans)?;
        if !manifest.untracked_directory_links.is_empty() {
            bail!(
                "cannot freeze inputs with untracked directory links: {:?}",
                manifest.untracked_directory_links
            );
        }
        fs::create_dir(&destination)?;
        let copy_result = (|| -> Result<()> {
            for relative in manifest.sources.keys().chain(manifest.assets.keys()) {
                copy_input_file(&root, &destination, relative)?;
            }
            let copied = Self::scan(&destination)?;
            if copied != manifest {
                bail!("frozen input copy does not match its manifest");
            }
            let current = Self::scan_stable(&root, max_rescans)?;
            if current != manifest {
                bail!("source inputs changed while freezing the snapshot");
            }
            Ok(())
        })();
        if let Err(error) = copy_result {
            let _ = fs::remove_dir_all(&destination);
            return Err(error);
        }
        Ok(FrozenInputs {
            input_hash: manifest.digest(),
            manifest,
            snapshot_path: destination.to_string_lossy().into_owned(),
        })
    }

    fn scan_stable_with(
        max_rescans: usize,
        mut scan: impl FnMut() -> Result<Self>,
    ) -> Result<Self> {
        let mut previous = scan()?;
        for _ in 0..=max_rescans {
            let current = scan()?;
            if current == previous {
                return Ok(current);
            }
            previous = current;
        }
        bail!(
            "project inputs changed during the bounded stability scan after {} rescans",
            max_rescans
        )
    }

    pub fn scan(root: &Path) -> Result<Self> {
        let mut result = Self::default();
        let mut pending = vec![root.to_owned()];
        let mut buffer = [0u8; 32 * 1024];
        while let Some(dir) = pending.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(e).with_context(|| format!("reading inputs in {}", dir.display()));
                }
            };
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                let rel = path.strip_prefix(root)?;
                if !should_trigger(rel) {
                    continue;
                }
                let name = rel.to_string_lossy().replace('\\', "/");
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(path);
                    continue;
                }
                if kind.is_symlink() && path.is_dir() {
                    result.untracked_directory_links.push(name);
                    continue;
                }
                if !kind.is_file() && !kind.is_symlink() {
                    continue;
                }
                let mut file = match fs::File::open(&path) {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        return Err(e).with_context(|| format!("reading input {}", path.display()));
                    }
                };
                let mut hash = Sha256::new();
                loop {
                    let len = file.read(&mut buffer)?;
                    if len == 0 {
                        break;
                    }
                    hash.update(&buffer[..len]);
                }
                let digest = format!("{:x}", hash.finalize());
                if name.starts_with("assets/") {
                    result.assets.insert(name, digest);
                } else {
                    result.sources.insert(name, digest);
                }
            }
        }
        result.untracked_directory_links.sort();
        Ok(result)
    }

    pub fn digest(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("serializable inputs"))
        )
    }
}

fn copy_input_file(root: &Path, destination: &Path, relative: &str) -> Result<()> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("input manifest contains an unsafe path: {relative}");
    }
    let source = root.join(relative_path);
    let metadata = fs::symlink_metadata(&source)
        .with_context(|| format!("reading frozen input {}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("cannot freeze non-regular input: {relative}");
    }
    let target = destination.join(relative_path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&source, &target).with_context(|| format!("copying frozen input {}", relative))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_changes_and_deletions_are_detected_but_output_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("assets")).unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("main.rs"), "first").unwrap();
        fs::write(dir.path().join("assets/icon"), "image").unwrap();
        let before = Inputs::scan(dir.path()).unwrap();
        fs::write(dir.path().join("target/debug/app"), "output").unwrap();
        assert_eq!(before, Inputs::scan(dir.path()).unwrap());
        fs::write(dir.path().join("main.rs"), "other").unwrap();
        let after = Inputs::scan(dir.path()).unwrap();
        assert_ne!(before.sources, after.sources);
        assert_eq!(before.assets, after.assets);
        fs::remove_file(dir.path().join("assets/icon")).unwrap();
        let deleted = Inputs::scan(dir.path()).unwrap();
        assert!(deleted.assets.is_empty());
        let delta = AssetDelta::between(&after.assets, &deleted.assets);
        assert_eq!(delta.changed, Vec::<String>::new());
        assert_eq!(delta.removed, vec!["assets/icon"]);
    }

    #[test]
    fn asset_delta_marks_added_and_hash_changed_files() {
        let before = BTreeMap::from([
            ("assets/old.png".into(), "old".into()),
            ("assets/same.png".into(), "same".into()),
        ]);
        let after = BTreeMap::from([
            ("assets/new.png".into(), "new".into()),
            ("assets/same.png".into(), "same".into()),
            ("assets/changed.png".into(), "new-hash".into()),
        ]);
        let delta = AssetDelta::between(&before, &after);
        assert_eq!(delta.changed, vec!["assets/changed.png", "assets/new.png"]);
        assert_eq!(delta.removed, vec!["assets/old.png"]);
    }

    #[test]
    fn stable_scan_retries_until_two_consecutive_manifests_match() {
        let first = Inputs {
            sources: BTreeMap::from([(String::from("main.rs"), String::from("a"))]),
            ..Inputs::default()
        };
        let second = Inputs {
            sources: BTreeMap::from([(String::from("main.rs"), String::from("b"))]),
            ..Inputs::default()
        };
        let mut scans = vec![first.clone(), second.clone(), second.clone()].into_iter();
        let stable = Inputs::scan_stable_with(2, || Ok(scans.next().unwrap())).unwrap();
        assert_eq!(stable, second);
    }

    #[test]
    fn stable_scan_rejects_continuous_changes_after_the_bound() {
        let mut next = 0_u8;
        let error = Inputs::scan_stable_with(1, || {
            next += 1;
            Ok(Inputs {
                sources: BTreeMap::from([(String::from("main.rs"), next.to_string())]),
                ..Inputs::default()
            })
        })
        .unwrap_err();
        assert!(error.to_string().contains("bounded stability scan"));
    }

    #[test]
    fn freeze_to_copies_and_rechecks_a_manifest_outside_the_source_root() {
        let root = tempfile::tempdir().unwrap();
        let destination_parent = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.path().join("assets/icon.txt"), "icon\n").unwrap();
        fs::create_dir_all(root.path().join("target")).unwrap();
        fs::write(root.path().join("target/ignored"), "ignored\n").unwrap();
        let destination = destination_parent.path().join("snapshot");

        let frozen = Inputs::freeze_to(root.path(), &destination, 1).unwrap();

        assert_eq!(frozen.manifest, Inputs::scan(&destination).unwrap());
        assert_eq!(frozen.input_hash, frozen.manifest.digest());
        assert_eq!(
            fs::read_to_string(destination.join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(!destination.join("target/ignored").exists());
    }

    #[test]
    fn freeze_to_rejects_a_destination_inside_the_source_root() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("main.rs"), "main\n").unwrap();
        let error = Inputs::freeze_to(root.path(), &root.path().join("snapshot"), 1).unwrap_err();
        assert!(error.to_string().contains("outside the input root"));
    }
}
