//! Content manifests for the files covered by the live project watcher.

use anyhow::{Context, Result};
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
}
