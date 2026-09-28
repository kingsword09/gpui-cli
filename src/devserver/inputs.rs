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
const SENSITIVE_INPUT_FILE_NAMES: &[&str] = &["local.properties", "keystore.properties"];

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_sensitive_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FrozenInputs {
    pub manifest: Inputs,
    pub external_inputs: Vec<FrozenExternalInput>,
    pub path_relocations: Vec<PathRelocation>,
    pub input_hash: String,
    pub snapshot_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FrozenExternalInput {
    pub source_root: String,
    pub snapshot_root: String,
    pub manifest: Inputs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PathRelocation {
    pub source_root: String,
    pub snapshot_root: String,
}

/// Explicit native-host input scope used alongside the Cargo source manifest.
///
/// The scope is intentionally limited to generated-project native manifests,
/// scripts, resources and source trees. Build outputs, IDE projects and local
/// signing/configuration files are excluded and never become build inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NativeInputs {
    pub files: BTreeMap<String, String>,
    pub external_hashes: BTreeMap<String, String>,
    pub untracked_directory_links: Vec<String>,
    pub excluded_sensitive_files: Vec<String>,
}

const NATIVE_ROOTS: &[&str] = &[
    "mobile/ios",
    "mobile/android/gradle",
    "mobile/android/.cargo/config.toml",
    ".cargo/config.toml",
    "gpui.toml",
];

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

impl NativeInputs {
    /// Scans only the known native-host input roots. Missing platform roots
    /// are valid because a project may target desktop only.
    pub fn scan(root: &Path) -> Result<Self> {
        let mut result = Self::default();
        let mut pending = Vec::new();
        for relative in NATIVE_ROOTS {
            let path = root.join(relative);
            if path.is_dir() {
                pending.push(path);
            } else if path.is_file() {
                collect_native_file(root, &path, &mut result)?;
            }
        }

        while let Some(dir) = pending.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("reading native inputs in {}", dir.display()));
                }
            };
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(root)?;
                if !native_should_trigger(relative) {
                    continue;
                }
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_symlink() && path.is_dir() {
                    result
                        .untracked_directory_links
                        .push(relative.to_string_lossy().replace('\\', "/"));
                } else if kind.is_symlink() {
                    bail!(
                        "native input uses an untracked file symlink: {}",
                        relative.display()
                    );
                } else if kind.is_file() {
                    collect_native_file(root, &path, &mut result)?;
                }
            }
        }
        result.untracked_directory_links.sort();
        result.excluded_sensitive_files.sort();
        Ok(result)
    }

    pub fn digest(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("serializable native inputs"))
        )
    }
}

fn native_should_trigger(path: &Path) -> bool {
    path.components().all(|component| {
        let name = component.as_os_str().to_string_lossy();
        !IGNORED.contains(&name.as_ref())
            && !name.ends_with(".xcodeproj")
            && !name.ends_with(".xcworkspace")
    })
}

fn is_sensitive_input_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| SENSITIVE_INPUT_FILE_NAMES.contains(&name))
}

fn collect_native_file(root: &Path, path: &Path, result: &mut NativeInputs) -> Result<()> {
    let relative = path.strip_prefix(root)?;
    let name = relative.to_string_lossy().replace('\\', "/");
    if is_sensitive_input_file(relative) {
        result.excluded_sensitive_files.push(name);
        return Ok(());
    }
    let mut file = fs::File::open(path)
        .with_context(|| format!("reading native input: {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let len = file.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hash.update(&buffer[..len]);
    }
    result.files.insert(name, format!("{:x}", hash.finalize()));
    Ok(())
}

impl Inputs {
    /// Discovers Cargo's local path-package boundary without mutating the
    /// project. `--locked` is deliberate: strict input discovery must not
    /// create or rewrite Cargo.lock as a side effect.
    pub fn cargo_input_scope(root: &Path) -> Result<CargoInputScope> {
        CargoInputScope::discover(root)
    }

    pub fn native_input_scope(root: &Path) -> Result<NativeInputs> {
        NativeInputs::scan(root)
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
        let scope = CargoInputScope {
            workspace_root: root.to_string_lossy().into_owned(),
            external_path_dependencies: Vec::new(),
        };
        Self::freeze_to_with_cargo_scope(&root, destination, &scope, max_rescans)
    }

    /// Freezes the workspace and the external local Cargo package roots in a
    /// scope. External roots are copied below `external/NNNN`; Cargo manifest
    /// path fields in the snapshot are rewritten to those relocated roots.
    /// The source workspace and external packages are never rewritten.
    pub fn freeze_to_with_cargo_scope(
        root: &Path,
        destination: &Path,
        scope: &CargoInputScope,
        max_rescans: usize,
    ) -> Result<FrozenInputs> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving input root for snapshot: {}", root.display()))?;
        let scope_root = fs::canonicalize(Path::new(&scope.workspace_root)).with_context(|| {
            format!(
                "resolving Cargo scope workspace root: {}",
                scope.workspace_root
            )
        })?;
        if scope_root != root {
            bail!(
                "Cargo input scope workspace root {} does not match input root {}",
                scope_root.display(),
                root.display()
            );
        }

        let external_roots = resolve_external_roots(&root, scope)?;
        let destination = prepare_snapshot_destination(&root, &external_roots, destination)?;
        let manifest = Self::scan_stable(&root, max_rescans)?;
        reject_untracked_links("workspace", &manifest)?;
        let external_manifests = external_roots
            .iter()
            .map(|external| {
                let manifest = Self::scan_stable(&external.source_root, max_rescans)?;
                reject_untracked_links("external Cargo package", &manifest)?;
                Ok(manifest)
            })
            .collect::<Result<Vec<_>>>()?;

        fs::create_dir(&destination)?;
        let copy_result = (|| -> Result<()> {
            copy_manifest_files(&root, &destination, &manifest)?;
            let copied = Self::scan(&destination)?;
            if !snapshot_copy_matches(&manifest, &copied) {
                bail!("frozen workspace copy does not match its manifest");
            }

            for (external, manifest) in external_roots.iter().zip(&external_manifests) {
                let target = destination.join(&external.snapshot_root);
                fs::create_dir_all(&target)?;
                copy_manifest_files(&external.source_root, &target, manifest)?;
                let copied = Self::scan(&target)?;
                if !snapshot_copy_matches(manifest, &copied) {
                    bail!(
                        "frozen external package copy does not match its manifest: {}",
                        external.source_root.display()
                    );
                }
            }

            let current = Self::scan_stable(&root, max_rescans)?;
            if current != manifest {
                bail!("source inputs changed while freezing the snapshot");
            }
            for (external, expected) in external_roots.iter().zip(&external_manifests) {
                let current = Self::scan_stable(&external.source_root, max_rescans)?;
                if current != *expected {
                    bail!(
                        "external Cargo package changed while freezing the snapshot: {}",
                        external.source_root.display()
                    );
                }
            }

            let relocations = external_roots
                .iter()
                .map(|external| ResolvedPathRelocation {
                    source_root: external.source_root.clone(),
                    snapshot_root: destination.join(&external.snapshot_root),
                })
                .collect::<Vec<_>>();
            rewrite_cargo_manifests(&root, &destination, &manifest, &relocations)?;
            for (external, manifest) in external_roots.iter().zip(&external_manifests) {
                rewrite_cargo_manifests(
                    &external.source_root,
                    &destination.join(&external.snapshot_root),
                    manifest,
                    &relocations,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = copy_result {
            let _ = fs::remove_dir_all(&destination);
            return Err(error);
        }

        let external_inputs = external_roots
            .iter()
            .zip(external_manifests)
            .map(|(external, manifest)| FrozenExternalInput {
                source_root: external.source_root.to_string_lossy().into_owned(),
                snapshot_root: external.snapshot_root.to_string_lossy().replace('\\', "/"),
                manifest,
            })
            .collect::<Vec<_>>();
        let path_relocations = external_inputs
            .iter()
            .map(|external| PathRelocation {
                source_root: external.source_root.clone(),
                snapshot_root: external.snapshot_root.clone(),
            })
            .collect::<Vec<_>>();
        let input_hash = frozen_input_digest(&manifest, &external_inputs, &path_relocations)?;
        Ok(FrozenInputs {
            manifest,
            external_inputs,
            path_relocations,
            input_hash,
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
                if is_sensitive_input_file(&path) {
                    result.excluded_sensitive_files.push(name);
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
        result.excluded_sensitive_files.sort();
        Ok(result)
    }

    pub fn digest(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("serializable inputs"))
        )
    }
}

#[derive(Clone, Debug)]
struct ResolvedExternalRoot {
    source_root: PathBuf,
    snapshot_root: PathBuf,
}

#[derive(Clone, Debug)]
struct ResolvedPathRelocation {
    source_root: PathBuf,
    snapshot_root: PathBuf,
}

fn resolve_external_roots(
    workspace_root: &Path,
    scope: &CargoInputScope,
) -> Result<Vec<ResolvedExternalRoot>> {
    let mut roots = Vec::with_capacity(scope.external_path_dependencies.len());
    for (index, dependency) in scope.external_path_dependencies.iter().enumerate() {
        let raw_root = Path::new(&dependency.root);
        let raw_root = if raw_root.is_absolute() {
            raw_root.to_owned()
        } else {
            workspace_root.join(raw_root)
        };
        let metadata = fs::symlink_metadata(&raw_root).with_context(|| {
            format!(
                "reading external Cargo package root: {}",
                raw_root.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "external Cargo package root must be a non-symlink directory: {}",
                raw_root.display()
            );
        }
        let source_root = fs::canonicalize(&raw_root).with_context(|| {
            format!(
                "resolving external Cargo package root: {}",
                raw_root.display()
            )
        })?;
        if source_root == workspace_root || source_root.starts_with(workspace_root) {
            bail!(
                "external Cargo package root is inside the workspace: {}",
                source_root.display()
            );
        }
        if workspace_root.starts_with(&source_root) {
            bail!(
                "external Cargo package root contains the workspace: {}",
                source_root.display()
            );
        }
        if roots.iter().any(|existing: &ResolvedExternalRoot| {
            source_root.starts_with(&existing.source_root)
                || existing.source_root.starts_with(&source_root)
        }) {
            bail!(
                "external Cargo package roots overlap: {}",
                source_root.display()
            );
        }
        roots.push(ResolvedExternalRoot {
            source_root,
            snapshot_root: PathBuf::from("external").join(format!("{index:04}")),
        });
    }
    Ok(roots)
}

fn prepare_snapshot_destination(
    workspace_root: &Path,
    external_roots: &[ResolvedExternalRoot],
    destination: &Path,
) -> Result<PathBuf> {
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
    if destination.starts_with(workspace_root)
        || external_roots
            .iter()
            .any(|external| destination.starts_with(&external.source_root))
    {
        bail!("snapshot destination must be outside all input roots");
    }
    Ok(destination)
}

fn reject_untracked_links(label: &str, manifest: &Inputs) -> Result<()> {
    if manifest.untracked_directory_links.is_empty() {
        return Ok(());
    }
    bail!(
        "cannot freeze {label} with untracked directory links: {:?}",
        manifest.untracked_directory_links
    )
}

fn snapshot_copy_matches(expected: &Inputs, copied: &Inputs) -> bool {
    expected.sources == copied.sources
        && expected.assets == copied.assets
        && expected.untracked_directory_links == copied.untracked_directory_links
        && copied.excluded_sensitive_files.is_empty()
}

fn copy_manifest_files(root: &Path, destination: &Path, manifest: &Inputs) -> Result<()> {
    for relative in manifest.sources.keys().chain(manifest.assets.keys()) {
        copy_input_file(root, destination, relative)?;
    }
    Ok(())
}

fn rewrite_cargo_manifests(
    source_root: &Path,
    destination_root: &Path,
    manifest: &Inputs,
    relocations: &[ResolvedPathRelocation],
) -> Result<()> {
    for relative in manifest.sources.keys().chain(manifest.assets.keys()) {
        if Path::new(relative).file_name() != Some(std::ffi::OsStr::new("Cargo.toml")) {
            continue;
        }
        let source_manifest = source_root.join(relative);
        let destination_manifest = destination_root.join(relative);
        rewrite_cargo_manifest(&source_manifest, &destination_manifest, relocations)?;
    }
    Ok(())
}

fn rewrite_cargo_manifest(
    source_manifest: &Path,
    destination_manifest: &Path,
    relocations: &[ResolvedPathRelocation],
) -> Result<()> {
    let source = fs::read_to_string(source_manifest)
        .with_context(|| format!("reading Cargo manifest: {}", source_manifest.display()))?;
    let mut value: toml::Value = toml::from_str(&source)
        .with_context(|| format!("parsing Cargo manifest: {}", source_manifest.display()))?;
    if !rewrite_toml_paths(
        &mut value,
        source_manifest
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cargo manifest has no parent"))?,
        destination_manifest
            .parent()
            .ok_or_else(|| anyhow::anyhow!("snapshot Cargo manifest has no parent"))?,
        relocations,
    )? {
        return Ok(());
    }
    let rewritten = toml::to_string(&value).context("serializing relocated Cargo manifest")?;
    fs::write(destination_manifest, rewritten).with_context(|| {
        format!(
            "writing relocated Cargo manifest: {}",
            destination_manifest.display()
        )
    })?;
    Ok(())
}

fn rewrite_toml_paths(
    value: &mut toml::Value,
    source_parent: &Path,
    destination_parent: &Path,
    relocations: &[ResolvedPathRelocation],
) -> Result<bool> {
    match value {
        toml::Value::Array(values) => {
            let mut changed = false;
            for value in values {
                changed |=
                    rewrite_toml_paths(value, source_parent, destination_parent, relocations)?;
            }
            Ok(changed)
        }
        toml::Value::Table(table) => {
            let original_path = table
                .get("path")
                .and_then(toml::Value::as_str)
                .map(str::to_owned);
            let mut changed = false;
            if let Some(original_path) = original_path
                && let Some(rewritten) = relocated_path(
                    &original_path,
                    source_parent,
                    destination_parent,
                    relocations,
                )?
                && let Some(toml::Value::String(path)) = table.get_mut("path")
            {
                *path = rewritten;
                changed = true;
            }
            for value in table.iter_mut().map(|(_, value)| value) {
                changed |=
                    rewrite_toml_paths(value, source_parent, destination_parent, relocations)?;
            }
            Ok(changed)
        }
        _ => Ok(false),
    }
}

fn relocated_path(
    original: &str,
    source_parent: &Path,
    destination_parent: &Path,
    relocations: &[ResolvedPathRelocation],
) -> Result<Option<String>> {
    let original_path = Path::new(original);
    let candidate = if original_path.is_absolute() {
        original_path.to_owned()
    } else {
        source_parent.join(original_path)
    };
    let candidate = match fs::canonicalize(candidate) {
        Ok(candidate) => candidate,
        Err(_) => return Ok(None),
    };
    let Some(relocation) = relocations
        .iter()
        .find(|relocation| candidate.starts_with(&relocation.source_root))
    else {
        return Ok(None);
    };
    let suffix = candidate.strip_prefix(&relocation.source_root)?;
    let target = relocation.snapshot_root.join(suffix);
    Ok(Some(relative_path(destination_parent, &target)?))
}

fn relative_path(from: &Path, to: &Path) -> Result<String> {
    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        bail!(
            "cannot relocate Cargo path across filesystem roots: {} -> {}",
            from.display(),
            to.display()
        );
    }
    let mut result = PathBuf::new();
    for component in &from_components[common..] {
        if matches!(component, std::path::Component::Normal(_)) {
            result.push("..");
        }
    }
    for component in &to_components[common..] {
        if let std::path::Component::Normal(value) = component {
            result.push(value);
        }
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    Ok(result.to_string_lossy().replace('\\', "/"))
}

fn frozen_input_digest(
    manifest: &Inputs,
    external_inputs: &[FrozenExternalInput],
    path_relocations: &[PathRelocation],
) -> Result<String> {
    #[derive(Serialize)]
    struct ExternalDigest<'a> {
        snapshot_root: &'a str,
        manifest: &'a Inputs,
    }
    #[derive(Serialize)]
    struct DigestInput<'a> {
        manifest: &'a Inputs,
        external_inputs: Vec<ExternalDigest<'a>>,
        snapshot_roots: Vec<&'a str>,
    }
    let digest_input = DigestInput {
        manifest,
        external_inputs: external_inputs
            .iter()
            .map(|external| ExternalDigest {
                snapshot_root: &external.snapshot_root,
                manifest: &external.manifest,
            })
            .collect(),
        snapshot_roots: path_relocations
            .iter()
            .map(|relocation| relocation.snapshot_root.as_str())
            .collect(),
    };
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&digest_input).context("serializing frozen inputs")?)
    ))
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
    fn sensitive_native_config_is_recorded_but_never_hashed_or_frozen() {
        let root = tempfile::tempdir().unwrap();
        let destination_parent = tempfile::tempdir().unwrap();
        let gradle = root.path().join("mobile/android/gradle");
        fs::create_dir_all(&gradle).unwrap();
        fs::write(gradle.join("local.properties"), "sdk.dir=/private/sdk\n").unwrap();
        fs::write(
            gradle.join("keystore.properties"),
            "storePassword=secret-value\n",
        )
        .unwrap();

        let first = Inputs::scan(root.path()).unwrap();
        assert!(first.sources.is_empty());
        assert_eq!(
            first.excluded_sensitive_files,
            vec![
                "mobile/android/gradle/keystore.properties",
                "mobile/android/gradle/local.properties",
            ]
        );
        fs::write(
            gradle.join("keystore.properties"),
            "storePassword=rotated-secret\n",
        )
        .unwrap();
        assert_eq!(first, Inputs::scan(root.path()).unwrap());

        let destination = destination_parent.path().join("snapshot");
        let frozen = Inputs::freeze_to(root.path(), &destination, 1).unwrap();

        assert_eq!(
            frozen.manifest.excluded_sensitive_files,
            first.excluded_sensitive_files
        );
        assert!(snapshot_copy_matches(
            &frozen.manifest,
            &Inputs::scan(&destination).unwrap()
        ));
        assert!(
            !destination
                .join("mobile/android/gradle/local.properties")
                .exists()
        );
        assert!(
            !destination
                .join("mobile/android/gradle/keystore.properties")
                .exists()
        );
        let serialized = serde_json::to_string(&frozen.manifest).unwrap();
        assert!(serialized.contains("keystore.properties"));
        assert!(!serialized.contains("rotated-secret"));
        assert!(!serialized.contains("private/sdk"));
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
    fn native_scope_collects_manifests_and_sources_but_excludes_generated_and_sensitive_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".cargo")).unwrap();
        fs::create_dir_all(root.path().join("mobile/ios/Assets.xcassets/App.imageset")).unwrap();
        fs::create_dir_all(root.path().join("mobile/ios/build")).unwrap();
        fs::create_dir_all(root.path().join("mobile/ios/App.xcodeproj")).unwrap();
        fs::create_dir_all(root.path().join("mobile/android/gradle/app/src/main/res")).unwrap();
        fs::create_dir_all(root.path().join("mobile/android/gradle/app/build")).unwrap();
        fs::create_dir_all(root.path().join("mobile/android/gradle/.gradle")).unwrap();
        fs::create_dir_all(
            root.path()
                .join("mobile/android/gradle/app/src/main/jniLibs"),
        )
        .unwrap();
        fs::create_dir_all(root.path().join("mobile/android/.cargo")).unwrap();
        fs::write(root.path().join("gpui.toml"), "[app]\nname = \"probe\"\n").unwrap();
        fs::write(root.path().join(".cargo/config.toml"), "[build]\n").unwrap();
        fs::write(root.path().join("mobile/ios/project.yml"), "name: Probe\n").unwrap();
        fs::write(root.path().join("mobile/ios/App.swift"), "struct App {}\n").unwrap();
        fs::write(
            root.path()
                .join("mobile/ios/Assets.xcassets/App.imageset/Contents.json"),
            "{}\n",
        )
        .unwrap();
        fs::write(root.path().join("mobile/ios/build/generated"), "ignored\n").unwrap();
        fs::write(
            root.path()
                .join("mobile/android/gradle/app/build.gradle.kts"),
            "plugins {}\n",
        )
        .unwrap();
        fs::write(
            root.path()
                .join("mobile/android/gradle/app/src/main/res/values.xml"),
            "<resources/>\n",
        )
        .unwrap();
        fs::write(
            root.path().join("mobile/android/gradle/local.properties"),
            "sdk.dir=/secret\n",
        )
        .unwrap();
        fs::write(
            root.path().join("mobile/android/.cargo/config.toml"),
            "[target]\n",
        )
        .unwrap();

        let native = NativeInputs::scan(root.path()).unwrap();

        assert!(native.files.contains_key("gpui.toml"));
        assert!(native.files.contains_key("mobile/ios/project.yml"));
        assert!(
            native
                .files
                .contains_key("mobile/ios/Assets.xcassets/App.imageset/Contents.json")
        );
        assert!(
            native
                .files
                .contains_key("mobile/android/gradle/app/build.gradle.kts")
        );
        assert!(
            native
                .files
                .contains_key("mobile/android/.cargo/config.toml")
        );
        assert!(!native.files.contains_key("mobile/ios/build/generated"));
        assert!(
            !native
                .files
                .contains_key("mobile/android/gradle/app/src/main/jniLibs/anything.so")
        );
        assert_eq!(
            native.excluded_sensitive_files,
            vec!["mobile/android/gradle/local.properties"]
        );
        assert!(!native.digest().is_empty());
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
        assert_eq!(
            frozen.input_hash,
            frozen_input_digest(
                &frozen.manifest,
                &frozen.external_inputs,
                &frozen.path_relocations
            )
            .unwrap()
        );
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
        assert!(error.to_string().contains("outside all input roots"));
    }

    #[test]
    fn freeze_to_copies_external_package_and_relocates_cargo_path() {
        let container = tempfile::tempdir().unwrap();
        let workspace = container.path().join("workspace");
        let external = container.path().join("external-lib");
        let destination_parent = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.join("app/src")).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            workspace.join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nexternal-lib = { path = \"../../external-lib\" }\n",
        )
        .unwrap();
        fs::write(workspace.join("app/src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            external.join("Cargo.toml"),
            "[package]\nname = \"external-lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(external.join("lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();

        let external = fs::canonicalize(external).unwrap();
        let scope = CargoInputScope {
            workspace_root: fs::canonicalize(&workspace)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            external_path_dependencies: vec![ExternalPathDependency {
                root: external.to_string_lossy().into_owned(),
                manifest_path: external.join("Cargo.toml").to_string_lossy().into_owned(),
                package_ids: vec!["external-lib".into()],
            }],
        };
        let destination = destination_parent.path().join("snapshot");

        let frozen =
            Inputs::freeze_to_with_cargo_scope(&workspace, &destination, &scope, 1).unwrap();

        assert_eq!(frozen.external_inputs.len(), 1);
        assert_eq!(frozen.path_relocations[0].snapshot_root, "external/0000");
        assert!(destination.join("external/0000/lib.rs").is_file());
        let app_manifest: toml::Value =
            toml::from_str(&fs::read_to_string(destination.join("app/Cargo.toml")).unwrap())
                .unwrap();
        assert_eq!(
            app_manifest["dependencies"]["external-lib"]["path"],
            toml::Value::String("../external/0000".into())
        );
        assert!(
            fs::read_to_string(workspace.join("app/Cargo.toml"))
                .unwrap()
                .contains("../../external-lib")
        );
    }
}
