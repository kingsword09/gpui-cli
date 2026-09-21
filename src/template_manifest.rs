//! Versioned metadata for generated template files.
//!
//! The manifest is intentionally data-only. It records the bytes that a
//! generator produced and where the matching baseline can be obtained; it
//! does not contain credentials, live state, or user edits.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_RELATIVE_PATH: &str = ".gpui/template-manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateManifest {
    pub schema_version: u32,
    pub generator: GeneratorInfo,
    pub template_version: String,
    pub platforms: Vec<String>,
    pub dependencies: DependencyInfo,
    pub baseline: BaselineInfo,
    pub groups: Vec<ManifestGroup>,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratorInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyInfo {
    pub gpui_pre_version: String,
    pub gpui_kit_revision: String,
    pub gpui_mobile_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineInfo {
    pub template_version: String,
    pub content_id: String,
    pub distribution: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestGroup {
    pub id: String,
    pub atomic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    pub path: String,
    pub group: String,
    pub base_sha256: String,
    pub template_path: String,
}

impl TemplateManifest {
    pub fn path(root: &Path) -> PathBuf {
        root.join(MANIFEST_RELATIVE_PATH)
    }

    pub fn read(root: &Path) -> Result<Option<Self>> {
        let path = Self::path(root);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("reading template manifest at {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid template manifest at {}", path.display()))?;
        manifest.validate()?;
        Ok(Some(manifest))
    }

    pub fn write(&self, root: &Path) -> Result<()> {
        self.validate()?;
        let path = Self::path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut bytes = serde_json::to_vec_pretty(self).context("serializing template manifest")?;
        bytes.push(b'\n');
        fs::write(&path, bytes)
            .with_context(|| format!("writing template manifest at {}", path.display()))?;
        Ok(())
    }

    pub fn file(&self, path: &str) -> Option<&ManifestFile> {
        self.files.iter().find(|entry| entry.path == path)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported template manifest schema {}; expected {}",
                self.schema_version,
                SCHEMA_VERSION
            );
        }
        if self.template_version.trim().is_empty()
            || self.baseline.template_version != self.template_version
            || !is_sha256(&self.baseline.content_id)
            || self.baseline.distribution.trim().is_empty()
        {
            bail!("template manifest has an invalid baseline reference");
        }

        let groups: BTreeSet<&str> = self.groups.iter().map(|group| group.id.as_str()).collect();
        if groups.len() != self.groups.len() || groups.is_empty() {
            bail!("template manifest groups must be unique and non-empty");
        }

        let mut paths = BTreeSet::new();
        for entry in &self.files {
            validate_relative_path(&entry.path)?;
            if !groups.contains(entry.group.as_str()) {
                bail!(
                    "template manifest file '{}' references unknown group '{}'",
                    entry.path,
                    entry.group
                );
            }
            if !is_sha256(&entry.base_sha256) {
                bail!(
                    "template manifest file '{}' has invalid metadata",
                    entry.path
                );
            }
            validate_relative_path(&entry.template_path)?;
            if !paths.insert(entry.path.as_str()) {
                bail!("template manifest contains duplicate file '{}'", entry.path);
            }
        }
        Ok(())
    }
}

fn is_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn validate_relative_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("unsafe template manifest path '{value}'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> TemplateManifest {
        TemplateManifest {
            schema_version: SCHEMA_VERSION,
            generator: GeneratorInfo {
                name: "gpui-cli".into(),
                version: "0.1.0".into(),
            },
            template_version: "test-v1".into(),
            platforms: vec!["macos".into()],
            dependencies: DependencyInfo {
                gpui_pre_version: "0.3.5".into(),
                gpui_kit_revision: "kit".into(),
                gpui_mobile_revision: "mobile".into(),
            },
            baseline: BaselineInfo {
                template_version: "test-v1".into(),
                content_id: format!("sha256:{}", "0".repeat(64)),
                distribution: "embedded-version-package".into(),
            },
            groups: vec![ManifestGroup {
                id: "app-runtime".into(),
                atomic: true,
            }],
            files: vec![ManifestFile {
                path: "crates/app/src/lib.rs".into(),
                group: "app-runtime".into(),
                base_sha256: format!("sha256:{}", "1".repeat(64)),
                template_path: "app/src/lib.rs".into(),
            }],
        }
    }

    #[test]
    fn rejects_unsafe_paths_and_unknown_groups() {
        let mut unsafe_manifest = manifest();
        unsafe_manifest.files[0].path = "../outside".into();
        assert!(unsafe_manifest.validate().is_err());

        let mut unknown_group = manifest();
        unknown_group.files[0].group = "missing".into();
        assert!(unknown_group.validate().is_err());
    }

    #[test]
    fn round_trips_manifest_with_newline() {
        let dir = tempfile::tempdir().unwrap();
        let expected = manifest();
        expected.write(dir.path()).unwrap();
        let bytes = std::fs::read(TemplateManifest::path(dir.path())).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(TemplateManifest::read(dir.path()).unwrap(), Some(expected));
    }
}
