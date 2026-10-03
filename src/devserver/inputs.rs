//! Content manifests for the files covered by the live project watcher.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

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
const MAX_INDEXED_DIRTY_PATHS: usize = 4096;
const EXTERNAL_INPUT_PREFIX: &str = "\0cargo-external";
const EXTERNAL_INPUT_MANIFEST_PREFIX: &str = "external";
const SENSITIVE_INPUT_FILE_NAMES: &[&str] = &["local.properties", "keystore.properties"];
const SENSITIVE_INPUT_FILE_EXTENSIONS: &[&str] = &["jks", "keystore", "p12", "pfx"];

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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub external_sources: BTreeMap<String, String>,
    // Directory symlinks are not traversed; the manifest advertises this scope.
    pub untracked_directory_links: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_untracked_directory_links: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_sensitive_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_excluded_sensitive_files: Vec<String>,
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
    pub snapshot_root: String,
    pub manifest: Inputs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PathRelocation {
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CargoInputScope {
    pub workspace_root: String,
    pub external_path_dependencies: Vec<ExternalPathDependency>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

/// A filesystem stamp that is safe to use for deciding whether an existing
/// content hash can be reused. The file identity is important for editors
/// that replace a file in place: a path, size and timestamp can otherwise
/// look unchanged while the underlying file has changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedFile {
    pub hash: String,
    pub modified: SystemTime,
    pub size: u64,
    pub identity: file_id::FileId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexedRefreshKind {
    Cached,
    InitialFullScan,
    Incremental,
    FallbackFullScan,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexedRefresh {
    pub kind: Option<IndexedRefreshKind>,
    /// Distinct project input paths whose content hash was refreshed.
    pub hashed_files: usize,
    /// Content bytes actually read for hashes during this refresh. A stable
    /// dirty-file hash reads the file twice by design.
    pub hashed_bytes: u64,
    pub reused_files: usize,
    pub removed_files: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputScanMetrics {
    pub passes: usize,
    pub hashed_files: usize,
    pub hashed_bytes: u64,
}

/// Metadata-assisted input index used by the live watcher.
///
/// Watcher callbacks only call [`Self::mark_dirty`] or
/// [`Self::mark_renamed`]. They never read the project. A later refresh walks
/// only the dirty subtrees, reuses hashes whose metadata and identity are
/// unchanged, and removes entries that disappeared from a dirty subtree.
/// Initial refreshes, overflow, unsupported file identities, and read races
/// use the stable full scanner instead.
#[derive(Debug)]
pub struct IndexedInputs {
    root: PathBuf,
    roots: Vec<IndexedRoot>,
    entries: BTreeMap<String, IndexedFile>,
    untracked_directory_links: BTreeSet<String>,
    excluded_sensitive_files: BTreeSet<String>,
    manifest: Inputs,
    dirty: BTreeSet<String>,
    initialized: bool,
    force_full_scan: bool,
    index_disabled: bool,
    incremental_enabled: bool,
    incremental_disabled_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexedRoot {
    prefix: String,
    filesystem_root: PathBuf,
    full_scan_on_change: bool,
}

fn workspace_indexed_root(root: &Path) -> IndexedRoot {
    IndexedRoot {
        prefix: String::new(),
        filesystem_root: root.to_owned(),
        full_scan_on_change: false,
    }
}

impl IndexedInputs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = fs::canonicalize(&root).unwrap_or(root);
        let incremental_disabled_reason = filesystem_incremental_policy(&root);
        Self {
            roots: vec![workspace_indexed_root(&root)],
            root,
            entries: BTreeMap::new(),
            untracked_directory_links: BTreeSet::new(),
            excluded_sensitive_files: BTreeSet::new(),
            manifest: Inputs::default(),
            dirty: BTreeSet::new(),
            initialized: false,
            force_full_scan: false,
            index_disabled: false,
            incremental_enabled: incremental_disabled_reason.is_none(),
            incremental_disabled_reason,
        }
    }

    pub fn new_with_cargo_scope(root: &Path, scope: &CargoInputScope) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let mut index = Self::new(root.clone());
        index.roots = indexed_roots_for_cargo_scope(&root, scope)?;
        index.refresh_filesystem_policy();
        Ok(index)
    }

    /// Replaces the Cargo path-package root set while retaining the last
    /// published manifest so the next full scan can produce an exact revision
    /// and asset delta.
    pub fn refresh_cargo_scope(&mut self, scope: &CargoInputScope) -> Result<bool> {
        let roots = indexed_roots_for_cargo_scope(&self.root, scope)?;
        if roots == self.roots {
            return Ok(false);
        }
        self.roots = roots;
        self.entries.clear();
        self.untracked_directory_links.clear();
        self.excluded_sensitive_files.clear();
        self.dirty.clear();
        self.initialized = false;
        self.force_full_scan = false;
        self.index_disabled = false;
        self.refresh_filesystem_policy();
        Ok(true)
    }

    pub fn watch_roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .map(|root| root.filesystem_root.clone())
            .collect()
    }

    pub fn manifest(&self) -> &Inputs {
        &self.manifest
    }

    pub fn entries(&self) -> &BTreeMap<String, IndexedFile> {
        &self.entries
    }

    pub fn is_index_disabled(&self) -> bool {
        self.index_disabled || !self.incremental_enabled
    }

    pub fn incremental_disabled_reason(&self) -> Option<&str> {
        self.incremental_disabled_reason.as_deref()
    }

    /// Permanently disables metadata reuse for this index instance. Use this
    /// for filesystems whose timestamps or identities are not trustworthy;
    /// every refresh then uses the stable full content scanner.
    pub fn disable_incremental(&mut self) {
        self.incremental_enabled = false;
        self.incremental_disabled_reason = Some("filesystem metadata is not trusted".into());
        self.force_full_scan = true;
    }

    fn refresh_filesystem_policy(&mut self) {
        self.incremental_disabled_reason = self
            .roots
            .iter()
            .find_map(|root| filesystem_incremental_policy(&root.filesystem_root));
        self.incremental_enabled = self.incremental_disabled_reason.is_none();
    }

    /// Records a watcher path without touching the filesystem.
    pub fn mark_dirty(&mut self, path: impl AsRef<Path>) {
        let Some(mut relative) = self.relative_dirty_path(path.as_ref()) else {
            self.force_full_scan = true;
            return;
        };
        if let Some((root, _)) = self.root_for_logical_path(&relative)
            && root.full_scan_on_change
        {
            relative = root.prefix.clone();
        }
        if !should_trigger(Path::new(&relative)) {
            return;
        }
        if self.dirty.len() >= MAX_INDEXED_DIRTY_PATHS && !self.dirty.contains(&relative) {
            self.dirty.clear();
            self.force_full_scan = true;
            return;
        }
        self.dirty.insert(relative);
    }

    /// Invalidates both sides of a rename. This also covers a directory move:
    /// the old subtree is removed during refresh and the new subtree is
    /// scanned, so a late event for either path cannot leave stale entries.
    pub fn mark_renamed(&mut self, from: impl AsRef<Path>, to: impl AsRef<Path>) {
        self.mark_dirty(from);
        self.mark_dirty(to);
    }

    /// Marks the watcher stream as unreliable. The next refresh performs a
    /// stable full scan, which is the safe response to notify overflow or a
    /// backend that cannot report complete paths.
    pub fn mark_overflow(&mut self) {
        self.force_full_scan = true;
    }

    /// Revalidates the entire input tree regardless of the watcher index. Used
    /// by explicit synchronization boundaries such as observe --sync/build.
    pub fn verify_full(&mut self, max_rescans: usize) -> Result<IndexedRefresh> {
        self.force_full_scan = true;
        self.refresh(max_rescans)
    }

    pub fn refresh(&mut self, max_rescans: usize) -> Result<IndexedRefresh> {
        if !self.initialized
            || self.force_full_scan
            || self.index_disabled
            || !self.incremental_enabled
        {
            let kind = if self.initialized {
                IndexedRefreshKind::FallbackFullScan
            } else {
                IndexedRefreshKind::InitialFullScan
            };
            return self.refresh_full(max_rescans, kind);
        }
        if self.dirty.is_empty() {
            return Ok(IndexedRefresh {
                kind: Some(IndexedRefreshKind::Cached),
                ..IndexedRefresh::default()
            });
        }

        let dirty = self.coalesced_dirty_paths();
        match self.refresh_dirty(&dirty, max_rescans) {
            Ok(refresh) => {
                self.dirty.clear();
                Ok(refresh)
            }
            Err(_error) => {
                self.force_full_scan = true;
                self.refresh_full(max_rescans, IndexedRefreshKind::FallbackFullScan)
            }
        }
    }

    fn refresh_full(
        &mut self,
        max_rescans: usize,
        kind: IndexedRefreshKind,
    ) -> Result<IndexedRefresh> {
        let previous_paths = indexed_manifest_paths(&self.manifest);
        let (manifest, entries, metrics) = if self.incremental_enabled && !self.index_disabled {
            match scan_indexed_roots_stable(&self.roots, max_rescans) {
                Ok(scanned) => (scanned.manifest, Some(scanned.entries), scanned.metrics),
                Err(_) => {
                    let (manifest, metrics) = scan_input_roots_stable(&self.roots, max_rescans)?;
                    (manifest, None, metrics)
                }
            }
        } else {
            let (manifest, metrics) = scan_input_roots_stable(&self.roots, max_rescans)?;
            (manifest, None, metrics)
        };
        let hashed_files =
            manifest.sources.len() + manifest.assets.len() + manifest.external_sources.len();
        let current_paths = indexed_manifest_paths(&manifest);
        self.manifest = manifest;
        self.entries = entries.unwrap_or_default();
        self.untracked_directory_links = self
            .manifest
            .untracked_directory_links
            .iter()
            .cloned()
            .chain(
                self.manifest
                    .external_untracked_directory_links
                    .iter()
                    .map(|path| internal_input_name(path)),
            )
            .collect();
        self.excluded_sensitive_files = self
            .manifest
            .excluded_sensitive_files
            .iter()
            .cloned()
            .chain(
                self.manifest
                    .external_excluded_sensitive_files
                    .iter()
                    .map(|path| internal_input_name(path)),
            )
            .collect();
        self.index_disabled = self.entries.is_empty() && !current_paths.is_empty();
        self.initialized = true;
        self.force_full_scan = false;
        self.dirty.clear();

        Ok(IndexedRefresh {
            kind: Some(kind),
            hashed_files,
            hashed_bytes: metrics.hashed_bytes,
            reused_files: 0,
            removed_files: previous_paths.difference(&current_paths).count(),
        })
    }

    fn refresh_dirty(&mut self, dirty: &[String], max_rescans: usize) -> Result<IndexedRefresh> {
        let mut refresh = IndexedRefresh {
            kind: Some(IndexedRefreshKind::Incremental),
            ..IndexedRefresh::default()
        };
        for relative in dirty {
            let (root, local_relative) = self
                .root_for_logical_path(relative)
                .map(|(root, local_relative)| (root.clone(), local_relative))
                .ok_or_else(|| {
                    anyhow::anyhow!("dirty path is outside indexed roots: {relative}")
                })?;
            if root.full_scan_on_change && local_relative.is_empty() {
                let scanned = scan_indexed_roots_stable(std::slice::from_ref(&root), max_rescans)?;
                let old_entries = self.take_entries_under(&root.prefix);
                self.take_special_paths_under(&root.prefix);
                let current_paths = scanned.entries.keys().cloned().collect::<BTreeSet<_>>();
                refresh.removed_files += old_entries
                    .keys()
                    .filter(|path| !current_paths.contains(*path))
                    .count();
                refresh.hashed_files += current_paths.len();
                refresh.hashed_bytes = refresh
                    .hashed_bytes
                    .saturating_add(scanned.metrics.hashed_bytes);
                self.entries.extend(scanned.entries);
                self.untracked_directory_links
                    .retain(|path| !path_is_under(path, &root.prefix));
                self.untracked_directory_links
                    .extend(scanned.manifest.untracked_directory_links);
                self.excluded_sensitive_files
                    .retain(|path| !path_is_under(path, &root.prefix));
                self.excluded_sensitive_files
                    .extend(scanned.manifest.excluded_sensitive_files);
                continue;
            }
            let old_entries = self.take_entries_under(relative);
            self.take_special_paths_under(relative);
            let prefix = root.prefix;
            let filesystem_root = root.filesystem_root;
            let path = filesystem_root.join(&local_relative);
            self.scan_dirty_path(
                &prefix,
                &filesystem_root,
                &local_relative,
                &path,
                old_entries,
                &mut refresh,
            )?;
        }
        self.rebuild_manifest();
        Ok(refresh)
    }

    fn scan_dirty_path(
        &mut self,
        prefix: &str,
        filesystem_root: &Path,
        local_relative: &str,
        path: &Path,
        mut old_entries: BTreeMap<String, IndexedFile>,
        refresh: &mut IndexedRefresh,
    ) -> Result<()> {
        let kind = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata.file_type(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                refresh.removed_files += old_entries.len();
                return Ok(());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading dirty input {}", path.display()));
            }
        };
        if kind.is_dir() {
            let mut pending = vec![path.to_owned()];
            while let Some(dir) = pending.pop() {
                let directory_before = read_directory_stamp(&dir)?;
                let entries = fs::read_dir(&dir)
                    .with_context(|| format!("reading dirty inputs in {}", dir.display()))?;
                let entries = entries.collect::<std::io::Result<Vec<_>>>()?;
                let mut names_before = entries
                    .iter()
                    .map(|entry| entry.file_name())
                    .collect::<Vec<_>>();
                names_before.sort();
                for entry in entries {
                    let child = entry.path();
                    let child_relative = child.strip_prefix(filesystem_root)?;
                    if !should_trigger(child_relative) {
                        continue;
                    }
                    let child_name = logical_input_name(prefix, child_relative);
                    let child_kind = entry.file_type()?;
                    if child_kind.is_dir() {
                        pending.push(child);
                    } else if child_kind.is_symlink() && child.is_dir() {
                        self.untracked_directory_links.insert(child_name);
                    } else if child_kind.is_file() || child_kind.is_symlink() {
                        self.scan_dirty_file(&child_name, &child, &mut old_entries, true, refresh)?;
                    }
                }
                if directory_before != read_directory_stamp(&dir)? {
                    bail!(
                        "directory changed while refreshing inputs: {}",
                        dir.display()
                    );
                }
                let entries_after = fs::read_dir(&dir)
                    .with_context(|| format!("rechecking dirty inputs in {}", dir.display()))?;
                let mut names_after = entries_after
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect::<std::io::Result<Vec<_>>>()?;
                names_after.sort();
                if names_before != names_after {
                    bail!(
                        "directory entries changed while refreshing inputs: {}",
                        dir.display()
                    );
                }
            }
        } else if kind.is_symlink() && path.is_dir() {
            self.untracked_directory_links
                .insert(logical_input_name(prefix, Path::new(local_relative)));
        } else if kind.is_file() || kind.is_symlink() {
            self.scan_dirty_file(
                &logical_input_name(prefix, Path::new(local_relative)),
                path,
                &mut old_entries,
                true,
                refresh,
            )?;
        }
        refresh.removed_files += old_entries.len();
        Ok(())
    }

    fn scan_dirty_file(
        &mut self,
        relative: &str,
        path: &Path,
        old_entries: &mut BTreeMap<String, IndexedFile>,
        force_hash: bool,
        refresh: &mut IndexedRefresh,
    ) -> Result<()> {
        if is_sensitive_input_file(path) {
            self.excluded_sensitive_files.insert(relative.to_owned());
            if old_entries.remove(relative).is_some() {
                refresh.removed_files += 1;
            }
            return Ok(());
        }
        let Some(old) = old_entries.remove(relative) else {
            let (hash, before) = hash_input_file_stable(path)?;
            self.entries.insert(
                relative.to_owned(),
                IndexedFile {
                    hash,
                    modified: before.modified,
                    size: before.size,
                    identity: before.identity,
                },
            );
            refresh.hashed_files += 1;
            refresh.hashed_bytes += before.size.saturating_mul(2);
            return Ok(());
        };
        let stamp = read_file_stamp(path)?;
        if !force_hash && same_file_stamp(&old, &stamp) {
            self.entries.insert(relative.to_owned(), old);
            refresh.reused_files += 1;
            return Ok(());
        }
        let (hash, stable_stamp) = hash_input_file_stable(path)?;
        if stamp != stable_stamp {
            bail!(
                "input changed while incrementally hashing: {}",
                path.display()
            );
        }
        self.entries.insert(
            relative.to_owned(),
            IndexedFile {
                hash,
                modified: stable_stamp.modified,
                size: stable_stamp.size,
                identity: stable_stamp.identity,
            },
        );
        refresh.hashed_files += 1;
        refresh.hashed_bytes += stable_stamp.size.saturating_mul(2);
        Ok(())
    }

    fn take_entries_under(&mut self, relative: &str) -> BTreeMap<String, IndexedFile> {
        let matching = self
            .entries
            .keys()
            .filter(|path| path_is_under(path, relative))
            .cloned()
            .collect::<Vec<_>>();
        let mut old = BTreeMap::new();
        for path in matching {
            if let Some(entry) = self.entries.remove(&path) {
                old.insert(path, entry);
            }
        }
        old
    }

    fn take_special_paths_under(&mut self, relative: &str) {
        self.untracked_directory_links
            .retain(|path| !path_is_under(path, relative));
        self.excluded_sensitive_files
            .retain(|path| !path_is_under(path, relative));
    }

    fn rebuild_manifest(&mut self) {
        let mut manifest = Inputs::default();
        for path in &self.untracked_directory_links {
            let external = external_manifest_name(path);
            if let Some(external) = external {
                manifest.external_untracked_directory_links.push(external);
            } else {
                manifest.untracked_directory_links.push(path.clone());
            }
        }
        for path in &self.excluded_sensitive_files {
            let external = external_manifest_name(path);
            if let Some(external) = external {
                manifest.external_excluded_sensitive_files.push(external);
            } else {
                manifest.excluded_sensitive_files.push(path.clone());
            }
        }
        for (path, entry) in &self.entries {
            if let Some(external_path) = path.strip_prefix(&format!("{EXTERNAL_INPUT_PREFIX}/")) {
                manifest.external_sources.insert(
                    format!("{EXTERNAL_INPUT_MANIFEST_PREFIX}/{external_path}"),
                    entry.hash.clone(),
                );
            } else if path.starts_with("assets/") {
                manifest.assets.insert(path.clone(), entry.hash.clone());
            } else {
                manifest.sources.insert(path.clone(), entry.hash.clone());
            }
        }
        self.manifest = manifest;
    }

    fn coalesced_dirty_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = Vec::new();
        for path in &self.dirty {
            if paths.iter().any(|parent| path_is_under(path, parent)) {
                continue;
            }
            paths.retain(|existing| !path_is_under(existing, path));
            paths.push(path.clone());
        }
        paths
    }

    fn relative_dirty_path(&self, path: &Path) -> Option<String> {
        let candidate = if path.is_absolute() {
            path.to_owned()
        } else {
            self.root.join(path)
        };
        let candidate = normalize_watcher_path(&candidate)?;
        let indexed_root = self.roots.iter().find(|root| {
            candidate == root.filesystem_root || candidate.starts_with(&root.filesystem_root)
        })?;
        let relative = candidate.strip_prefix(&indexed_root.filesystem_root).ok()?;
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return None;
        }
        Some(logical_input_name(&indexed_root.prefix, relative))
    }

    fn root_for_logical_path(&self, logical: &str) -> Option<(&IndexedRoot, String)> {
        if logical.is_empty() {
            return self.roots.first().map(|root| (root, String::new()));
        }
        for root in &self.roots {
            if root.prefix.is_empty() {
                if !logical.starts_with(&format!("{EXTERNAL_INPUT_PREFIX}/")) {
                    return Some((root, logical.to_owned()));
                }
            } else if logical == root.prefix {
                return Some((root, String::new()));
            } else if let Some(suffix) = logical.strip_prefix(&(root.prefix.clone() + "/")) {
                return Some((root, suffix.to_owned()));
            }
        }
        None
    }
}

fn normalize_watcher_path(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = fs::canonicalize(path) {
        return Some(path);
    }
    let parent = path.parent()?;
    let file_name = path.file_name()?;
    Some(fs::canonicalize(parent).ok()?.join(file_name))
}

fn indexed_roots_for_cargo_scope(root: &Path, scope: &CargoInputScope) -> Result<Vec<IndexedRoot>> {
    let scope_root = fs::canonicalize(Path::new(&scope.workspace_root))
        .context("resolving Cargo input scope workspace root")?;
    if scope_root != root {
        bail!("Cargo input scope workspace root does not match indexed input root");
    }
    let mut roots = vec![workspace_indexed_root(root)];
    for (index, external) in resolve_external_roots(root, scope)?.into_iter().enumerate() {
        roots.push(IndexedRoot {
            prefix: format!("{EXTERNAL_INPUT_PREFIX}/{index:04}"),
            filesystem_root: external.source_root,
            full_scan_on_change: external.full_scan_on_change,
        });
    }
    Ok(roots)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    size: u64,
    identity: file_id::FileId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryStamp {
    modified: SystemTime,
    size: u64,
    identity: file_id::FileId,
}

fn read_directory_stamp(path: &Path) -> Result<DirectoryStamp> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("reading input directory metadata: {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("input directory changed type: {}", path.display());
    }
    let modified = metadata
        .modified()
        .with_context(|| format!("reading input directory mtime: {}", path.display()))?;
    let identity = file_id::get_file_id(path)
        .with_context(|| format!("reading input directory identity: {}", path.display()))?;
    Ok(DirectoryStamp {
        modified,
        size: metadata.len(),
        identity,
    })
}

/// Metadata reuse is permitted only when the host can positively identify a
/// local filesystem type with stable file identities and timestamps. Unknown
/// and network/userspace mounts remain usable, but every watcher refresh falls
/// back to hashing the complete declared input roots.
fn filesystem_incremental_policy(path: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let path_string = match CString::new(path.as_os_str().as_encoded_bytes()) {
            Ok(path) => path,
            Err(_) => {
                return Some(
                    "filesystem path cannot be identified; metadata reuse disabled".into(),
                );
            }
        };
        let mut stats = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: `statfs` receives a valid C string and writable result.
        if unsafe { libc::statfs(path_string.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return Some("could not identify filesystem; metadata reuse disabled".into());
        }
        let stats = unsafe { stats.assume_init() };
        let fs_name = unsafe { std::ffi::CStr::from_ptr(stats.f_fstypename.as_ptr()) }
            .to_string_lossy()
            .to_ascii_lowercase();
        return macos_filesystem_policy(&fs_name);
    }
    #[cfg(target_os = "linux")]
    {
        let path_string = match CString::new(path.as_os_str().as_encoded_bytes()) {
            Ok(path) => path,
            Err(_) => {
                return Some(
                    "filesystem path cannot be identified; metadata reuse disabled".into(),
                );
            }
        };
        let mut stats = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: `statfs` receives a valid C string and writable result.
        if unsafe { libc::statfs(path_string.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return Some("could not identify filesystem; metadata reuse disabled".into());
        }
        let fs_type = unsafe { stats.assume_init() }.f_type as libc::c_long;
        return linux_filesystem_policy(fs_type as u64);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};

        if windows_path_is_unc(path) {
            return Some("UNC/network filesystem; metadata reuse disabled".into());
        }
        let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        path_wide.push(0);
        let mut volume_root = vec![0_u16; 32768];
        // SAFETY: both buffers are NUL-terminated/writable UTF-16 buffers.
        if unsafe {
            GetVolumePathNameW(
                path_wide.as_ptr(),
                volume_root.as_mut_ptr(),
                volume_root.len() as u32,
            )
        } == 0
        {
            let Some(fallback_root) = windows_drive_root(path) else {
                return Some(
                    "filesystem volume could not be identified; metadata reuse disabled".into(),
                );
            };
            return windows_drive_policy(unsafe { GetDriveTypeW(fallback_root.as_ptr()) });
        }
        let drive_type = unsafe { GetDriveTypeW(volume_root.as_ptr()) };
        return windows_drive_policy(drive_type);
    }
    #[allow(unreachable_code)]
    Some("filesystem type cannot be reliably detected on this platform".into())
}

#[cfg(target_os = "macos")]
fn macos_filesystem_policy(name: &str) -> Option<String> {
    match name {
        "apfs" | "hfs" => None,
        "nfs" | "smbfs" | "afpfs" | "webdav" | "macfuse" | "osxfuse" => Some(format!(
            "network or userspace filesystem {name}; metadata reuse disabled"
        )),
        _ => Some(format!(
            "unrecognized filesystem {name}; metadata reuse disabled"
        )),
    }
}

#[cfg(target_os = "windows")]
fn windows_drive_policy(drive_type: u32) -> Option<String> {
    use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOTE};

    match drive_type {
        DRIVE_FIXED => None,
        DRIVE_REMOTE => Some("network drive; metadata reuse disabled".into()),
        _ => Some(format!(
            "non-fixed or unknown drive type {drive_type}; metadata reuse disabled"
        )),
    }
}

#[cfg(target_os = "windows")]
fn windows_drive_root(path: &Path) -> Option<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};

    let Component::Prefix(prefix) = path.components().next()? else {
        return None;
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
        _ => return None,
    };
    let root = format!("{}:\\", char::from(drive));
    Some(
        std::ffi::OsStr::new(&root)
            .encode_wide()
            .chain([0])
            .collect(),
    )
}

#[cfg(target_os = "windows")]
fn windows_path_is_unc(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    )
}

#[cfg(target_os = "linux")]
fn linux_filesystem_policy(file_system_type: u64) -> Option<String> {
    match file_system_type as libc::c_long {
        libc::EXT4_SUPER_MAGIC
        | libc::XFS_SUPER_MAGIC
        | libc::BTRFS_SUPER_MAGIC
        | libc::TMPFS_MAGIC
        | libc::OVERLAYFS_SUPER_MAGIC
        | libc::F2FS_SUPER_MAGIC
        | 0x00004d44 // MSDOS/VFAT
        | 0x2011_bab0 // exFAT
        | 0x5346_544e // NTFS
        | 0x00009660 // ISO 9660
        | 0x1501_3346 // UDF
        | 0x2fc1_2fc1 // ZFS
        | 0x7371_7368 // SquashFS
        | 0x28cd_3d45 // cramfs
        | 0x3153_464a // JFS
        | 0x5265_4973 // ReiserFS
        | 0x3434 // NILFS2
        => None,
        libc::NFS_SUPER_MAGIC
        | libc::SMB_SUPER_MAGIC
        | libc::CODA_SUPER_MAGIC
        | libc::FUSE_SUPER_MAGIC
        | libc::AFS_SUPER_MAGIC
        | 0x0102_1997 // 9p
        | 0x7472_6976 // virtiofs
        | 0x00c3_6400 // Ceph
        | 0x0bd0_0bd0 // Lustre
        | 0xff53_4d42 // CIFS
        => Some(format!(
            "network or userspace filesystem type {file_system_type:#x}; metadata reuse disabled"
        )),
        _ => Some(format!(
            "unrecognized filesystem type {file_system_type:#x}; metadata reuse disabled"
        )),
    }
}

struct ScannedIndex {
    manifest: Inputs,
    entries: BTreeMap<String, IndexedFile>,
    metrics: InputScanMetrics,
}

fn scan_indexed_roots_stable(roots: &[IndexedRoot], max_rescans: usize) -> Result<ScannedIndex> {
    let mut previous = scan_indexed_roots_once(roots)?;
    let mut metrics = previous.metrics;
    for _ in 0..=max_rescans {
        let current = scan_indexed_roots_once(roots)?;
        metrics.passes += current.metrics.passes;
        metrics.hashed_files += current.metrics.hashed_files;
        metrics.hashed_bytes = metrics
            .hashed_bytes
            .saturating_add(current.metrics.hashed_bytes);
        if current.manifest == previous.manifest {
            return Ok(ScannedIndex { metrics, ..current });
        }
        previous = current;
    }
    bail!(
        "project inputs changed during the bounded indexed scan after {} rescans",
        max_rescans
    )
}

fn scan_indexed_roots_once(roots: &[IndexedRoot]) -> Result<ScannedIndex> {
    let mut combined = ScannedIndex {
        manifest: Inputs::default(),
        entries: BTreeMap::new(),
        metrics: InputScanMetrics {
            passes: 1,
            ..InputScanMetrics::default()
        },
    };
    for root in roots {
        let scanned = scan_indexed_root_once(root).map_err(|error| {
            if root.prefix.is_empty() {
                error
            } else {
                anyhow::anyhow!(
                    "external Cargo path-package input scan failed; absolute paths omitted"
                )
            }
        })?;
        merge_input_manifests(&mut combined.manifest, scanned.manifest)?;
        for (path, entry) in scanned.entries {
            if combined.entries.insert(path.clone(), entry).is_some() {
                bail!("logical input roots overlap at {path}");
            }
        }
        combined.metrics.hashed_files += scanned.metrics.hashed_files;
        combined.metrics.hashed_bytes = combined
            .metrics
            .hashed_bytes
            .saturating_add(scanned.metrics.hashed_bytes);
    }
    Ok(combined)
}

fn scan_indexed_root_once(root: &IndexedRoot) -> Result<ScannedIndex> {
    let mut manifest = Inputs::default();
    let mut entries = BTreeMap::new();
    let mut metrics = InputScanMetrics {
        passes: 1,
        ..InputScanMetrics::default()
    };
    let mut pending = vec![root.filesystem_root.clone()];
    while let Some(dir) = pending.pop() {
        let children = match fs::read_dir(&dir) {
            Ok(children) => children,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && root.prefix.is_empty() =>
            {
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading inputs in {}", dir.display()));
            }
        };
        for child in children {
            let child = child?;
            let path = child.path();
            let relative = path.strip_prefix(&root.filesystem_root)?;
            if !should_trigger(relative) {
                continue;
            }
            let name = logical_input_name(&root.prefix, relative);
            let kind = child.file_type()?;
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if kind.is_symlink() && path.is_dir() {
                manifest.untracked_directory_links.push(name);
                continue;
            }
            if !kind.is_file() && !kind.is_symlink() {
                continue;
            }
            if is_sensitive_input_file(&path) {
                manifest.excluded_sensitive_files.push(name);
                continue;
            }
            let before = read_file_stamp(&path)?;
            let hash = hash_input_file(&path)?;
            let after = read_file_stamp(&path)?;
            if before != after {
                bail!("input changed while indexing: {}", path.display());
            }
            if name.starts_with("assets/") {
                manifest.assets.insert(name.clone(), hash.clone());
            } else {
                manifest.sources.insert(name.clone(), hash.clone());
            }
            metrics.hashed_files += 1;
            metrics.hashed_bytes = metrics.hashed_bytes.saturating_add(before.size);
            entries.insert(
                name,
                IndexedFile {
                    hash,
                    modified: before.modified,
                    size: before.size,
                    identity: before.identity,
                },
            );
        }
    }
    manifest.untracked_directory_links.sort();
    manifest.excluded_sensitive_files.sort();
    Ok(ScannedIndex {
        manifest,
        entries,
        metrics,
    })
}

fn merge_input_manifests(target: &mut Inputs, source: Inputs) -> Result<()> {
    for (path, hash) in source.sources {
        if let Some(external_path) = path.strip_prefix(&format!("{EXTERNAL_INPUT_PREFIX}/")) {
            let logical_path = format!("{EXTERNAL_INPUT_MANIFEST_PREFIX}/{external_path}");
            if target
                .external_sources
                .insert(logical_path.clone(), hash)
                .is_some()
            {
                bail!("multiple indexed roots map to the same external input: {logical_path}");
            }
        } else if target.sources.insert(path.clone(), hash).is_some()
            || target.assets.contains_key(&path)
        {
            bail!("multiple indexed roots map to the same logical input: {path}");
        }
    }
    for (path, hash) in source.assets {
        if target.assets.insert(path.clone(), hash).is_some() || target.sources.contains_key(&path)
        {
            bail!("multiple indexed roots map to the same logical input: {path}");
        }
    }
    for path in source.untracked_directory_links {
        if let Some(external) = external_manifest_name(&path) {
            target.external_untracked_directory_links.push(external);
        } else {
            target.untracked_directory_links.push(path);
        }
    }
    for path in source.excluded_sensitive_files {
        if let Some(external) = external_manifest_name(&path) {
            target.external_excluded_sensitive_files.push(external);
        } else {
            target.excluded_sensitive_files.push(path);
        }
    }
    target.untracked_directory_links.sort();
    target.external_untracked_directory_links.sort();
    target.excluded_sensitive_files.sort();
    target.external_excluded_sensitive_files.sort();
    Ok(())
}

fn logical_input_name(prefix: &str, relative: &Path) -> String {
    let relative = input_relative_name(relative);
    if prefix.is_empty() {
        relative
    } else if relative.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}/{relative}")
    }
}

fn external_manifest_name(path: &str) -> Option<String> {
    path.strip_prefix(&format!("{EXTERNAL_INPUT_PREFIX}/"))
        .map(|suffix| format!("{EXTERNAL_INPUT_MANIFEST_PREFIX}/{suffix}"))
}

fn internal_input_name(path: &str) -> String {
    path.strip_prefix(&format!("{EXTERNAL_INPUT_MANIFEST_PREFIX}/"))
        .map(|suffix| format!("{EXTERNAL_INPUT_PREFIX}/{suffix}"))
        .unwrap_or_else(|| path.to_owned())
}

fn indexed_manifest_paths(manifest: &Inputs) -> BTreeSet<String> {
    manifest
        .sources
        .keys()
        .chain(manifest.assets.keys())
        .cloned()
        .chain(manifest.external_sources.keys().filter_map(|path| {
            path.strip_prefix(&format!("{EXTERNAL_INPUT_MANIFEST_PREFIX}/"))
                .map(|suffix| format!("{EXTERNAL_INPUT_PREFIX}/{suffix}"))
        }))
        .collect()
}

fn scan_input_roots_stable(
    roots: &[IndexedRoot],
    max_rescans: usize,
) -> Result<(Inputs, InputScanMetrics)> {
    let (mut previous, mut metrics) = scan_input_roots_once(roots)?;
    for _ in 0..=max_rescans {
        let (current, pass_metrics) = scan_input_roots_once(roots)?;
        metrics.passes += pass_metrics.passes;
        metrics.hashed_files += pass_metrics.hashed_files;
        metrics.hashed_bytes = metrics
            .hashed_bytes
            .saturating_add(pass_metrics.hashed_bytes);
        if current == previous {
            return Ok((current, metrics));
        }
        previous = current;
    }
    bail!(
        "project inputs changed during the bounded stability scan after {} rescans",
        max_rescans
    )
}

fn scan_input_roots_once(roots: &[IndexedRoot]) -> Result<(Inputs, InputScanMetrics)> {
    let mut manifest = Inputs::default();
    let mut metrics = InputScanMetrics {
        passes: 1,
        ..InputScanMetrics::default()
    };
    for root in roots {
        let (partial, partial_metrics) = scan_input_root(root).map_err(|error| {
            if root.prefix.is_empty() {
                error
            } else {
                anyhow::anyhow!(
                    "external Cargo path-package input scan failed; absolute paths omitted"
                )
            }
        })?;
        merge_input_manifests(&mut manifest, partial)?;
        metrics.hashed_files += partial_metrics.hashed_files;
        metrics.hashed_bytes = metrics
            .hashed_bytes
            .saturating_add(partial_metrics.hashed_bytes);
    }
    Ok((manifest, metrics))
}

fn scan_input_root(root: &IndexedRoot) -> Result<(Inputs, InputScanMetrics)> {
    let mut result = Inputs::default();
    let mut pending = vec![root.filesystem_root.clone()];
    let mut buffer = [0u8; 32 * 1024];
    let mut metrics = InputScanMetrics {
        passes: 1,
        ..InputScanMetrics::default()
    };
    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && root.prefix.is_empty() =>
            {
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading inputs in {}", dir.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(&root.filesystem_root)?;
            if !should_trigger(relative) {
                continue;
            }
            let name = logical_input_name(&root.prefix, relative);
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
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("reading input {}", path.display()));
                }
            };
            let mut hash = Sha256::new();
            let mut file_bytes = 0_u64;
            loop {
                let len = file.read(&mut buffer)?;
                if len == 0 {
                    break;
                }
                hash.update(&buffer[..len]);
                file_bytes = file_bytes.saturating_add(len as u64);
            }
            metrics.hashed_files += 1;
            metrics.hashed_bytes = metrics.hashed_bytes.saturating_add(file_bytes);
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
    Ok((result, metrics))
}

fn read_file_stamp(path: &Path) -> Result<FileStamp> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("reading input metadata: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("input is no longer a regular file: {}", path.display());
    }
    let modified = metadata
        .modified()
        .with_context(|| format!("reading input mtime: {}", path.display()))?;
    let identity = file_id::get_file_id(path)
        .with_context(|| format!("reading input identity: {}", path.display()))?;
    Ok(FileStamp {
        modified,
        size: metadata.len(),
        identity,
    })
}

fn same_file_stamp(entry: &IndexedFile, stamp: &FileStamp) -> bool {
    entry.modified == stamp.modified && entry.size == stamp.size && entry.identity == stamp.identity
}

fn hash_input_file(path: &Path) -> Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("reading input {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let len = file.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hash.update(&buffer[..len]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn hash_input_file_stable(path: &Path) -> Result<(String, FileStamp)> {
    let before = read_file_stamp(path)?;
    let first = hash_input_file(path)?;
    let middle = read_file_stamp(path)?;
    let second = hash_input_file(path)?;
    let after = read_file_stamp(path)?;
    if before != middle || middle != after || first != second {
        bail!(
            "input changed while incrementally hashing: {}",
            path.display()
        );
    }
    Ok((second, after))
}

fn input_relative_name(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn path_is_under(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
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
                    if is_sensitive_input_file(&path) {
                        result
                            .excluded_sensitive_files
                            .push(relative.to_string_lossy().replace('\\', "/"));
                        continue;
                    }
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
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    SENSITIVE_INPUT_FILE_NAMES.contains(&name)
        || path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| {
                SENSITIVE_INPUT_FILE_EXTENSIONS
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(extension))
            })
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

        let mut external_roots = resolve_external_roots(&root, scope)?;
        let manifest = Self::scan_stable(&root, max_rescans)?;
        reject_untracked_links("workspace", &manifest)?;
        assign_external_snapshot_roots(&mut external_roots, &manifest);
        let destination = prepare_snapshot_destination(&root, &external_roots, destination)?;
        let external_manifests = external_roots
            .iter()
            .map(|external| {
                let manifest =
                    Self::scan_stable(&external.source_root, max_rescans).map_err(|_| {
                        anyhow::anyhow!("external Cargo input scan failed; absolute paths omitted")
                    })?;
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
                copy_manifest_files(&external.source_root, &target, manifest).map_err(|_| {
                    anyhow::anyhow!(
                        "copying external Cargo package inputs failed; absolute paths omitted"
                    )
                })?;
                let copied = Self::scan(&target)?;
                if !snapshot_copy_matches(manifest, &copied) {
                    bail!("frozen external package copy does not match its manifest");
                }
            }

            let current = Self::scan_stable(&root, max_rescans)?;
            if current != manifest {
                bail!("source inputs changed while freezing the snapshot");
            }
            for (external, expected) in external_roots.iter().zip(&external_manifests) {
                let current =
                    Self::scan_stable(&external.source_root, max_rescans).map_err(|_| {
                        anyhow::anyhow!(
                            "external Cargo input verification failed; absolute paths omitted"
                        )
                    })?;
                if current != *expected {
                    bail!("external Cargo package changed while freezing the snapshot");
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
                )
                .map_err(|_| {
                    anyhow::anyhow!(
                        "rewriting external Cargo manifests failed; absolute paths omitted"
                    )
                })?;
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
                snapshot_root: external.snapshot_root.to_string_lossy().replace('\\', "/"),
                manifest,
            })
            .collect::<Vec<_>>();
        let path_relocations = external_inputs
            .iter()
            .map(|external| PathRelocation {
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
        Self::scan_with_metrics(root).map(|(manifest, _)| manifest)
    }

    fn scan_with_metrics(root: &Path) -> Result<(Self, InputScanMetrics)> {
        scan_input_roots_once(&[workspace_indexed_root(root)])
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
    full_scan_on_change: bool,
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
    let mut roots =
        Vec::<(Vec<String>, PathBuf, bool)>::with_capacity(scope.external_path_dependencies.len());
    for dependency in &scope.external_path_dependencies {
        let raw_root = Path::new(&dependency.root);
        let raw_root = if raw_root.is_absolute() {
            raw_root.to_owned()
        } else {
            workspace_root.join(raw_root)
        };
        let metadata =
            fs::symlink_metadata(&raw_root).context("reading external Cargo package root")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("external Cargo package root must be a non-symlink directory");
        }
        let source_root =
            fs::canonicalize(&raw_root).context("resolving external Cargo package root")?;
        if source_root == workspace_root || source_root.starts_with(workspace_root) {
            bail!("external Cargo package root is inside the workspace");
        }
        if workspace_root.starts_with(&source_root) {
            bail!("external Cargo package root contains the workspace");
        }
        let mut package_identity = dependency
            .package_ids
            .iter()
            .map(|id| {
                id.rsplit_once('#')
                    .map(|(_, stable)| stable)
                    .unwrap_or(id)
                    .to_owned()
            })
            .collect::<Vec<_>>();
        package_identity.sort();
        package_identity.dedup();
        if package_identity.is_empty() {
            bail!("external Cargo path package has no stable package identity");
        }
        let build_script = cargo_package_has_build_script(&source_root)?;
        let mut candidate = (package_identity, source_root, build_script);
        while let Some(index) = roots.iter().position(|existing| {
            candidate.1.starts_with(&existing.1) || existing.1.starts_with(&candidate.1)
        }) {
            let existing = roots.remove(index);
            if candidate.1.starts_with(&existing.1) {
                candidate.1 = existing.1;
            }
            candidate.0.extend(existing.0);
            candidate.2 |= existing.2;
        }
        roots.push(candidate);
    }
    let mut roots = roots
        .into_iter()
        .map(|(mut identities, source_root, has_build_script)| {
            identities.sort();
            identities.dedup();
            (identities.join("+"), source_root, has_build_script)
        })
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| left.0.cmp(&right.0));
    if roots.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        bail!("external Cargo path packages have ambiguous stable package identities");
    }
    Ok(roots
        .into_iter()
        .enumerate()
        .map(
            |(index, (_, source_root, full_scan_on_change))| ResolvedExternalRoot {
                source_root,
                snapshot_root: PathBuf::from("external").join(format!("{index:04}")),
                full_scan_on_change,
            },
        )
        .collect())
}

fn cargo_package_has_build_script(root: &Path) -> Result<bool> {
    let manifest_path = root.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .with_context(|| "reading external Cargo package manifest")?;
    let manifest: toml::Value =
        toml::from_str(&manifest).context("parsing external Cargo package manifest")?;
    let build = manifest
        .get("package")
        .and_then(|package| package.get("build"));
    Ok(match build {
        Some(toml::Value::Boolean(enabled)) => *enabled,
        Some(toml::Value::String(path)) => !path.is_empty(),
        _ => root.join("build.rs").is_file(),
    })
}

fn assign_external_snapshot_roots(roots: &mut [ResolvedExternalRoot], workspace: &Inputs) {
    let workspace_paths = workspace
        .sources
        .keys()
        .chain(workspace.assets.keys())
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut attempt = 0_u64;
    loop {
        let base = if attempt == 0 {
            "external".to_owned()
        } else {
            format!("__gpui_external_{attempt:04}__")
        };
        let candidates = (0..roots.len())
            .map(|index| PathBuf::from(&base).join(format!("{index:04}")))
            .collect::<Vec<_>>();
        let collides = candidates.iter().any(|candidate| {
            let candidate = candidate.to_string_lossy().replace('\\', "/");
            workspace_paths.iter().any(|path| {
                *path == candidate
                    || path.starts_with(&format!("{candidate}/"))
                    || candidate.starts_with(&format!("{path}/"))
            })
        });
        if !collides {
            for (root, candidate) in roots.iter_mut().zip(candidates) {
                root.snapshot_root = candidate;
            }
            return;
        }
        attempt = attempt.saturating_add(1);
    }
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
        && expected.external_sources == copied.external_sources
        && expected.untracked_directory_links == copied.untracked_directory_links
        && expected.external_untracked_directory_links == copied.external_untracked_directory_links
        && copied.excluded_sensitive_files.is_empty()
        && copied.external_excluded_sensitive_files.is_empty()
}

fn copy_manifest_files(root: &Path, destination: &Path, manifest: &Inputs) -> Result<()> {
    for relative in manifest
        .sources
        .keys()
        .chain(manifest.assets.keys())
        .chain(manifest.external_sources.keys())
    {
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
    for relative in manifest
        .sources
        .keys()
        .chain(manifest.assets.keys())
        .chain(manifest.external_sources.keys())
    {
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
        Sha256::digest(serde_json::to_vec(&digest_input).context("serializing frozen inputs")?,)
    ))
}

impl CargoInputScope {
    /// Runs full locked Cargo metadata so path dependencies nested below a
    /// workspace member are visible as well as direct path dependencies.
    pub fn discover(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root).context("resolving Cargo workspace root")?;
        let output = Command::new("cargo")
            .current_dir(&root)
            .args(["metadata", "--format-version", "1", "--locked"])
            .output()
            .context("running cargo metadata")?;
        if !output.status.success() {
            bail!("cargo metadata failed with status {}", output.status);
        }
        Self::from_metadata_json(&root, &output.stdout)
    }

    /// Parses Cargo metadata separately from process execution so the input
    /// boundary can be tested against deterministic fixtures.
    pub fn from_metadata_json(root: &Path, json: &[u8]) -> Result<Self> {
        let root = fs::canonicalize(root).context("resolving Cargo workspace root")?;
        let metadata: CargoMetadata =
            serde_json::from_slice(json).context("parsing cargo metadata JSON")?;
        let metadata_root = resolve_metadata_path(&root, &metadata.workspace_root);
        let metadata_root =
            fs::canonicalize(&metadata_root).context("resolving cargo metadata workspace root")?;
        if metadata_root != root {
            bail!("cargo metadata workspace root does not match input root");
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
            let manifest = fs::canonicalize(&raw_manifest)
                .context("resolving local Cargo package manifest")?;
            let package_root = manifest
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Cargo package manifest has no parent"))?;

            if package_root == root || package_root.starts_with(&root) {
                continue;
            }
            if root.starts_with(package_root) {
                bail!("external Cargo path package contains the workspace root");
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
    let metadata = fs::symlink_metadata(manifest).context("reading Cargo package manifest")?;
    if metadata.file_type().is_symlink() {
        bail!("external Cargo path package uses a symlinked manifest");
    }
    let parent = manifest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cargo package manifest has no parent"))?;
    let metadata = fs::symlink_metadata(parent).context("reading Cargo path package directory")?;
    if metadata.file_type().is_symlink() {
        bail!("external Cargo path package uses a symlinked directory");
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

    fn benchmark_process_cpu_ns() -> Option<u64> {
        #[cfg(unix)]
        {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
            // getrusage reports process user+system CPU independently of wall
            // time; unsupported platforms keep the metric explicitly absent.
            let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
            if result != 0 {
                return None;
            }
            let usage = unsafe { usage.assume_init() };
            let micros = (usage.ru_utime.tv_sec as u64)
                .saturating_mul(1_000_000)
                .saturating_add(usage.ru_utime.tv_usec as u64)
                .saturating_add(
                    (usage.ru_stime.tv_sec as u64)
                        .saturating_mul(1_000_000)
                        .saturating_add(usage.ru_stime.tv_usec as u64),
                );
            Some(micros.saturating_mul(1_000))
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn benchmark_cpu_model() -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            return std::process::Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        }
        #[cfg(target_os = "linux")]
        {
            return fs::read_to_string("/proc/cpuinfo").ok().and_then(|info| {
                info.lines()
                    .find_map(|line| line.strip_prefix("model name\t: "))
                    .map(str::to_owned)
            });
        }
        #[cfg(target_os = "windows")]
        {
            return std::env::var("PROCESSOR_IDENTIFIER").ok();
        }
        #[allow(unreachable_code)]
        None
    }

    fn benchmark_percentile(values: &mut [u64], fraction: f64) -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        values.sort_unstable();
        let position = (values.len() - 1) as f64 * fraction;
        let lower = position.floor() as usize;
        let upper = (lower + 1).min(values.len() - 1);
        let weight = position - lower as f64;
        Some(values[lower] as f64 + (values[upper] - values[lower]) as f64 * weight)
    }

    fn benchmark_measure<T>(run: impl FnOnce() -> T) -> (T, u64, Option<u64>) {
        let cpu_before = benchmark_process_cpu_ns();
        let wall = std::time::Instant::now();
        let result = run();
        let wall_ns = wall.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let cpu_ns = cpu_before
            .zip(benchmark_process_cpu_ns())
            .map(|(before, after)| after.saturating_sub(before));
        (result, wall_ns, cpu_ns)
    }

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
    fn indexed_refresh_hashes_dirty_files_and_reuses_unchanged_metadata() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("main.rs"), "source").unwrap();
        fs::write(root.path().join("assets/icon.png"), "image").unwrap();
        fs::write(root.path().join("assets/unchanged.txt"), "same").unwrap();

        let mut index = IndexedInputs::new(root.path());
        let initial = index.refresh(1).unwrap();
        assert_eq!(initial.kind, Some(IndexedRefreshKind::InitialFullScan));
        assert_eq!(initial.hashed_files, 3);
        assert_eq!(initial.hashed_bytes, 30);
        assert_eq!(index.entries().len(), 3);

        let cached = index.refresh(1).unwrap();
        assert_eq!(cached.kind, Some(IndexedRefreshKind::Cached));
        assert_eq!(cached.hashed_files, 0);

        // A direct watcher path is always rehashed, even if size/mtime/file ID
        // happen to look unchanged by the time the debounced event is handled.
        fs::write(root.path().join("main.rs"), "source").unwrap();
        index.mark_dirty("main.rs");
        let refreshed = index.refresh(1).unwrap();
        assert_eq!(refreshed.kind, Some(IndexedRefreshKind::Incremental));
        assert_eq!(refreshed.hashed_files, 1);
        assert_eq!(refreshed.hashed_bytes, 12);
        assert_eq!(refreshed.reused_files, 0);
        assert_eq!(index.manifest(), &Inputs::scan(root.path()).unwrap());

        index.mark_dirty("assets");
        let directory_refresh = index.refresh(1).unwrap();
        assert_eq!(directory_refresh.hashed_files, 2);
        assert_eq!(directory_refresh.hashed_bytes, 18);
        assert_eq!(directory_refresh.reused_files, 0);
        assert_eq!(index.manifest(), &Inputs::scan(root.path()).unwrap());
    }

    #[test]
    fn indexed_refresh_invalidates_both_sides_of_file_and_directory_moves() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("old/nested")).unwrap();
        fs::write(root.path().join("old/nested/a.rs"), "a").unwrap();
        fs::write(root.path().join("old/nested/b.rs"), "b").unwrap();
        let mut index = IndexedInputs::new(root.path());
        index.refresh(1).unwrap();

        fs::rename(root.path().join("old"), root.path().join("new")).unwrap();
        index.mark_renamed("old", "new");
        let refresh = index.refresh(1).unwrap();
        assert_eq!(refresh.kind, Some(IndexedRefreshKind::Incremental));
        assert_eq!(refresh.hashed_files, 2);
        assert_eq!(refresh.removed_files, 2);
        assert_eq!(
            index.entries().keys().cloned().collect::<Vec<_>>(),
            vec!["new/nested/a.rs", "new/nested/b.rs",]
        );

        fs::rename(
            root.path().join("new/nested/a.rs"),
            root.path().join("new/nested/c.rs"),
        )
        .unwrap();
        index.mark_renamed("new/nested/a.rs", "new/nested/c.rs");
        let refresh = index.refresh(1).unwrap();
        assert_eq!(refresh.hashed_files, 1);
        assert_eq!(refresh.removed_files, 1);
        assert!(!index.entries().contains_key("new/nested/a.rs"));
        assert!(index.entries().contains_key("new/nested/c.rs"));
        assert_eq!(index.manifest(), &Inputs::scan(root.path()).unwrap());
    }

    #[test]
    fn indexed_refresh_tracks_external_cargo_roots_without_serializing_host_paths() {
        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::create_dir_all(workspace.path().join("external/0000/src")).unwrap();
        fs::create_dir_all(external.path().join("src")).unwrap();
        fs::write(workspace.path().join("src/lib.rs"), "workspace").unwrap();
        fs::write(
            workspace.path().join("external/0000/src/lib.rs"),
            "workspace path collides textually",
        )
        .unwrap();
        fs::write(
            external.path().join("Cargo.toml"),
            "[package]\nname='dep'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(external.path().join("src/lib.rs"), "external one").unwrap();
        fs::write(external.path().join("private.jks"), "not indexed").unwrap();
        fs::create_dir_all(external.path().join("real-directory")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            external.path().join("real-directory"),
            external.path().join("directory-link"),
        )
        .unwrap();
        let scope = CargoInputScope {
            workspace_root: workspace.path().to_string_lossy().into_owned(),
            external_path_dependencies: vec![ExternalPathDependency {
                root: external.path().to_string_lossy().into_owned(),
                manifest_path: external
                    .path()
                    .join("Cargo.toml")
                    .to_string_lossy()
                    .into_owned(),
                package_ids: vec!["path+external#dep@0.1.0".into()],
            }],
        };
        let mut index = IndexedInputs::new_with_cargo_scope(workspace.path(), &scope).unwrap();
        let initial = index.refresh(1).unwrap();
        assert_eq!(initial.kind, Some(IndexedRefreshKind::InitialFullScan));
        assert!(index.manifest().sources.contains_key("src/lib.rs"));
        assert_eq!(
            index.manifest().sources["external/0000/src/lib.rs"],
            format!("{:x}", Sha256::digest("workspace path collides textually"))
        );
        assert_eq!(
            index.manifest().external_sources["external/0000/src/lib.rs"],
            format!("{:x}", Sha256::digest("external one"))
        );
        assert_eq!(
            index.manifest().external_excluded_sensitive_files,
            vec!["external/0000/private.jks"]
        );
        #[cfg(unix)]
        assert_eq!(
            index.manifest().external_untracked_directory_links,
            vec!["external/0000/directory-link"]
        );
        assert!(
            !index
                .manifest()
                .excluded_sensitive_files
                .iter()
                .any(|path| path.starts_with(EXTERNAL_INPUT_PREFIX))
        );
        let encoded = serde_json::to_string(index.manifest()).unwrap();
        assert!(!encoded.contains(external.path().to_str().unwrap()));
        assert!(!encoded.contains(EXTERNAL_INPUT_PREFIX));

        let external_file = external.path().join("src/lib.rs");
        fs::write(&external_file, "external two").unwrap();
        index.mark_dirty(&external_file);
        let refresh = index.refresh(1).unwrap();
        assert_eq!(refresh.hashed_files, 1);
        assert_ne!(
            index.manifest().external_sources["external/0000/src/lib.rs"],
            format!("{:x}", Sha256::digest("external one"))
        );

        let second_external = tempfile::tempdir().unwrap();
        fs::write(
            second_external.path().join("Cargo.toml"),
            "[package]\nname='dep2'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(second_external.path().join("lib.rs"), "second root").unwrap();
        let changed_scope = CargoInputScope {
            workspace_root: scope.workspace_root.clone(),
            external_path_dependencies: vec![
                scope.external_path_dependencies[0].clone(),
                ExternalPathDependency {
                    root: second_external.path().to_string_lossy().into_owned(),
                    manifest_path: second_external
                        .path()
                        .join("Cargo.toml")
                        .to_string_lossy()
                        .into_owned(),
                    package_ids: vec!["path+external#dep2@0.1.0".into()],
                },
            ],
        };
        assert!(index.refresh_cargo_scope(&changed_scope).unwrap());
        let scope_refresh = index.refresh(1).unwrap();
        assert_eq!(
            scope_refresh.kind,
            Some(IndexedRefreshKind::InitialFullScan)
        );
        assert!(
            index
                .manifest()
                .external_sources
                .contains_key("external/0000/lib.rs")
        );
        assert!(
            !index
                .manifest()
                .external_sources
                .contains_key("external/0001/lib.rs")
        );
    }

    #[test]
    fn external_root_slots_follow_package_identity_not_absolute_path_order() {
        let workspace = tempfile::tempdir().unwrap();
        let alpha_root = tempfile::tempdir().unwrap();
        let zulu_root = tempfile::tempdir().unwrap();
        for (root, name, source) in [
            (alpha_root.path(), "alpha", "alpha source"),
            (zulu_root.path(), "zulu", "zulu source"),
        ] {
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='0.1.0'\n"),
            )
            .unwrap();
            fs::write(root.join("src/lib.rs"), source).unwrap();
        }
        let make_dependency = |root: &Path, name: &str| ExternalPathDependency {
            root: fs::canonicalize(root)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            manifest_path: fs::canonicalize(root.join("Cargo.toml"))
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            package_ids: vec![format!("path+file:///checkout#{name}@0.1.0")],
        };
        let alpha = make_dependency(alpha_root.path(), "alpha");
        let zulu = make_dependency(zulu_root.path(), "zulu");
        let workspace_root = fs::canonicalize(workspace.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let first_scope = CargoInputScope {
            workspace_root: workspace_root.clone(),
            external_path_dependencies: vec![zulu.clone(), alpha.clone()],
        };
        let second_scope = CargoInputScope {
            workspace_root,
            external_path_dependencies: vec![alpha, zulu],
        };

        let mut first =
            IndexedInputs::new_with_cargo_scope(workspace.path(), &first_scope).unwrap();
        let mut second =
            IndexedInputs::new_with_cargo_scope(workspace.path(), &second_scope).unwrap();
        first.refresh(1).unwrap();
        second.refresh(1).unwrap();

        assert_eq!(
            first.manifest().external_sources,
            second.manifest().external_sources
        );
        assert_eq!(
            first.manifest().external_sources["external/0000/src/lib.rs"],
            format!("{:x}", Sha256::digest("alpha source"))
        );
        assert_eq!(
            first.manifest().external_sources["external/0001/src/lib.rs"],
            format!("{:x}", Sha256::digest("zulu source"))
        );
    }

    #[test]
    fn indexed_refresh_falls_back_after_overflow_and_dirty_queue_limit() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("main.rs"), "first").unwrap();
        let mut index = IndexedInputs::new(root.path());
        index.refresh(1).unwrap();

        fs::write(root.path().join("main.rs"), "other").unwrap();
        index.mark_overflow();
        let overflow = index.refresh(1).unwrap();
        assert_eq!(overflow.kind, Some(IndexedRefreshKind::FallbackFullScan));
        assert_eq!(index.manifest(), &Inputs::scan(root.path()).unwrap());

        for path_index in 0..=MAX_INDEXED_DIRTY_PATHS {
            index.mark_dirty(format!("source-{path_index}.rs"));
        }
        let bounded = index.refresh(1).unwrap();
        assert_eq!(bounded.kind, Some(IndexedRefreshKind::FallbackFullScan));
    }

    #[test]
    fn indexed_refresh_ignores_events_for_excluded_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("target/debug")).unwrap();
        fs::write(root.path().join("main.rs"), "source").unwrap();
        let mut index = IndexedInputs::new(root.path());
        index.refresh(1).unwrap();

        index.mark_dirty("target/debug/app");
        let refresh = index.refresh(1).unwrap();
        assert_eq!(refresh.kind, Some(IndexedRefreshKind::Cached));
    }

    #[test]
    fn indexed_refresh_can_disable_metadata_reuse_for_unreliable_filesystems() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("main.rs"), "source").unwrap();
        let mut index = IndexedInputs::new(root.path());
        index.refresh(1).unwrap();
        index.disable_incremental();

        let full = index.refresh(1).unwrap();
        assert_eq!(full.kind, Some(IndexedRefreshKind::FallbackFullScan));
        assert_eq!(full.hashed_files, 1);
        assert!(index.is_index_disabled());

        let next = index.refresh(1).unwrap();
        assert_eq!(next.kind, Some(IndexedRefreshKind::FallbackFullScan));
        assert_eq!(next.hashed_files, 1);
    }

    #[test]
    fn local_temporary_filesystem_is_classified_for_incremental_indexing() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(filesystem_incremental_policy(root.path()), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_network_and_unknown_filesystems_disable_metadata_reuse() {
        assert!(macos_filesystem_policy("nfs").is_some());
        assert!(macos_filesystem_policy("smbfs").is_some());
        assert!(macos_filesystem_policy("unrecognized").is_some());
        assert!(macos_filesystem_policy("exfat").is_some());
        assert!(macos_filesystem_policy("msdos").is_some());
        assert_eq!(macos_filesystem_policy("apfs"), None);
        assert_eq!(macos_filesystem_policy("hfs"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_network_and_unknown_filesystems_disable_metadata_reuse() {
        assert!(linux_filesystem_policy(libc::NFS_SUPER_MAGIC as u64).is_some());
        assert!(linux_filesystem_policy(libc::SMB_SUPER_MAGIC as u64).is_some());
        assert!(linux_filesystem_policy(libc::FUSE_SUPER_MAGIC as u64).is_some());
        assert_eq!(linux_filesystem_policy(libc::EXT4_SUPER_MAGIC as u64), None);
        assert_eq!(linux_filesystem_policy(0x00004d44), None);
        assert_eq!(linux_filesystem_policy(0x2011_bab0), None);
        assert!(linux_filesystem_policy(0x0102_1997).is_some());
        assert!(linux_filesystem_policy(0x7472_6976).is_some());
        assert!(linux_filesystem_policy(0xdeadbeef).is_some());
    }

    #[test]
    fn external_cargo_build_script_forces_root_rescans() {
        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::write(
            external.path().join("Cargo.toml"),
            "[package]\nname='dep'\nversion='0.1.0'\nbuild='build.rs'\n",
        )
        .unwrap();
        fs::write(external.path().join("build.rs"), "fn main() {}\n").unwrap();
        fs::write(external.path().join("build-input.txt"), "one").unwrap();
        let scope = CargoInputScope {
            workspace_root: fs::canonicalize(workspace.path())
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            external_path_dependencies: vec![ExternalPathDependency {
                root: fs::canonicalize(external.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                manifest_path: fs::canonicalize(external.path().join("Cargo.toml"))
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                package_ids: vec!["path+file:///dependency#dep@0.1.0".into()],
            }],
        };
        let mut index = IndexedInputs::new_with_cargo_scope(workspace.path(), &scope).unwrap();
        assert!(!index.is_index_disabled());
        index.refresh(1).unwrap();
        let input = external.path().join("build-input.txt");
        fs::write(&input, "two").unwrap();
        index.mark_dirty(&input);
        let refresh = index.refresh(1).unwrap();
        assert_eq!(refresh.kind, Some(IndexedRefreshKind::Incremental));
        assert_eq!(refresh.hashed_files, 3);
        assert_eq!(
            index.manifest().external_sources["external/0000/build-input.txt"],
            format!("{:x}", Sha256::digest("two"))
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_unknown_and_remote_drive_types_disable_metadata_reuse() {
        use windows_sys::Win32::System::WindowsProgramming::{
            DRIVE_FIXED, DRIVE_REMOTE, DRIVE_UNKNOWN,
        };

        assert_eq!(windows_drive_policy(DRIVE_FIXED), None);
        assert!(windows_drive_policy(DRIVE_REMOTE).is_some());
        assert!(windows_drive_policy(DRIVE_UNKNOWN).is_some());
        assert!(windows_path_is_unc(Path::new(r"\\server\share\project")));
        assert!(windows_path_is_unc(Path::new(
            r"\\?\UNC\server\share\project"
        )));
        assert!(!windows_path_is_unc(Path::new(r"\\?\D:\a\project")));
        assert!(!windows_path_is_unc(Path::new(r"D:\a\project")));
        assert_eq!(
            windows_drive_root(Path::new(r"\\?\D:\a\project")),
            Some("D:\\".encode_utf16().chain([0]).collect())
        );
    }

    #[test]
    fn indexed_refresh_rehashes_replaced_file_identity() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("main.rs"), "old").unwrap();
        let mut index = IndexedInputs::new(root.path());
        index.refresh(1).unwrap();
        let old_identity = index.entries()["main.rs"].identity;

        fs::write(root.path().join("replacement.rs"), "new").unwrap();
        fs::rename(
            root.path().join("replacement.rs"),
            root.path().join("main.rs"),
        )
        .unwrap();
        index.mark_dirty("main.rs");
        let refresh = index.refresh(1).unwrap();
        assert_eq!(refresh.hashed_files, 1);
        assert_ne!(index.entries()["main.rs"].identity, old_identity);
        assert_eq!(index.manifest(), &Inputs::scan(root.path()).unwrap());
    }

    #[test]
    fn indexed_refresh_hashes_direct_dirty_file_even_when_metadata_is_restored() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.rs");
        fs::write(&path, "old").unwrap();
        let mut index = IndexedInputs::new(root.path());
        index.refresh(1).unwrap();
        let previous_mtime = fs::metadata(&path).unwrap().modified().unwrap();

        fs::write(&path, "new").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(previous_mtime))
            .unwrap();
        index.mark_dirty("main.rs");
        let refresh = index.refresh(1).unwrap();

        assert_eq!(refresh.hashed_files, 1);
        assert_eq!(index.manifest(), &Inputs::scan(root.path()).unwrap());
        assert_ne!(
            index.manifest().sources["main.rs"],
            format!("{:x}", Sha256::digest("old"))
        );
    }

    #[test]
    #[ignore = "manual large-tree CPU/I/O benchmark; see T05 backlog instructions"]
    fn large_project_input_index_benchmark() {
        use serde::Serialize;

        #[derive(Serialize)]
        struct Sample {
            phase: &'static str,
            pair: usize,
            order: &'static str,
            indexed_wall_ns: u64,
            indexed_cpu_ns: Option<u64>,
            indexed_hashed_files: usize,
            indexed_hashed_bytes: u64,
            indexed_reused_files: usize,
            oracle_wall_ns: u64,
            oracle_cpu_ns: Option<u64>,
            oracle_passes: usize,
            oracle_hashed_files: usize,
            oracle_hashed_bytes: u64,
            oracle_equal: bool,
        }

        fn setting(name: &str, default: usize) -> usize {
            std::env::var(name)
                .ok()
                .map(|value| value.parse().unwrap_or_else(|_| panic!("invalid {name}")))
                .unwrap_or(default)
        }

        let file_count = setting("GPUI_T05_BENCH_FILES", 4096);
        let file_bytes = setting("GPUI_T05_BENCH_FILE_BYTES", 8192);
        let warmup_pairs = setting("GPUI_T05_BENCH_WARMUP", 10);
        let measured_pairs = setting("GPUI_T05_BENCH_MEASUREMENTS", 30);
        assert!(file_count > 1 && file_bytes > 1 && measured_pairs > 0);

        let root = tempfile::tempdir().unwrap();
        let source_dir = root.path().join("src");
        fs::create_dir_all(&source_dir).unwrap();
        let mut content = vec![b'a'; file_bytes];
        content[0] = b'0';
        for file_index in 0..file_count {
            content[1] = b'a' + (file_index % 26) as u8;
            fs::write(
                source_dir.join(format!("input-{file_index:05}.rs")),
                &content,
            )
            .unwrap();
        }

        let dirty_relative = PathBuf::from("src/input-00000.rs");
        let dirty_absolute = root.path().join(&dirty_relative);
        let original_mtime = fs::metadata(&dirty_absolute).unwrap().modified().unwrap();
        let mut indexed = IndexedInputs::new(root.path());
        let (initial_index, initial_index_wall_ns, initial_index_cpu_ns) =
            benchmark_measure(|| indexed.refresh(2).unwrap());
        let (
            (initial_oracle, initial_oracle_metrics),
            initial_oracle_wall_ns,
            initial_oracle_cpu_ns,
        ) = benchmark_measure(|| {
            scan_input_roots_stable(&[workspace_indexed_root(root.path())], 2).unwrap()
        });
        assert_eq!(indexed.manifest(), &initial_oracle);

        let mut samples = Vec::with_capacity(warmup_pairs + measured_pairs);
        let mut wrong_revision_acceptance = 0_usize;
        for pair in 0..warmup_pairs + measured_pairs {
            let previous_digest = indexed.manifest().digest();
            content[0] = if content[0] == b'0' { b'1' } else { b'0' };
            fs::write(&dirty_absolute, &content).unwrap();
            fs::File::options()
                .write(true)
                .open(&dirty_absolute)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(original_mtime))
                .unwrap();
            indexed.mark_dirty(&dirty_relative);

            let indexed_first = pair % 2 == 0;
            let (
                indexed_refresh,
                indexed_wall_ns,
                indexed_cpu_ns,
                oracle_manifest,
                oracle_metrics,
                oracle_wall_ns,
                oracle_cpu_ns,
            ) = if indexed_first {
                let (refresh, wall, cpu) = benchmark_measure(|| indexed.refresh(2).unwrap());
                let ((manifest, metrics), oracle_wall, oracle_cpu) = benchmark_measure(|| {
                    scan_input_roots_stable(&[workspace_indexed_root(root.path())], 2).unwrap()
                });
                (
                    refresh,
                    wall,
                    cpu,
                    manifest,
                    metrics,
                    oracle_wall,
                    oracle_cpu,
                )
            } else {
                let ((manifest, metrics), oracle_wall, oracle_cpu) = benchmark_measure(|| {
                    scan_input_roots_stable(&[workspace_indexed_root(root.path())], 2).unwrap()
                });
                let (refresh, wall, cpu) = benchmark_measure(|| indexed.refresh(2).unwrap());
                (
                    refresh,
                    wall,
                    cpu,
                    manifest,
                    metrics,
                    oracle_wall,
                    oracle_cpu,
                )
            };
            let oracle_equal = indexed.manifest() == &oracle_manifest;
            if !oracle_equal || indexed.manifest().digest() == previous_digest {
                wrong_revision_acceptance += 1;
            }
            assert!(
                oracle_equal,
                "indexed manifest diverged from the full hash oracle"
            );
            assert_ne!(
                indexed.manifest().digest(),
                previous_digest,
                "same-size, restored-mtime edit did not advance the manifest"
            );

            samples.push(Sample {
                phase: if pair < warmup_pairs {
                    "warmup"
                } else {
                    "measure"
                },
                pair,
                order: if indexed_first {
                    "indexed-first"
                } else {
                    "oracle-first"
                },
                indexed_wall_ns,
                indexed_cpu_ns,
                indexed_hashed_files: indexed_refresh.hashed_files,
                indexed_hashed_bytes: indexed_refresh.hashed_bytes,
                indexed_reused_files: indexed_refresh.reused_files,
                oracle_wall_ns,
                oracle_cpu_ns,
                oracle_passes: oracle_metrics.passes,
                oracle_hashed_files: oracle_metrics.hashed_files,
                oracle_hashed_bytes: oracle_metrics.hashed_bytes,
                oracle_equal,
            });
        }

        let measured = samples
            .iter()
            .filter(|sample| sample.phase == "measure")
            .collect::<Vec<_>>();
        let mut indexed_wall = measured
            .iter()
            .map(|sample| sample.indexed_wall_ns)
            .collect::<Vec<_>>();
        let mut oracle_wall = measured
            .iter()
            .map(|sample| sample.oracle_wall_ns)
            .collect::<Vec<_>>();
        let mut indexed_cpu = measured
            .iter()
            .filter_map(|sample| sample.indexed_cpu_ns)
            .collect::<Vec<_>>();
        let mut oracle_cpu = measured
            .iter()
            .filter_map(|sample| sample.oracle_cpu_ns)
            .collect::<Vec<_>>();
        let report = serde_json::json!({
            "schema_version": 1,
            "benchmark": "T05 indexed watcher scan vs stable full content oracle",
            "environment": {
                "os": std::env::consts::OS,
                "architecture": std::env::consts::ARCH,
                "cpu_model": benchmark_cpu_model(),
                "logical_cpu_count": std::thread::available_parallelism().map(usize::from).ok(),
                "rustc": std::process::Command::new("rustc").arg("-Vv")
                    .output().ok().filter(|result| result.status.success())
                    .map(|result| String::from_utf8_lossy(&result.stdout).trim().to_owned()),
                "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
                "cache_state": "warm filesystem cache; no global page-cache dropping",
                "process_cpu_time": if cfg!(unix) { "getrusage user+system" } else { "unavailable" },
            },
            "dataset": {
                "files": file_count,
                "bytes_per_file": file_bytes,
                "total_input_bytes": (file_count as u64).saturating_mul(file_bytes as u64),
                "changed_files_per_pair": 1,
                "changed_file_size_and_mtime_preserved": true,
            },
            "protocol": {
                "warmup_pairs": warmup_pairs,
                "measurement_pairs": measured_pairs,
                "order": "alternating; file edit and mtime restoration excluded from scan timings",
                "oracle": "stable full content scan (max_rescans=2)",
            },
            "initial_scan": {
                "indexed_wall_ns": initial_index_wall_ns,
                "indexed_cpu_ns": initial_index_cpu_ns,
                "indexed_hashed_files": initial_index.hashed_files,
                "indexed_hashed_bytes": initial_index.hashed_bytes,
                "oracle_wall_ns": initial_oracle_wall_ns,
                "oracle_cpu_ns": initial_oracle_cpu_ns,
                "oracle_passes": initial_oracle_metrics.passes,
                "oracle_hashed_files": initial_oracle_metrics.hashed_files,
                "oracle_hashed_bytes": initial_oracle_metrics.hashed_bytes,
            },
            "correctness": {
                "wrong_revision_acceptance": wrong_revision_acceptance,
                "oracle_manifest_mismatches": samples.iter().filter(|sample| !sample.oracle_equal).count(),
            },
            "summary_ns": {
                "indexed_wall_p50": benchmark_percentile(&mut indexed_wall, 0.50),
                "indexed_wall_p95": benchmark_percentile(&mut indexed_wall, 0.95),
                "oracle_wall_p50": benchmark_percentile(&mut oracle_wall, 0.50),
                "oracle_wall_p95": benchmark_percentile(&mut oracle_wall, 0.95),
                "indexed_process_cpu_p50": benchmark_percentile(&mut indexed_cpu, 0.50),
                "indexed_process_cpu_p95": benchmark_percentile(&mut indexed_cpu, 0.95),
                "oracle_process_cpu_p50": benchmark_percentile(&mut oracle_cpu, 0.50),
                "oracle_process_cpu_p95": benchmark_percentile(&mut oracle_cpu, 0.95),
            },
            "samples": samples,
        });
        let encoded = serde_json::to_vec_pretty(&report).unwrap();
        if let Some(output_path) = std::env::var_os("GPUI_T05_BENCH_OUTPUT") {
            use std::io::Write;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output_path)
                .expect("benchmark output path must not already exist");
            output.write_all(&encoded).unwrap();
            output.write_all(b"\n").unwrap();
        }
        println!(
            "T05_INPUT_INDEX_BENCHMARK={}",
            String::from_utf8(encoded).unwrap()
        );
    }

    #[test]
    fn sensitive_native_config_is_recorded_but_never_hashed_or_frozen() {
        let root = tempfile::tempdir().unwrap();
        let destination_parent = tempfile::tempdir().unwrap();
        let gradle = root.path().join("mobile/android/gradle");
        fs::create_dir_all(gradle.join("app")).unwrap();
        fs::write(gradle.join("local.properties"), "sdk.dir=/private/sdk\n").unwrap();
        fs::write(
            gradle.join("keystore.properties"),
            "storePassword=secret-value\n",
        )
        .unwrap();
        fs::write(gradle.join("app/release.jks"), "private-keystore-bytes").unwrap();

        let first = Inputs::scan(root.path()).unwrap();
        assert!(first.sources.is_empty());
        assert_eq!(
            first.excluded_sensitive_files,
            vec![
                "mobile/android/gradle/app/release.jks",
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
        assert!(
            !destination
                .join("mobile/android/gradle/app/release.jks")
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
                    "path+file:///dependencies/z-package#a-package@0.1.0",
                    &dependencies.path().join("z-package/Cargo.toml"),
                    None,
                ),
                package(
                    "path+file:///dependencies/a-package#z-package@0.1.0",
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
            vec!["path+file:///dependencies/a-package#z-package@0.1.0"]
        );
    }

    #[test]
    fn external_packages_with_duplicate_stable_identities_are_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (root, package_name) in [(first.path(), "first"), (second.path(), "second")] {
            fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname='{package_name}'\nversion='0.1.0'\n"),
            )
            .unwrap();
        }
        let scope = CargoInputScope {
            workspace_root: fs::canonicalize(workspace.path())
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            external_path_dependencies: [first.path(), second.path()]
                .into_iter()
                .map(|root| ExternalPathDependency {
                    root: fs::canonicalize(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    manifest_path: fs::canonicalize(root.join("Cargo.toml"))
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    package_ids: vec![
                        "path+file:///different/location#same-package@0.1.0".to_string(),
                    ],
                })
                .collect(),
        };

        let error =
            indexed_roots_for_cargo_scope(&fs::canonicalize(workspace.path()).unwrap(), &scope)
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("ambiguous stable package identities")
        );
    }

    #[test]
    fn nested_external_packages_share_one_indexed_root() {
        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let nested = external.path().join("member");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            external.path().join("Cargo.toml"),
            "[package]\nname='outer'\nversion='0.1.0'\n[workspace]\nmembers=['member']\n",
        )
        .unwrap();
        fs::write(
            nested.join("Cargo.toml"),
            "[package]\nname='inner'\nversion='0.1.0'\nbuild='build.rs'\n",
        )
        .unwrap();
        fs::write(nested.join("build.rs"), "fn main() {}\n").unwrap();
        fs::write(nested.join("input.txt"), "outer scan sees nested package").unwrap();
        let dependency = |root: &Path, package_id: &str| ExternalPathDependency {
            root: fs::canonicalize(root)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            manifest_path: fs::canonicalize(root.join("Cargo.toml"))
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            package_ids: vec![format!("path+file:///external#{package_id}@0.1.0")],
        };
        let scope = CargoInputScope {
            workspace_root: fs::canonicalize(workspace.path())
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            external_path_dependencies: vec![
                dependency(external.path(), "outer"),
                dependency(&nested, "inner"),
            ],
        };

        let mut index = IndexedInputs::new_with_cargo_scope(workspace.path(), &scope).unwrap();
        assert_eq!(index.watch_roots().len(), 2);
        assert!(!index.is_index_disabled());
        index.refresh(1).unwrap();
        assert!(
            index
                .manifest()
                .external_sources
                .contains_key("external/0000/member/input.txt")
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

    #[test]
    fn frozen_external_slots_are_stable_and_avoid_workspace_path_collisions() {
        let checkout = tempfile::tempdir().unwrap();
        let workspace = checkout.path().join("workspace");
        let dependencies = checkout.path().join("dependencies");
        let first_external = dependencies.join("first");
        let second_external = dependencies.join("second");
        let destination_parent = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.join("app/src")).unwrap();
        fs::create_dir_all(workspace.join("external/0000/src")).unwrap();
        fs::create_dir_all(&first_external).unwrap();
        fs::create_dir_all(&second_external).unwrap();
        fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers=['app']\nresolver='2'\n",
        )
        .unwrap();
        fs::write(
            workspace.join("app/Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\nedition='2024'\n\n[dependencies]\nfirst={path='../../dependencies/first'}\nsecond={path='../../dependencies/second'}\n",
        )
        .unwrap();
        fs::write(workspace.join("app/src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            workspace.join("external/0000/src/lib.rs"),
            "workspace collision file",
        )
        .unwrap();
        for (root, name) in [(&first_external, "first"), (&second_external, "second")] {
            fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='0.1.0'\n"),
            )
            .unwrap();
            fs::write(root.join("src.rs"), name).unwrap();
        }
        let dependency = |root: &Path, name: &str| ExternalPathDependency {
            root: root.to_string_lossy().into_owned(),
            manifest_path: root.join("Cargo.toml").to_string_lossy().into_owned(),
            package_ids: vec![format!("path+file:///dependencies#{name}@0.1.0")],
        };
        let scope = CargoInputScope {
            workspace_root: fs::canonicalize(&workspace)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            external_path_dependencies: vec![
                dependency(&second_external, "second"),
                dependency(&first_external, "first"),
            ],
        };
        let first_snapshot = destination_parent.path().join("snapshot-a");
        let second_snapshot = destination_parent.path().join("snapshot-b");

        let first =
            Inputs::freeze_to_with_cargo_scope(&workspace, &first_snapshot, &scope, 1).unwrap();
        let second =
            Inputs::freeze_to_with_cargo_scope(&workspace, &second_snapshot, &scope, 1).unwrap();

        assert_eq!(first.input_hash, second.input_hash);
        assert_eq!(
            first.path_relocations[0].snapshot_root,
            "__gpui_external_0001__/0000"
        );
        assert!(first_snapshot.join("external/0000/src/lib.rs").is_file());
        assert!(
            first_snapshot
                .join("__gpui_external_0001__/0000/src.rs")
                .is_file()
        );
        assert!(
            first.external_inputs[0]
                .snapshot_root
                .starts_with("__gpui_external_0001__/")
        );
        let frozen_external_roots = serde_json::to_string(&first.external_inputs).unwrap();
        assert!(!frozen_external_roots.contains(checkout.path().to_str().unwrap()));
    }
}
