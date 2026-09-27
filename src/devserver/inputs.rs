//! Content manifests for the files covered by the live project watcher.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// The local filesystem roots that Cargo reports for a project workspace.
///
/// Registry and git packages are intentionally not included here. Their
/// sources are managed by Cargo and are not part of the project's explicit
/// path-input boundary. External path packages are kept as separate roots so
/// a later snapshot step can copy them without accidentally traversing the
/// developer's parent directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CargoInputScope {
    pub workspace_root: String,
    pub external_path_dependencies: Vec<ExternalPathDependency>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExternalPathDependency {
    pub root: String,
    pub manifest_path: String,
    pub package_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    workspace_root: PathBuf,
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    manifest_path: PathBuf,
    source: Option<String>,
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
    /// Discovers Cargo's local path-package boundary without mutating the
    /// project. `--locked` is deliberate: strict input discovery must not
    /// create or rewrite Cargo.lock as a side effect.
    pub fn cargo_input_scope(root: &Path) -> Result<CargoInputScope> {
        CargoInputScope::discover(root)
    }

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

impl CargoInputScope {
    /// Runs full locked Cargo metadata so path dependencies nested below a
    /// workspace member are visible as well as direct path dependencies.
    pub fn discover(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving Cargo workspace root: {}", root.display()))?;
        let output = Command::new("cargo")
            .current_dir(&root)
            .args(["metadata", "--format-version", "1", "--locked"])
            .output()
            .context("running cargo metadata")?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            if detail.is_empty() {
                bail!("cargo metadata failed with status {}", output.status);
            }
            bail!("cargo metadata failed: {detail}");
        }
        Self::from_metadata_json(&root, &output.stdout)
    }

    /// Parses Cargo metadata separately from process execution so the input
    /// boundary can be tested against deterministic fixtures.
    pub fn from_metadata_json(root: &Path, json: &[u8]) -> Result<Self> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving Cargo workspace root: {}", root.display()))?;
        let metadata: CargoMetadata =
            serde_json::from_slice(json).context("parsing cargo metadata JSON")?;
        let metadata_root = resolve_metadata_path(&root, &metadata.workspace_root);
        let metadata_root = fs::canonicalize(&metadata_root).with_context(|| {
            format!(
                "resolving cargo metadata workspace root: {}",
                metadata_root.display()
            )
        })?;
        if metadata_root != root {
            bail!(
                "cargo metadata workspace root {} does not match input root {}",
                metadata_root.display(),
                root.display()
            );
        }

        let mut external = BTreeMap::<PathBuf, (PathBuf, Vec<String>)>::new();
        for package in metadata.packages {
            // Cargo uses a null source for local path packages. Registry and
            // git packages are intentionally outside the project input scope.
            if package.source.is_some() {
                continue;
            }
            let raw_manifest = resolve_metadata_path(&root, &package.manifest_path);
            reject_symlinked_path_package(&raw_manifest)?;
            let manifest = fs::canonicalize(&raw_manifest).with_context(|| {
                format!(
                    "resolving local Cargo package manifest: {}",
                    raw_manifest.display()
                )
            })?;
            let package_root = manifest.parent().ok_or_else(|| {
                anyhow::anyhow!(
                    "Cargo package manifest has no parent: {}",
                    manifest.display()
                )
            })?;

            if package_root == root || package_root.starts_with(&root) {
                continue;
            }
            if root.starts_with(package_root) {
                bail!(
                    "external Cargo path package {} contains the workspace root: {}",
                    package.id,
                    package_root.display()
                );
            }

            let entry = external
                .entry(package_root.to_owned())
                .or_insert_with(|| (manifest.clone(), Vec::new()));
            entry.1.push(package.id);
        }

        let external_path_dependencies = external
            .into_iter()
            .map(|(root, (manifest_path, mut package_ids))| {
                package_ids.sort();
                package_ids.dedup();
                ExternalPathDependency {
                    root: root.to_string_lossy().into_owned(),
                    manifest_path: manifest_path.to_string_lossy().into_owned(),
                    package_ids,
                }
            })
            .collect();

        Ok(Self {
            workspace_root: root.to_string_lossy().into_owned(),
            external_path_dependencies,
        })
    }
}

fn resolve_metadata_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

fn reject_symlinked_path_package(manifest: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(manifest)
        .with_context(|| format!("reading Cargo package manifest: {}", manifest.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "external Cargo path package uses a symlinked manifest: {}",
            manifest.display()
        );
    }
    let parent = manifest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cargo package manifest has no parent"))?;
    let metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("reading Cargo path package directory: {}", parent.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "external Cargo path package uses a symlinked directory: {}",
            parent.display()
        );
    }
    Ok(())
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
    fn cargo_scope_collects_sorted_external_path_packages_and_ignores_registry_sources() {
        let workspace = tempfile::tempdir().unwrap();
        let dependencies = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::create_dir_all(dependencies.path().join("z-package")).unwrap();
        fs::create_dir_all(dependencies.path().join("a-package")).unwrap();
        fs::write(
            dependencies.path().join("z-package/Cargo.toml"),
            "[package]\nname = \"z-package\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"lib.rs\"\n",
        )
        .unwrap();
        fs::write(
            dependencies.path().join("a-package/Cargo.toml"),
            "[package]\nname = \"a-package\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"lib.rs\"\n",
        )
        .unwrap();

        let package = |id: &str, manifest: &Path, source: Option<&str>| {
            serde_json::json!({
                "id": id,
                "manifest_path": manifest,
                "source": source,
            })
        };
        let json = serde_json::json!({
            "workspace_root": workspace.path(),
            "packages": [
                package(
                    "path+file:///workspace#root@0.1.0",
                    &workspace.path().join("Cargo.toml"),
                    None,
                ),
                package(
                    "path+file:///dependencies/z-package#z-package@0.1.0",
                    &dependencies.path().join("z-package/Cargo.toml"),
                    None,
                ),
                package(
                    "path+file:///dependencies/a-package#a-package@0.1.0",
                    &dependencies.path().join("a-package/Cargo.toml"),
                    None,
                ),
                package(
                    "registry+https://example.invalid#registry@1.0.0",
                    &workspace.path().join("not-used/Cargo.toml"),
                    Some("registry+https://example.invalid"),
                ),
            ],
        });

        let scope = CargoInputScope::from_metadata_json(
            workspace.path(),
            &serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();
        let roots: Vec<_> = scope
            .external_path_dependencies
            .iter()
            .map(|dependency| PathBuf::from(&dependency.root))
            .collect();
        assert_eq!(
            roots,
            vec![
                fs::canonicalize(dependencies.path().join("a-package")).unwrap(),
                fs::canonicalize(dependencies.path().join("z-package")).unwrap(),
            ]
        );
        assert_eq!(
            scope.external_path_dependencies[0].package_ids,
            vec!["path+file:///dependencies/a-package#a-package@0.1.0"]
        );
    }

    #[test]
    fn cargo_scope_discovers_the_checked_in_workspace_without_mutating_it() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let scope = CargoInputScope::discover(root).unwrap();

        assert_eq!(
            scope.workspace_root,
            fs::canonicalize(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        );
        assert!(scope.external_path_dependencies.is_empty());
    }

    #[test]
    fn cargo_scope_rejects_an_external_package_that_contains_the_workspace() {
        let container = tempfile::tempdir().unwrap();
        let workspace = container.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(container.path().join("Cargo.toml"), "[package]\n").unwrap();
        let json = serde_json::json!({
            "workspace_root": workspace,
            "packages": [{
                "id": "path+file:///container#parent@0.1.0",
                "manifest_path": container.path().join("Cargo.toml"),
                "source": null,
            }],
        });

        let error =
            CargoInputScope::from_metadata_json(&workspace, &serde_json::to_vec(&json).unwrap())
                .unwrap_err();
        assert!(error.to_string().contains("contains the workspace root"));
    }

    #[cfg(unix)]
    #[test]
    fn cargo_scope_rejects_a_symlinked_external_package_root() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let dependencies = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let real = dependencies.path().join("real-package");
        let link = dependencies.path().join("linked-package");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("Cargo.toml"), "[package]\n").unwrap();
        symlink(&real, &link).unwrap();
        let json = serde_json::json!({
            "workspace_root": workspace.path(),
            "packages": [{
                "id": "path+file:///dependencies/linked-package#linked@0.1.0",
                "manifest_path": link.join("Cargo.toml"),
                "source": null,
            }],
        });

        let error = CargoInputScope::from_metadata_json(
            workspace.path(),
            &serde_json::to_vec(&json).unwrap(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("symlinked directory"));
    }

    #[test]
    fn cargo_scope_rejects_metadata_for_a_different_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(other.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let json = serde_json::json!({
            "workspace_root": other.path(),
            "packages": [],
        });

        let error = CargoInputScope::from_metadata_json(
            workspace.path(),
            &serde_json::to_vec(&json).unwrap(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not match input root"));
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
