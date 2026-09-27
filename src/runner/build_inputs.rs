//! Build-key input collection for the local desktop command path.

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
    let manifest = Inputs::scan_stable(root, 2)?;
    let native = NativeInputs::scan(root)?;
    let (host, toolchain_fingerprint) = rustc_identity()?;
    let target_triple = env::var("CARGO_BUILD_TARGET").unwrap_or(host);
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
        abi: None,
        native_config_hash: native.digest(),
        toolchain_fingerprint,
        relevant_env_hash: relevant_environment,
        preview_registry_hash: "none".into(),
    })
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
}
