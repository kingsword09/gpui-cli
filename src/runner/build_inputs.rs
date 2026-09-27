//! Build-key input collection for local build command paths.

use super::build_key::{BuildKey, BuildKeyMaterial, hash_relevant_environment};
use super::output_layout::{BuildOutputLayout, BuildPlatform};
use crate::devserver::inputs::{Inputs, NativeInputs};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

const RELEVANT_ENVIRONMENT: &[&str] = &[
    "ANDROID_HOME",
    "ANDROID_NDK_HOME",
    "CARGO_BUILD_TARGET",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_OPT_LEVEL",
    "CARGO_PROFILE_RELEASE_LTO",
    "CARGO_PROFILE_RELEASE_OPT_LEVEL",
    "CC",
    "CXX",
    "MACOSX_DEPLOYMENT_TARGET",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "JAVA_HOME",
];

/// Collects the current project's explicit desktop build dimensions and
/// returns the key-isolated output layout used by `gpui build/run desktop`.
pub fn desktop_output_layout(root: &Path, release: bool) -> Result<BuildOutputLayout> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving desktop build root: {}", root.display()))?;
    let key = desktop_build_key(&root, release)?;
    BuildOutputLayout::for_key(&root.join(".gpui/builds"), &key, BuildPlatform::Desktop)
}

pub fn desktop_build_key(root: &Path, release: bool) -> Result<BuildKey> {
    let (host, toolchain_fingerprint) = rustc_identity()?;
    let target_triple = env::var("CARGO_BUILD_TARGET").unwrap_or(host);
    build_key_for_target(root, target_triple, release, toolchain_fingerprint, None)
}

/// Collects the BuildKey and isolated output layout used by a non-live iOS
/// build/run. The Rust target is explicit because device and simulator builds
/// have different Cargo artifacts and Xcode destinations.
pub fn ios_output_layout(
    root: &Path,
    release: bool,
    rust_target: &str,
) -> Result<BuildOutputLayout> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving iOS build root: {}", root.display()))?;
    let key = ios_build_key(&root, release, rust_target)?;
    BuildOutputLayout::for_key(&root.join(".gpui/builds"), &key, BuildPlatform::Ios)
}

pub fn ios_build_key(root: &Path, release: bool, rust_target: &str) -> Result<BuildKey> {
    let (_, toolchain_fingerprint) = rustc_identity()?;
    build_key_for_target(
        root,
        rust_target.to_string(),
        release,
        toolchain_fingerprint,
        None,
    )
}

/// Collects the BuildKey and isolated output layout used by a non-live
/// Android build. The ABI set is normalized so equivalent input order does not
/// produce different JNI or Gradle output roots.
pub fn android_output_layout(
    root: &Path,
    release: bool,
    abis: &[String],
) -> Result<BuildOutputLayout> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving Android build root: {}", root.display()))?;
    let key = android_build_key(&root, release, abis)?;
    BuildOutputLayout::for_key(&root.join(".gpui/builds"), &key, BuildPlatform::Android)
}

pub fn android_build_key(root: &Path, release: bool, abis: &[String]) -> Result<BuildKey> {
    let mut abis: Vec<_> = abis
        .iter()
        .map(|abi| abi.trim().to_string())
        .filter(|abi| !abi.is_empty())
        .collect();
    abis.sort();
    abis.dedup();
    if abis.is_empty() {
        bail!("Android BuildKey requires at least one ABI");
    }
    let target_triple = abis
        .iter()
        .map(|abi| android_rust_target(abi))
        .collect::<Result<Vec<_>>>()?
        .join("+");
    let abi_set = abis.join("+");
    let (_, toolchain_fingerprint) = rustc_identity()?;
    build_key_for_target(
        root,
        target_triple,
        release,
        toolchain_fingerprint,
        Some(abi_set),
    )
}

fn build_key_for_target(
    root: &Path,
    target_triple: String,
    release: bool,
    toolchain_fingerprint: String,
    abi: Option<String>,
) -> Result<BuildKey> {
    let manifest = Inputs::scan_stable(root, 2)?;
    let native = NativeInputs::scan(root)?;
    let relevant_environment =
        hash_relevant_environment(RELEVANT_ENVIRONMENT.iter().map(|name| {
            (
                (*name).to_string(),
                env::var(name).unwrap_or_else(|_| "<unset>".into()),
            )
        }))?;
    BuildKey::new(BuildKeyMaterial {
        source_manifest_hash: manifest.digest(),
        cargo_lock_hash: manifest
            .sources
            .get("Cargo.lock")
            .cloned()
            .unwrap_or_else(|| "missing".into()),
        target_triple,
        profile: if release { "release" } else { "dev" }.into(),
        features: Vec::new(),
        abi,
        native_config_hash: native.digest(),
        toolchain_fingerprint,
        relevant_env_hash: relevant_environment,
        preview_registry_hash: "none".into(),
    })
}

fn android_rust_target(abi: &str) -> Result<&'static str> {
    match abi {
        "arm64-v8a" => Ok("aarch64-linux-android"),
        "armeabi-v7a" => Ok("armv7-linux-androideabi"),
        "x86" => Ok("i686-linux-android"),
        "x86_64" => Ok("x86_64-linux-android"),
        _ => bail!("Unknown Android ABI '{abi}'"),
    }
}

fn rustc_identity() -> Result<(String, String)> {
    let output = Command::new("rustc")
        .args(["-vV"])
        .output()
        .context("running rustc -vV for BuildKey")?;
    if !output.status.success() {
        bail!("rustc -vV failed with status {}", output.status);
    }
    let fingerprint = format!("{:x}", Sha256::digest(&output.stdout));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let host = stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("rustc -vV did not report a host triple"))?
        .trim()
        .to_string();
    Ok((host, fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_key_uses_native_and_source_manifests_and_is_key_isolated() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".cargo")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.path().join("Cargo.lock"), "# lock\n").unwrap();
        fs::write(root.path().join("gpui.toml"), "[app]\nname = \"probe\"\n").unwrap();
        fs::write(root.path().join(".cargo/config.toml"), "[build]\n").unwrap();

        let debug = desktop_output_layout(root.path(), false).unwrap();
        let release = desktop_output_layout(root.path(), true).unwrap();

        assert_ne!(debug.key_hash, release.key_hash);
        let canonical_root = fs::canonicalize(root.path()).unwrap();
        assert!(
            debug
                .root
                .starts_with(canonical_root.join(".gpui").join("builds").join("desktop"))
        );
        assert!(debug.cargo_target_dir.ends_with("cargo-target"));
    }

    #[test]
    fn missing_lock_is_explicitly_represented_in_the_desktop_key() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.path().join("gpui.toml"), "[app]\nname = \"probe\"\n").unwrap();

        let key = desktop_build_key(root.path(), false).unwrap();
        assert_eq!(key.material().cargo_lock_hash, "missing");
    }

    #[test]
    fn ios_targets_get_distinct_key_isolated_derived_data() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.path().join("Cargo.lock"), "# lock\n").unwrap();
        fs::write(root.path().join("gpui.toml"), "[app]\nname = \"probe\"\n").unwrap();

        let simulator = ios_output_layout(root.path(), false, "aarch64-apple-ios-sim").unwrap();
        let device = ios_output_layout(root.path(), false, "aarch64-apple-ios").unwrap();

        assert_ne!(simulator.key_hash, device.key_hash);
        assert!(
            simulator.root.starts_with(
                fs::canonicalize(root.path())
                    .unwrap()
                    .join(".gpui")
                    .join("builds")
                    .join("ios")
            )
        );
        assert!(simulator.android_jni_dir.is_none());
        assert!(
            simulator
                .ios_derived_data_dir
                .as_ref()
                .is_some_and(|path| path.ends_with("derived-data"))
        );
    }

    #[test]
    fn android_abi_sets_are_order_independent_and_isolate_gradle_output() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.path().join("Cargo.lock"), "# lock\n").unwrap();
        fs::write(root.path().join("gpui.toml"), "[app]\nname = \"probe\"\n").unwrap();

        let first =
            android_output_layout(root.path(), false, &["arm64-v8a".into(), "x86_64".into()])
                .unwrap();
        let second =
            android_output_layout(root.path(), false, &["x86_64".into(), "arm64-v8a".into()])
                .unwrap();

        assert_eq!(first, second);
        assert!(first.android_jni_dir.is_some());
        assert!(
            first
                .android_gradle_build_dir
                .as_ref()
                .is_some_and(|path| path.ends_with("gradle-build"))
        );
        assert_eq!(first.key_hash.len(), 64);
    }
}
