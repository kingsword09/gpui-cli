//! Verified manifests for immutable build outputs.

use super::build_key::BuildKey;
use super::output_layout::{BuildOutputLayout, BuildPlatform};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tempfile::NamedTempFile;

pub const BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const BUILD_ARTIFACT_MANIFEST_FILE: &str = "artifact-manifest.json";

/// A complete, content-addressed description of the files a build exposes for
/// later cache validation. The manifest file itself is stored outside the list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildArtifactManifest {
    pub schema_version: u32,
    pub platform: BuildPlatform,
    pub key_hash: String,
    pub roots: Vec<String>,
    pub files: Vec<BuildArtifactFile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildArtifactFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

impl BuildArtifactManifest {
    /// Captures the declared files or directories relative to one BuildKey
    /// output root. Directory entries are expanded recursively; symlinks and
    /// special files are rejected instead of becoming ambiguous cache inputs.
    pub fn capture(layout: &BuildOutputLayout, entries: &[PathBuf]) -> Result<Self> {
        Self::capture_at(&layout.root, layout.platform, &layout.key_hash, entries)
    }

    pub fn capture_at(
        root: &Path,
        platform: BuildPlatform,
        key_hash: &str,
        entries: &[PathBuf],
    ) -> Result<Self> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving build artifact root: {}", root.display()))?;
        validate_key_hash(key_hash)?;
        if entries.is_empty() {
            bail!("build artifact manifest requires at least one declared output");
        }

        let mut roots = entries
            .iter()
            .map(|entry| {
                let relative = normalize_relative_path(entry)?;
                portable_path(&relative)
            })
            .collect::<Result<Vec<_>>>()?;
        roots.sort();
        if roots.windows(2).any(|pair| pair[0] == pair[1]) {
            bail!("build artifact manifest contains duplicate output roots");
        }

        let mut files = Vec::new();
        for entry in entries {
            let relative = normalize_relative_path(entry)?;
            collect_entry(&root, &relative, &mut files)?;
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        if files.is_empty() {
            bail!("build artifact manifest cannot be empty");
        }
        if files.windows(2).any(|pair| pair[0].path == pair[1].path) {
            bail!("build artifact manifest contains duplicate files");
        }

        let manifest = Self {
            schema_version: BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            platform,
            key_hash: key_hash.to_string(),
            roots,
            files,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Loads and validates the schema without trusting the file contents as a
    /// cache hit. Call verify or read_verified before reusing outputs.
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("reading build artifact manifest: {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing build artifact manifest: {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Verifies every declared file's existence, regular-file type, size,
    /// executable bit and SHA-256. It recaptures every declared file/directory
    /// root, so newly added files are rejected as well as missing or changed
    /// files.
    pub fn verify(&self, root: &Path) -> Result<()> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving build artifact root: {}", root.display()))?;
        self.validate()?;
        let entries = self.roots.iter().map(PathBuf::from).collect::<Vec<_>>();
        let actual = Self::capture_at(&root, self.platform, &self.key_hash, &entries)?;
        if actual != *self {
            bail!("build artifact set does not match its manifest");
        }
        Ok(())
    }

    /// Verifies the schema, BuildKey/platform binding and all files in one
    /// operation. This is the only helper intended to answer a cache-hit
    /// question.
    pub fn read_verified(
        path: &Path,
        root: &Path,
        platform: BuildPlatform,
        key: &BuildKey,
    ) -> Result<Self> {
        let manifest = Self::read(path)?;
        if manifest.platform != platform {
            bail!(
                "build artifact manifest platform mismatch: expected {}, found {}",
                platform.label(),
                manifest.platform.label()
            );
        }
        if manifest.key_hash != key.key_hash() {
            bail!(
                "build artifact manifest BuildKey mismatch: expected {}, found {}",
                key.key_hash(),
                manifest.key_hash
            );
        }
        manifest.verify(root)?;
        Ok(manifest)
    }

    /// Writes a validated manifest through a same-directory temporary file so
    /// readers never observe a partially serialized cache record.
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("build artifact manifest has no parent"))?;
        fs::create_dir_all(parent)?;
        let mut temporary = NamedTempFile::new_in(parent)
            .with_context(|| format!("creating manifest temporary file in {}", parent.display()))?;
        serde_json::to_writer_pretty(temporary.as_file_mut(), self)
            .context("serializing build artifact manifest")?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| {
            anyhow::anyhow!("publishing build artifact manifest: {}", error.error)
        })?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported build artifact manifest schema {}; expected {}",
                self.schema_version,
                BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION
            );
        }
        validate_key_hash(&self.key_hash)?;
        if self.files.is_empty() {
            bail!("build artifact manifest cannot be empty");
        }
        if self.roots.is_empty() {
            bail!("build artifact manifest must declare output roots");
        }
        let mut previous_root = None;
        for root in &self.roots {
            let normalized = normalize_relative_path(Path::new(root))?;
            if portable_path(&normalized)? != *root {
                bail!("build artifact output root is not normalized: {root}");
            }
            if let Some(previous) = previous_root
                && previous >= root.as_str()
            {
                bail!("build artifact output roots must be sorted and unique");
            }
            previous_root = Some(root.as_str());
        }
        let mut previous = None;
        for file in &self.files {
            let normalized = normalize_relative_path(Path::new(&file.path))?;
            if portable_path(&normalized)? != file.path {
                bail!("build artifact path is not normalized: {}", file.path);
            }
            if file.path == BUILD_ARTIFACT_MANIFEST_FILE {
                bail!("build artifact manifest cannot contain itself");
            }
            validate_sha256(&file.sha256, "artifact hash")?;
            if !self
                .roots
                .iter()
                .any(|root| file.path == *root || file.path.starts_with(&format!("{root}/")))
            {
                bail!(
                    "build artifact file is outside declared output roots: {}",
                    file.path
                );
            }
            if let Some(previous) = previous
                && previous >= file.path.as_str()
            {
                bail!("build artifact files must be sorted and unique");
            }
            previous = Some(file.path.as_str());
        }
        Ok(())
    }
}

fn collect_entry(root: &Path, relative: &Path, files: &mut Vec<BuildArtifactFile>) -> Result<()> {
    let (path, metadata) = metadata_beneath(root, relative)?;
    if metadata.is_dir() {
        let mut children = fs::read_dir(&path)
            .with_context(|| format!("reading build artifact directory: {}", relative.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            collect_entry(root, &relative.join(child.file_name()), files)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        bail!(
            "build artifact is not a regular file: {}",
            relative.display()
        );
    }
    files.push(inspect_file(root, relative)?);
    Ok(())
}

fn metadata_beneath(root: &Path, relative: &Path) -> Result<(PathBuf, fs::Metadata)> {
    let relative = normalize_relative_path(relative)?;
    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            bail!("build artifact path is not safe: {}", relative.display());
        };
        path.push(name);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("reading build artifact: {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "build artifact path traverses a symlink: {}",
                relative.display()
            );
        }
        if components.peek().is_some() && !metadata.is_dir() {
            bail!(
                "build artifact parent is not a directory: {}",
                relative.display()
            );
        }
    }
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("reading build artifact: {}", path.display()))?;
    Ok((path, metadata))
}

fn inspect_file(root: &Path, relative: &Path) -> Result<BuildArtifactFile> {
    let relative = normalize_relative_path(relative)?;
    let relative_text = portable_path(&relative)?;
    let (path, metadata) = metadata_beneath(root, &relative)?;
    if !metadata.is_file() {
        bail!("build artifact is not a regular file: {relative_text}");
    }

    let mut file =
        File::open(&path).with_context(|| format!("opening build artifact: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        hasher.update(&buffer[..count]);
    }
    let final_metadata = file.metadata()?;
    if final_metadata.len() != size {
        bail!("build artifact changed while being read: {relative_text}");
    }
    let (_, current) = metadata_beneath(root, &relative)?;
    if !current.is_file() || current.len() != size {
        bail!("build artifact changed while being read: {relative_text}");
    }

    Ok(BuildArtifactFile {
        path: relative_text,
        size,
        sha256: format!("{:x}", hasher.finalize()),
        executable: is_executable(&final_metadata),
    })
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("build artifact path must be a non-empty relative path");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.to_string_lossy().contains('\\') {
                    bail!(
                        "build artifact path contains an ambiguous separator: {}",
                        path.display()
                    );
                }
                normalized.push(value);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!("build artifact path is not safe: {}", path.display())
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("build artifact path must be a non-empty relative path");
    }
    Ok(normalized)
}

fn portable_path(path: &Path) -> Result<String> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("build artifact path must be valid UTF-8")),
            _ => bail!("build artifact path is not normalized: {}", path.display()),
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

fn validate_key_hash(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("build artifact manifest has an invalid BuildKey hash");
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must be a 64-character hexadecimal digest");
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::build_key::BuildKeyMaterial;
    use std::fs;

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

    #[test]
    fn captures_sorted_files_and_verifies_content() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        fs::create_dir_all(root.path().join("nested")).unwrap();
        fs::write(root.path().join("bin/app"), b"binary").unwrap();
        fs::write(root.path().join("nested/config"), b"config").unwrap();

        let manifest = BuildArtifactManifest::capture_at(
            root.path(),
            BuildPlatform::Desktop,
            key().key_hash(),
            &[PathBuf::from("nested"), PathBuf::from("bin/app")],
        )
        .unwrap();

        assert_eq!(
            manifest
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["bin/app", "nested/config"]
        );
        manifest.verify(root.path()).unwrap();

        fs::write(root.path().join("bin/app"), b"binarY").unwrap();
        assert!(manifest.verify(root.path()).is_err());
    }

    #[test]
    fn atomic_round_trip_binds_platform_and_key() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("app"), b"artifact").unwrap();
        let key = key();
        let manifest = BuildArtifactManifest::capture_at(
            root.path(),
            BuildPlatform::Desktop,
            key.key_hash(),
            &[PathBuf::from("app")],
        )
        .unwrap();
        let manifest_path = root.path().join(BUILD_ARTIFACT_MANIFEST_FILE);
        manifest.write_atomic(&manifest_path).unwrap();

        let loaded = BuildArtifactManifest::read_verified(
            &manifest_path,
            root.path(),
            BuildPlatform::Desktop,
            &key,
        )
        .unwrap();
        assert_eq!(loaded, manifest);

        assert!(
            BuildArtifactManifest::read_verified(
                &manifest_path,
                root.path(),
                BuildPlatform::Android,
                &key,
            )
            .is_err()
        );

        let mut different_key_material = key.material().clone();
        different_key_material.profile = "release".into();
        let different_key = BuildKey::new(different_key_material).unwrap();
        assert!(
            BuildArtifactManifest::read_verified(
                &manifest_path,
                root.path(),
                BuildPlatform::Desktop,
                &different_key,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_partial_outputs_unsafe_paths_and_self_reference() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("app"), b"artifact").unwrap();
        let key = key();
        let missing = BuildArtifactManifest {
            schema_version: BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            platform: BuildPlatform::Desktop,
            key_hash: key.key_hash().to_string(),
            roots: vec!["missing".into()],
            files: vec![BuildArtifactFile {
                path: "missing".into(),
                size: 1,
                sha256: "0".repeat(64),
                executable: false,
            }],
        };
        assert!(missing.verify(root.path()).is_err());

        assert!(
            BuildArtifactManifest::capture_at(
                root.path(),
                BuildPlatform::Desktop,
                key.key_hash(),
                &[PathBuf::from("../app")],
            )
            .is_err()
        );

        let self_reference = BuildArtifactManifest {
            schema_version: BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            platform: BuildPlatform::Desktop,
            key_hash: key.key_hash().to_string(),
            roots: vec![BUILD_ARTIFACT_MANIFEST_FILE.into()],
            files: vec![BuildArtifactFile {
                path: BUILD_ARTIFACT_MANIFEST_FILE.into(),
                size: 1,
                sha256: "0".repeat(64),
                executable: false,
            }],
        };
        assert!(self_reference.validate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_parent_directories_during_capture_and_verification() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("app"), b"outside artifact").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();

        assert!(
            BuildArtifactManifest::capture_at(
                root.path(),
                BuildPlatform::Desktop,
                key().key_hash(),
                &[PathBuf::from("linked/app")],
            )
            .is_err()
        );

        let manifest = BuildArtifactManifest {
            schema_version: BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            platform: BuildPlatform::Desktop,
            key_hash: key().key_hash().to_string(),
            roots: vec!["linked/app".into()],
            files: vec![BuildArtifactFile {
                path: "linked/app".into(),
                size: fs::metadata(outside.path().join("app")).unwrap().len(),
                sha256: format!("{:x}", Sha256::digest(b"outside artifact")),
                executable: false,
            }],
        };
        assert!(manifest.verify(root.path()).is_err());
    }

    #[test]
    fn verification_rejects_new_files_inside_a_declared_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("app")).unwrap();
        fs::write(root.path().join("app/binary"), b"binary").unwrap();
        let key = key();
        let manifest = BuildArtifactManifest::capture_at(
            root.path(),
            BuildPlatform::Desktop,
            key.key_hash(),
            &[PathBuf::from("app")],
        )
        .unwrap();
        fs::write(root.path().join("app/extra"), b"unexpected").unwrap();

        assert!(manifest.verify(root.path()).is_err());
    }
}
