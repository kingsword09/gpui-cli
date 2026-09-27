//! Build-output paths isolated by platform and BuildKey.

use super::build_key::BuildKey;
use anyhow::{Result, bail};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildPlatform {
    Desktop,
    Android,
    Ios,
}

impl BuildPlatform {
    fn label(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Android => "android",
            Self::Ios => "ios",
        }
    }
}

/// Planned directories for one immutable `(platform, BuildKey)` build.
///
/// The layout is deliberately only a path plan in this slice. The build
/// commands and cache coordinator will consume it later; they must not fall
/// back to a shared project `target`, JNI, or DerivedData directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BuildOutputLayout {
    pub platform: BuildPlatform,
    pub key_hash: String,
    pub root: PathBuf,
    pub cargo_target_dir: PathBuf,
    pub native_staging_dir: PathBuf,
    pub android_jni_dir: Option<PathBuf>,
    pub ios_derived_data_dir: Option<PathBuf>,
}

impl BuildOutputLayout {
    /// Produces deterministic, non-overlapping paths without creating them.
    pub fn for_key(base: &Path, key: &BuildKey, platform: BuildPlatform) -> Result<Self> {
        if !base.is_absolute() {
            bail!("isolated build output base must be an absolute path");
        }
        if !is_sha256_hex(key.key_hash()) {
            bail!("BuildKey hash is not a safe hexadecimal path segment");
        }

        let root = base.join(platform.label()).join(key.key_hash());
        let cargo_target_dir = root.join("cargo-target");
        let native_staging_dir = root.join("native-staging");
        let (android_jni_dir, ios_derived_data_dir) = match platform {
            BuildPlatform::Desktop => (None, None),
            BuildPlatform::Android => {
                let abi = key.material().abi.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("Android output layout requires a BuildKey ABI")
                })?;
                validate_segment("ABI", abi)?;
                (
                    Some(
                        native_staging_dir
                            .join("android")
                            .join("jni-libs")
                            .join(abi),
                    ),
                    None,
                )
            }
            BuildPlatform::Ios => (
                None,
                Some(native_staging_dir.join("ios").join("derived-data")),
            ),
        };

        Ok(Self {
            platform,
            key_hash: key.key_hash().to_string(),
            root,
            cargo_target_dir,
            native_staging_dir,
            android_jni_dir,
            ios_derived_data_dir,
        })
    }

    /// Creates only directories owned by this layout.
    pub fn prepare(&self) -> Result<()> {
        fs::create_dir_all(&self.cargo_target_dir)?;
        fs::create_dir_all(&self.native_staging_dir)?;
        if let Some(path) = &self.android_jni_dir {
            fs::create_dir_all(path)?;
        }
        if let Some(path) = &self.ios_derived_data_dir {
            fs::create_dir_all(path)?;
        }
        Ok(())
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_segment(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value
            .chars()
            .any(|character| character == '/' || character == '\\')
    {
        bail!("{label} is not a safe output path segment");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::build_key::BuildKeyMaterial;
    use serde_json::Value;

    fn key(source: &str, abi: Option<&str>) -> BuildKey {
        BuildKey::new(BuildKeyMaterial {
            source_manifest_hash: source.into(),
            cargo_lock_hash: "lock".into(),
            target_triple: "aarch64-linux-android".into(),
            profile: "dev".into(),
            features: vec![],
            abi: abi.map(str::to_string),
            native_config_hash: "native".into(),
            toolchain_fingerprint: "toolchain".into(),
            relevant_env_hash: "env".into(),
            preview_registry_hash: "registry".into(),
        })
        .unwrap()
    }

    #[test]
    fn same_key_is_stable_and_android_jni_is_abi_specific() {
        let base = tempfile::tempdir().unwrap();
        let build_key = key("source", Some("arm64-v8a"));
        let first =
            BuildOutputLayout::for_key(base.path(), &build_key, BuildPlatform::Android).unwrap();
        let second =
            BuildOutputLayout::for_key(base.path(), &build_key, BuildPlatform::Android).unwrap();

        assert_eq!(first, second);
        assert!(first.root.starts_with(base.path().join("android")));
        assert!(
            first
                .android_jni_dir
                .as_ref()
                .unwrap()
                .ends_with(Path::new("android").join("jni-libs").join("arm64-v8a"))
        );
        first.prepare().unwrap();
        assert!(first.cargo_target_dir.is_dir());
        assert!(first.android_jni_dir.as_ref().unwrap().is_dir());
    }

    #[test]
    fn different_keys_and_platforms_do_not_share_output_roots() {
        let base = tempfile::tempdir().unwrap();
        let first = key("source-a", Some("arm64-v8a"));
        let second = key("source-b", Some("arm64-v8a"));
        let first_android =
            BuildOutputLayout::for_key(base.path(), &first, BuildPlatform::Android).unwrap();
        let second_android =
            BuildOutputLayout::for_key(base.path(), &second, BuildPlatform::Android).unwrap();
        let first_ios =
            BuildOutputLayout::for_key(base.path(), &first, BuildPlatform::Ios).unwrap();

        assert_ne!(first_android.root, second_android.root);
        assert_ne!(first_android.root, first_ios.root);
        assert!(first_ios.android_jni_dir.is_none());
        assert!(first_ios.ios_derived_data_dir.is_some());
    }

    #[test]
    fn desktop_has_no_mobile_native_staging_path() {
        let base = tempfile::tempdir().unwrap();
        let layout =
            BuildOutputLayout::for_key(base.path(), &key("source", None), BuildPlatform::Desktop)
                .unwrap();

        assert!(layout.android_jni_dir.is_none());
        assert!(layout.ios_derived_data_dir.is_none());
        assert_eq!(
            layout.root,
            base.path().join("desktop").join(&layout.key_hash)
        );
    }

    #[test]
    fn invalid_layout_inputs_are_rejected() {
        let base = tempfile::tempdir().unwrap();
        let relative = Path::new("relative-output");
        let error = BuildOutputLayout::for_key(
            relative,
            &key("source", Some("arm64-v8a")),
            BuildPlatform::Android,
        )
        .unwrap_err();
        assert!(error.to_string().contains("absolute path"));

        let error =
            BuildOutputLayout::for_key(base.path(), &key("source", None), BuildPlatform::Android)
                .unwrap_err();
        assert!(error.to_string().contains("requires a BuildKey ABI"));

        let error = BuildOutputLayout::for_key(
            base.path(),
            &key("source", Some("../escape")),
            BuildPlatform::Android,
        )
        .unwrap_err();
        assert!(error.to_string().contains("safe output path segment"));
    }

    #[test]
    fn serialized_layout_is_suitable_for_build_evidence() {
        let base = tempfile::tempdir().unwrap();
        let layout = BuildOutputLayout::for_key(
            base.path(),
            &key("source", Some("x86_64")),
            BuildPlatform::Android,
        )
        .unwrap();
        let value: Value = serde_json::to_value(&layout).unwrap();

        assert_eq!(value["platform"], "android");
        assert_eq!(value["key_hash"], layout.key_hash);
        assert!(
            value["android_jni_dir"]
                .as_str()
                .unwrap()
                .ends_with("x86_64")
        );
    }
}
