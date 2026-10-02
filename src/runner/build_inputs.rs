//! Build-key input collection for local build command paths.

use super::build_key::{BuildKey, BuildKeyMaterial, hash_relevant_environment};
use super::output_layout::{BuildOutputLayout, BuildPlatform};
use crate::devserver::inputs::{Inputs, NativeInputs};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const RELEVANT_ENVIRONMENT: &[&str] = &[
    "ANDROID_HOME",
    "ANDROID_NDK_HOME",
    "ANDROID_SDK_ROOT",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_OPT_LEVEL",
    "CARGO_PROFILE_RELEASE_LTO",
    "CARGO_PROFILE_RELEASE_OPT_LEVEL",
    "CC",
    "CXX",
    "CODE_SIGNING_ALLOWED",
    "CODE_SIGN_IDENTITY",
    "CODE_SIGNING_REQUIRED",
    "DEVELOPER_DIR",
    "DEVELOPMENT_TEAM",
    "IPHONEOS_DEPLOYMENT_TARGET",
    "MACOSX_DEPLOYMENT_TARGET",
    "NDK_HOME",
    "PROVISIONING_PROFILE_SPECIFIER",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "SDKROOT",
    "JAVA_HOME",
];

const ANDROID_KEYSTORE_PROPERTIES_RELATIVE: &str = "mobile/android/gradle/keystore.properties";
const ANDROID_SIGNING_EXTERNAL_HASH: &str = "android.custom-signing";

/// A stable Cargo workspace copy whose lifetime is bound to a build command.
/// The temporary parent is intentionally kept alive so Cargo cannot fall back
/// to the mutable source workspace while the command is running.
pub struct FrozenBuildRoot {
    _temp_dir: TempDir,
    pub root: PathBuf,
    pub manifest: Inputs,
    pub input_hash: String,
}

impl FrozenBuildRoot {
    pub fn create(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving build root for freeze: {}", root.display()))?;
        let scope = Inputs::cargo_input_scope(&root)?;
        let temp_dir = tempfile::tempdir().context("creating frozen build root")?;
        let destination = temp_dir.path().join("workspace");
        let frozen = Inputs::freeze_to_with_cargo_scope(&root, &destination, &scope, 2)?;
        let snapshot_path = PathBuf::from(&frozen.snapshot_path);
        Ok(Self {
            _temp_dir: temp_dir,
            root: snapshot_path,
            manifest: frozen.manifest,
            input_hash: frozen.input_hash,
        })
    }

    /// Copies one explicitly approved sensitive signing input into the
    /// short-lived snapshot. The file is never included in the public input
    /// manifest or BuildKey material directly; callers add only a digest.
    fn copy_sensitive_file(&self, source: &Path, relative: &Path) -> Result<PathBuf> {
        copy_sensitive_file_to_root(&self.root, source, relative)
    }
}

fn copy_sensitive_file_to_root(
    destination_root: &Path,
    source: &Path,
    relative: &Path,
) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("checking sensitive signing input {}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "sensitive signing input is not a regular file: {}",
            source.display()
        );
    }
    let destination = destination_root.join(relative);
    if !destination.starts_with(destination_root) {
        bail!("sensitive signing input escapes frozen workspace");
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, &destination).with_context(|| {
        format!(
            "copying sensitive signing input {} into snapshot",
            source.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
    }
    Ok(destination)
}

/// Frozen inputs shared by a strict check invocation.
///
/// The snapshot lifetime is owned by this value so callers can hand its root
/// to several preview workers without allowing any worker to fall back to the
/// mutable source workspace.
pub struct FrozenCheckInputs {
    pub snapshot: FrozenBuildRoot,
    pub cache_hit_disabled_reason: Option<String>,
}

/// Creates one immutable workspace snapshot for a strict check invocation and
/// applies the known local build-script cache safety policy.
pub fn frozen_check_inputs(root: &Path) -> Result<FrozenCheckInputs> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving check root for freeze: {}", root.display()))?;
    let snapshot = FrozenBuildRoot::create(&root)?;
    let cache_hit_disabled_reason = local_build_script_cache_disabled_reason(&snapshot.root)?;
    Ok(FrozenCheckInputs {
        snapshot,
        cache_hit_disabled_reason,
    })
}

pub struct DesktopBuildPlan {
    pub key: BuildKey,
    pub layout: BuildOutputLayout,
    pub snapshot: FrozenBuildRoot,
    pub cache_hit_disabled_reason: Option<String>,
}

pub struct IosBuildPlan {
    pub key: BuildKey,
    pub layout: BuildOutputLayout,
    pub snapshot: FrozenBuildRoot,
    pub cache_hit_disabled_reason: Option<String>,
    pub physical_signing_identity: Option<IosPhysicalSigningIdentity>,
}

pub struct AndroidBuildPlan {
    pub key: BuildKey,
    pub layout: BuildOutputLayout,
    pub snapshot: FrozenBuildRoot,
    pub cache_hit_disabled_reason: Option<String>,
    pub debug_keystore_identity: Option<AndroidDebugKeystoreIdentity>,
    pub signing_identity: Option<AndroidSigningIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AndroidPreviewCachePolicy {
    pub disabled_reason: Option<String>,
    pub debug_keystore_hash: Option<String>,
    pub android_signing_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AndroidDebugKeystoreIdentity {
    path: PathBuf,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AndroidSigningIdentity {
    properties_path: PathBuf,
    keystore_path: PathBuf,
    properties_sha256: String,
    keystore_sha256: String,
    fingerprint: String,
}

impl AndroidSigningIdentity {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn copy_into_snapshot(&self, snapshot: &FrozenBuildRoot, root: &Path) -> Result<()> {
        let properties_relative = self.properties_path.strip_prefix(root).with_context(|| {
            format!(
                "Android signing properties are outside the project root: {}",
                self.properties_path.display()
            )
        })?;
        snapshot.copy_sensitive_file(&self.properties_path, properties_relative)?;
        let keystore_relative = self.keystore_path.strip_prefix(root).with_context(|| {
            format!(
                "Android signing keystore is outside the project root: {}",
                self.keystore_path.display()
            )
        })?;
        snapshot.copy_sensitive_file(&self.keystore_path, keystore_relative)?;
        self.verify_unchanged()?;
        Ok(())
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        let properties = hash_regular_file(&self.properties_path)?;
        let keystore = hash_regular_file(&self.keystore_path)?;
        if properties != self.properties_sha256 || keystore != self.keystore_sha256 {
            bail!("Android signing inputs changed during the build");
        }
        Ok(())
    }
}

/// Copies the supported local Android signing inputs into an existing frozen
/// workspace for a matrix/live preview. Secrets remain outside the public
/// input manifest; only the BuildKey fingerprint is returned.
pub fn prepare_android_signing_snapshot(
    source_root: &Path,
    snapshot_root: &Path,
) -> Result<Option<String>> {
    let source_root = fs::canonicalize(source_root).with_context(|| {
        format!(
            "resolving Android signing source root {}",
            source_root.display()
        )
    })?;
    let snapshot_root = fs::canonicalize(snapshot_root).with_context(|| {
        format!(
            "resolving Android signing snapshot root {}",
            snapshot_root.display()
        )
    })?;
    let native = NativeInputs::scan(&source_root)?;
    if !has_custom_android_signing_config(&source_root, &native)?
        || !supported_android_signing_config(&source_root, &native)?
    {
        return Ok(None);
    }
    let Some(identity) = android_custom_signing_identity(&source_root).ok().flatten() else {
        return Ok(None);
    };
    let properties_relative = identity
        .properties_path
        .strip_prefix(&source_root)
        .context("Android signing properties escaped the project root")?;
    let keystore_relative = identity
        .keystore_path
        .strip_prefix(&source_root)
        .context("Android signing keystore escaped the project root")?;
    copy_sensitive_file_to_root(
        &snapshot_root,
        &identity.properties_path,
        properties_relative,
    )?;
    copy_sensitive_file_to_root(&snapshot_root, &identity.keystore_path, keystore_relative)?;
    identity.verify_unchanged()?;
    Ok(Some(identity.fingerprint().to_owned()))
}

/// Returns the fingerprint of the supported local Android signing inputs at
/// `root`, without exposing properties or keystore contents.
pub fn android_custom_signing_fingerprint(root: &Path) -> Result<Option<String>> {
    Ok(android_custom_signing_identity(root)?.map(|identity| identity.fingerprint().to_owned()))
}

fn hash_regular_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("checking signing input {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("signing input is not a regular file: {}", path.display());
    }
    let bytes =
        fs::read(path).with_context(|| format!("reading signing input {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[derive(Debug)]
struct AndroidCacheSigningPolicy {
    disabled_reason: Option<String>,
    debug_keystore_identity: Option<AndroidDebugKeystoreIdentity>,
    signing_identity: Option<AndroidSigningIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AndroidDebugSigningMode {
    Default,
    Custom,
    Unknown,
}

fn android_custom_signing_identity(root: &Path) -> Result<Option<AndroidSigningIdentity>> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving Android signing project root {}", root.display()))?;
    let properties_path = root.join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE);
    let metadata = match fs::symlink_metadata(&properties_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "checking Android signing properties {}",
                    properties_path.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "Android signing properties are not a regular file: {}",
            properties_path.display()
        );
    }

    let contents = fs::read_to_string(&properties_path).with_context(|| {
        format!(
            "reading Android signing properties {}",
            properties_path.display()
        )
    })?;
    let store_file = parse_android_store_file(&contents)?;
    let keystore_path = resolve_android_keystore_path(&root, &properties_path, &store_file)?;
    let properties_sha256 = hash_regular_file(&properties_path)?;
    let keystore_sha256 = hash_regular_file(&keystore_path)?;
    let properties_relative = normalized_relative_path(&properties_path, &root)?;
    let keystore_relative = normalized_relative_path(&keystore_path, &root)?;
    let material = format!(
        "properties-path={properties_relative}\nkeystore-path={keystore_relative}\nproperties-sha256={properties_sha256}\nkeystore-sha256={keystore_sha256}"
    );

    Ok(Some(AndroidSigningIdentity {
        properties_path,
        keystore_path,
        properties_sha256,
        keystore_sha256,
        fingerprint: format!("{:x}", Sha256::digest(material.as_bytes())),
    }))
}

fn parse_android_store_file(contents: &str) -> Result<String> {
    let mut store_file = None;
    for (line_number, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let delimiter = line.find(['=', ':']).with_context(|| {
            format!(
                "invalid Android signing properties line {}",
                line_number + 1
            )
        })?;
        let key = line[..delimiter].trim();
        let value = decode_android_property_value(line[delimiter + 1..].trim())?;
        if key == "storeFile" {
            if store_file.is_some() {
                bail!("Android signing properties contain duplicate storeFile entries");
            }
            if value.is_empty() {
                bail!("Android signing properties contain an empty storeFile");
            }
            store_file = Some(value);
        }
    }
    store_file.context("Android signing properties do not define storeFile")
}

fn decode_android_property_value(value: &str) -> Result<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let escaped = chars
            .next()
            .context("Android signing properties contain a trailing escape")?;
        match escaped {
            't' => decoded.push('\t'),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            'f' => decoded.push('\u{000c}'),
            'u' => {
                let mut digits = String::with_capacity(4);
                for _ in 0..4 {
                    digits.push(chars.next().context(
                        "Android signing properties contain an incomplete unicode escape",
                    )?);
                }
                let code = u32::from_str_radix(&digits, 16)
                    .context("Android signing properties contain an invalid unicode escape")?;
                let character = char::from_u32(code)
                    .context("Android signing properties contain an invalid unicode scalar")?;
                decoded.push(character);
            }
            other => decoded.push(other),
        }
    }
    Ok(decoded)
}

fn resolve_android_keystore_path(
    root: &Path,
    properties_path: &Path,
    store_file: &str,
) -> Result<PathBuf> {
    let store_file = Path::new(store_file);
    if store_file.is_absolute() {
        bail!("Android signing storeFile must be relative to the project");
    }

    let properties_root = properties_path
        .parent()
        .context("Android signing properties have no parent directory")?;
    let candidates = [
        properties_root.join("app").join(store_file),
        properties_root.join(store_file),
        root.join(store_file),
    ];
    let mut matches = Vec::new();
    for candidate in candidates {
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("checking Android signing keystore {}", candidate.display())
                });
            }
        };
        if metadata.file_type().is_symlink() {
            bail!(
                "Android signing keystore must not be a symlink: {}",
                candidate.display()
            );
        }
        if !metadata.is_file() {
            bail!(
                "Android signing storeFile is not a regular file: {}",
                candidate.display()
            );
        }
        let canonical = fs::canonicalize(&candidate).with_context(|| {
            format!("resolving Android signing keystore {}", candidate.display())
        })?;
        if !is_android_keystore_path(&canonical) {
            bail!(
                "Android signing storeFile does not use a supported keystore file extension: {}",
                candidate.display()
            );
        }
        if !canonical.starts_with(root) {
            bail!(
                "Android signing keystore is outside the project root: {}",
                candidate.display()
            );
        }
        if !matches.contains(&canonical) {
            matches.push(canonical);
        }
    }

    let [keystore] = matches.as_slice() else {
        if matches.is_empty() {
            bail!("Android signing storeFile does not resolve to a project file");
        }
        bail!("Android signing storeFile resolves to multiple project files");
    };
    Ok(keystore.clone())
}

fn is_android_keystore_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["jks", "keystore", "p12", "pfx"]
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension))
        })
}

fn normalized_relative_path(path: &Path, root: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| {
            format!(
                "path {} is outside Android project root {}",
                path.display(),
                root.display()
            )
        })?
        .to_string_lossy()
        .replace('\\', "/"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IosPhysicalSigningIdentity {
    fingerprint: String,
}

impl IosPhysicalSigningIdentity {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Refuses to reuse or publish a physical-device artifact if the local
    /// signing identities or provisioning profiles changed during the build.
    pub fn verify_unchanged(&self) -> Result<()> {
        let current = ios_physical_signing_fingerprint()?
            .context("iOS physical signing identities or provisioning profiles unavailable")?;
        if current != self.fingerprint {
            bail!("iOS physical signing inputs changed during the build");
        }
        Ok(())
    }
}

impl AndroidDebugKeystoreIdentity {
    /// Refuses to publish or reuse outputs if the key changed during planning/build.
    pub fn verify_unchanged(&self) -> Result<()> {
        let current = android_debug_keystore_hash_at(&self.path)?
            .with_context(|| "Android debug keystore disappeared or became non-regular")?;
        if current != self.sha256 {
            bail!("Android debug keystore changed during the build");
        }
        Ok(())
    }
}

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

/// Freezes the desktop workspace before deriving its BuildKey and output
/// layout. External Cargo path packages therefore participate in the key, and
/// the returned Cargo root cannot be changed by later source edits.
pub fn desktop_build_plan(root: &Path, release: bool) -> Result<DesktopBuildPlan> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving desktop build root: {}", root.display()))?;
    let frozen = frozen_check_inputs(&root)?;
    let snapshot = frozen.snapshot;
    let (host, toolchain_fingerprint) = rustc_identity()?;
    let target_triple = env::var("CARGO_BUILD_TARGET").unwrap_or(host);
    let native = NativeInputs::scan(&snapshot.root)?;
    let key = build_key_from_inputs(
        &snapshot.manifest,
        snapshot.input_hash.clone(),
        &native,
        target_triple,
        release,
        toolchain_fingerprint,
        None,
    )?;
    let layout =
        BuildOutputLayout::for_key(&root.join(".gpui/builds"), &key, BuildPlatform::Desktop)?;
    layout.prepare()?;
    Ok(DesktopBuildPlan {
        key,
        layout,
        snapshot,
        cache_hit_disabled_reason: frozen.cache_hit_disabled_reason,
    })
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

/// Freezes the iOS workspace before deriving its BuildKey and output layout.
/// Cargo and the generated Xcode project can therefore consume one immutable
/// input set for the duration of the non-live build command.
pub fn ios_build_plan(root: &Path, release: bool, rust_target: &str) -> Result<IosBuildPlan> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving iOS build root for freeze: {}", root.display()))?;
    let snapshot = FrozenBuildRoot::create(&root)?;
    let (_, rustc_fingerprint) = rustc_identity()?;
    let mut native = NativeInputs::scan(&snapshot.root)?;
    let build_script_disabled_reason = local_build_script_cache_disabled_reason(&snapshot.root)?;
    let xcode_fingerprint = ios_xcode_sdk_fingerprint(rust_target);
    let (toolchain_fingerprint, cache_hit_disabled_reason) =
        bind_ios_toolchain_identity(&mut native, &rustc_fingerprint, xcode_fingerprint);
    let physical = rust_target == "aarch64-apple-ios";
    let (signing_disabled_reason, physical_signing_identity) =
        bind_ios_physical_signing_identity(&mut native, physical)?;
    let cache_hit_disabled_reason = combine_cache_hit_disabled_reasons([
        cache_hit_disabled_reason,
        build_script_disabled_reason,
        signing_disabled_reason,
    ]);
    let key = build_key_from_inputs(
        &snapshot.manifest,
        snapshot.input_hash.clone(),
        &native,
        rust_target.to_string(),
        release,
        toolchain_fingerprint,
        None,
    )?;
    let layout = BuildOutputLayout::for_key(&root.join(".gpui/builds"), &key, BuildPlatform::Ios)?;
    layout.prepare()?;
    Ok(IosBuildPlan {
        key,
        layout,
        snapshot,
        cache_hit_disabled_reason,
        physical_signing_identity,
    })
}

pub fn ios_build_key(root: &Path, release: bool, rust_target: &str) -> Result<BuildKey> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving iOS build root: {}", root.display()))?;
    let manifest = Inputs::scan_stable(&root, 2)?;
    let mut native = NativeInputs::scan(&root)?;
    let (_, rustc_fingerprint) = rustc_identity()?;
    let xcode_fingerprint = ios_xcode_sdk_fingerprint(rust_target);
    let (toolchain_fingerprint, _) =
        bind_ios_toolchain_identity(&mut native, &rustc_fingerprint, xcode_fingerprint);
    let physical = rust_target == "aarch64-apple-ios";
    let _ = bind_ios_physical_signing_identity(&mut native, physical)?;
    build_key_from_inputs(
        &manifest,
        manifest.digest(),
        &native,
        rust_target.to_string(),
        release,
        toolchain_fingerprint,
        None,
    )
}

fn ios_xcode_sdk_fingerprint(rust_target: &str) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let sdk = match rust_target {
        "aarch64-apple-ios" => "iphoneos",
        "aarch64-apple-ios-sim" => "iphonesimulator",
        _ => return None,
    };
    let xcode_version = command_stdout("xcodebuild", &["-version"])?;
    let sdk_version = command_stdout("xcrun", &["--sdk", sdk, "--show-sdk-version"])?;
    let sdk_build = command_stdout("xcrun", &["--sdk", sdk, "--show-sdk-build-version"])?;
    let identity =
        format!("xcode={xcode_version}\nsdk={sdk}\nversion={sdk_version}\nbuild={sdk_build}");
    Some(format!("{:x}", Sha256::digest(identity.as_bytes())))
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn bind_ios_toolchain_identity(
    native: &mut NativeInputs,
    rustc_fingerprint: &str,
    xcode_fingerprint: Option<String>,
) -> (String, Option<String>) {
    let (xcode_identity, disabled_reason) = match xcode_fingerprint {
        Some(fingerprint) => (fingerprint, None),
        None => (
            "unavailable".into(),
            Some("Xcode/SDK toolchain identity is unavailable; iOS cache reuse is disabled".into()),
        ),
    };
    native
        .external_hashes
        .insert("ios.xcode-sdk-toolchain".into(), xcode_identity.clone());
    let combined = format!("rustc={rustc_fingerprint}\nxcode-sdk={xcode_identity}");
    (
        format!("{:x}", Sha256::digest(combined.as_bytes())),
        disabled_reason,
    )
}

fn bind_ios_physical_signing_identity(
    native: &mut NativeInputs,
    physical: bool,
) -> Result<(Option<String>, Option<IosPhysicalSigningIdentity>)> {
    if !physical {
        return Ok((None, None));
    }
    let fingerprint = ios_physical_signing_fingerprint()?;
    let identity = fingerprint.map(|fingerprint| IosPhysicalSigningIdentity { fingerprint });
    match identity {
        Some(identity) => {
            native
                .external_hashes
                .insert("ios.physical-signing".into(), identity.fingerprint.clone());
            Ok((None, Some(identity)))
        }
        None => {
            native
                .external_hashes
                .insert("ios.physical-signing".into(), "unavailable".into());
            Ok((
                Some(
                    "iOS physical signing identities or provisioning profiles are unavailable; cache reuse is disabled"
                        .into(),
                ),
                None,
            ))
        }
    }
}

fn ios_physical_signing_fingerprint() -> Result<Option<String>> {
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }

    let identities = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .context("reading iOS code-signing identities")?;
    if !identities.status.success() {
        return Ok(None);
    }
    let identity_fingerprints = String::from_utf8_lossy(&identities.stdout)
        .lines()
        .filter_map(|line| {
            let (_, remainder) = line.split_once(')')?;
            let fingerprint = remainder.split_whitespace().next()?;
            (fingerprint.len() == 40 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| fingerprint.to_ascii_uppercase())
        })
        .collect::<Vec<_>>();
    if identity_fingerprints.is_empty() {
        return Ok(None);
    }

    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return Ok(None);
    };
    let mut profile_hashes = Vec::new();
    for profiles_dir in [
        home.join("Library/MobileDevice/Provisioning Profiles"),
        home.join("Library/Developer/Xcode/UserData/Provisioning Profiles"),
    ] {
        let entries = match fs::read_dir(&profiles_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("scanning iOS provisioning profiles"),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension() != Some(OsStr::new("mobileprovision")) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            let bytes = fs::read(&path)
                .with_context(|| format!("reading iOS provisioning profile {}", path.display()))?;
            profile_hashes.push(format!("{:x}", Sha256::digest(bytes)));
        }
    }
    Ok(ios_signing_fingerprint(
        &identity_fingerprints,
        &profile_hashes,
    ))
}

fn ios_signing_fingerprint(identities: &[String], profiles: &[String]) -> Option<String> {
    if identities.is_empty() || profiles.is_empty() {
        return None;
    }
    let mut identities = identities.to_vec();
    identities.sort();
    identities.dedup();
    let mut profiles = profiles.to_vec();
    profiles.sort();
    profiles.dedup();
    let material = format!(
        "identities={}\nprofiles={}",
        identities.join("\n"),
        profiles.join("\n")
    );
    Some(format!("{:x}", Sha256::digest(material.as_bytes())))
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
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving Android build root: {}", root.display()))?;
    let abis = normalize_android_abis(abis)?;
    let target_triple = abis
        .iter()
        .map(|abi| android_rust_target(abi))
        .collect::<Result<Vec<_>>>()?
        .join("+");
    let abi_set = abis.join("+");
    let manifest = Inputs::scan_stable(&root, 2)?;
    let mut native = NativeInputs::scan(&root)?;
    let _ = android_cache_signing_policy(&root, release, &mut native)?;
    let (_, rustc_fingerprint) = rustc_identity()?;
    let (toolchain_fingerprint, _) = bind_android_toolchain_identity(
        &mut native,
        &rustc_fingerprint,
        android_toolchain_fingerprint(),
    );
    build_key_from_inputs(
        &manifest,
        manifest.digest(),
        &native,
        target_triple,
        release,
        toolchain_fingerprint,
        Some(abi_set),
    )
}

/// Returns the cache-safety inputs needed by a live Android preview. The
/// preview receives its BuildKey from the check planner; default-debug carries
/// its keystore identity for reusable output. A statically proven release-only
/// signing block carries both its release fingerprint and the effective
/// default-debug keystore identity; custom-debug signing carries only its
/// custom identity. Ambiguous or non-local signing remains cache-disabled.
pub fn android_preview_cache_policy(
    root: &Path,
    release: bool,
) -> Result<AndroidPreviewCachePolicy> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving Android preview root: {}", root.display()))?;
    let mut native = NativeInputs::scan(&root)?;
    let build_script_disabled_reason = local_build_script_cache_disabled_reason(&root)?;
    let signing_policy = android_cache_signing_policy(&root, release, &mut native)?;
    let (_, rustc_fingerprint) = rustc_identity()?;
    let (_, toolchain_disabled_reason) = bind_android_toolchain_identity(
        &mut native,
        &rustc_fingerprint,
        android_toolchain_fingerprint(),
    );
    let signing_disabled_reason = if release && signing_policy.signing_identity.is_some() {
        Some("Android release signing is not a live preview cache target".into())
    } else {
        signing_policy.disabled_reason
    };
    Ok(AndroidPreviewCachePolicy {
        disabled_reason: combine_cache_hit_disabled_reasons([
            toolchain_disabled_reason,
            signing_disabled_reason,
            build_script_disabled_reason,
        ]),
        debug_keystore_hash: signing_policy
            .debug_keystore_identity
            .map(|identity| identity.sha256),
        android_signing_fingerprint: signing_policy
            .signing_identity
            .map(|identity| identity.fingerprint),
    })
}

/// Reads the current default Android debug-keystore content hash without
/// exposing its path or bytes.
pub fn android_debug_keystore_hash() -> Result<Option<String>> {
    Ok(default_android_debug_keystore_identity()?.map(|identity| identity.sha256))
}

/// Freezes the Android workspace before deriving its BuildKey and output
/// layout. Cargo-ndk and Gradle can therefore consume one immutable input set
/// for the duration of the non-live build command.
pub fn android_build_plan(root: &Path, release: bool, abis: &[String]) -> Result<AndroidBuildPlan> {
    let root = fs::canonicalize(root).with_context(|| {
        format!(
            "resolving Android build root for freeze: {}",
            root.display()
        )
    })?;
    let abis = normalize_android_abis(abis)?;
    let target_triple = abis
        .iter()
        .map(|abi| android_rust_target(abi))
        .collect::<Result<Vec<_>>>()?
        .join("+");
    let abi_set = abis.join("+");
    let snapshot = FrozenBuildRoot::create(&root)?;
    let (_, rustc_fingerprint) = rustc_identity()?;
    let mut native = NativeInputs::scan(&snapshot.root)?;
    let build_script_disabled_reason = local_build_script_cache_disabled_reason(&snapshot.root)?;
    native
        .excluded_sensitive_files
        .extend(snapshot.manifest.excluded_sensitive_files.iter().cloned());
    native.excluded_sensitive_files.sort();
    native.excluded_sensitive_files.dedup();
    let signing_policy = android_cache_signing_policy(&root, release, &mut native)?;
    if let Some(identity) = &signing_policy.signing_identity {
        identity.copy_into_snapshot(&snapshot, &root)?;
    }
    let (toolchain_fingerprint, toolchain_disabled_reason) = bind_android_toolchain_identity(
        &mut native,
        &rustc_fingerprint,
        android_toolchain_fingerprint(),
    );
    let cache_hit_disabled_reason = combine_cache_hit_disabled_reasons([
        toolchain_disabled_reason,
        signing_policy.disabled_reason,
        build_script_disabled_reason,
    ]);
    let key = build_key_from_inputs(
        &snapshot.manifest,
        snapshot.input_hash.clone(),
        &native,
        target_triple,
        release,
        toolchain_fingerprint,
        Some(abi_set),
    )?;
    let layout =
        BuildOutputLayout::for_key(&root.join(".gpui/builds"), &key, BuildPlatform::Android)?;
    layout.prepare()?;
    Ok(AndroidBuildPlan {
        key,
        layout,
        snapshot,
        cache_hit_disabled_reason,
        debug_keystore_identity: signing_policy.debug_keystore_identity,
        signing_identity: signing_policy.signing_identity,
    })
}

fn local_build_script_cache_disabled_reason(root: &Path) -> Result<Option<String>> {
    let manifest = Inputs::scan(root)?;
    if manifest
        .sources
        .keys()
        .any(|path| Path::new(path).file_name() == Some(std::ffi::OsStr::new("build.rs")))
    {
        Ok(Some(
            "local build.rs hidden inputs are not modeled; BuildKey cache reuse is disabled".into(),
        ))
    } else {
        Ok(None)
    }
}

fn android_toolchain_fingerprint() -> Option<String> {
    let sdk_root = consistent_environment_directory(&["ANDROID_HOME", "ANDROID_SDK_ROOT"])?;
    let ndk_home = consistent_environment_directory(&["ANDROID_NDK_HOME"])?;
    if env::var_os("NDK_HOME").is_some()
        && consistent_environment_directory(&["NDK_HOME"]).as_ref() != Some(&ndk_home)
    {
        return None;
    }
    let sdk_packages = android_sdk_package_fingerprint(&sdk_root)?;
    let ndk_revision = android_package_revision(&ndk_home.join("source.properties"))?;
    let cargo_ndk = command_version("cargo", &["ndk", "--version"], false)?;
    let java = java_version()?;
    let identity = format!(
        "sdk-packages={sdk_packages}\nndk-revision={ndk_revision}\ncargo-ndk={cargo_ndk}\njava={java}"
    );
    Some(format!("{:x}", Sha256::digest(identity.as_bytes())))
}

fn consistent_environment_directory(names: &[&str]) -> Option<PathBuf> {
    let mut paths = Vec::new();
    for name in names {
        let Some(value) = env::var_os(name) else {
            continue;
        };
        if value.is_empty() {
            return None;
        }
        paths.push(Some(PathBuf::from(value)));
    }
    consistent_directories(paths)
}

fn consistent_directories(paths: impl IntoIterator<Item = Option<PathBuf>>) -> Option<PathBuf> {
    let mut selected: Option<PathBuf> = None;
    for path in paths.into_iter().flatten() {
        let path = fs::canonicalize(path).ok()?;
        if !path.is_dir() || selected.as_ref().is_some_and(|current| current != &path) {
            return None;
        }
        selected = Some(path);
    }
    selected
}

fn android_sdk_package_fingerprint(sdk_root: &Path) -> Option<String> {
    let mut packages = Vec::new();
    for category in ["platforms", "build-tools"] {
        let category_path = sdk_root.join(category);
        let category_metadata = fs::symlink_metadata(&category_path).ok()?;
        if category_metadata.file_type().is_symlink() || !category_metadata.is_dir() {
            return None;
        }
        let mut category_count = 0usize;
        for entry in fs::read_dir(&category_path).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path()).ok()?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return None;
            }
            let revision = android_package_revision(&entry.path().join("source.properties"))?;
            packages.push(format!("{category}/{name}={revision}"));
            category_count += 1;
        }
        if category_count == 0 {
            return None;
        }
    }
    packages.sort();
    Some(packages.join("\n"))
}

fn android_package_revision(source_properties: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(source_properties).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let contents = fs::read_to_string(source_properties).ok()?;
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "Pkg.Revision" && !value.trim().is_empty()).then(|| value.trim().to_string())
    })
}

fn java_version() -> Option<String> {
    match env::var_os("JAVA_HOME") {
        Some(home) if !home.is_empty() => {
            let executable = PathBuf::from(home).join("bin").join(if cfg!(windows) {
                "java.exe"
            } else {
                "java"
            });
            command_version(executable.as_os_str(), &["-version"], true)
        }
        Some(_) => None,
        None => command_version(OsStr::new("java"), &["-version"], true),
    }
}

fn command_version(
    program: impl AsRef<OsStr>,
    args: &[&str],
    include_stderr: bool,
) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let mut bytes = output.stdout;
    if include_stderr {
        bytes.extend_from_slice(&output.stderr);
    }
    let version = String::from_utf8_lossy(&bytes).trim().to_string();
    (!version.is_empty()).then_some(version)
}

fn bind_android_toolchain_identity(
    native: &mut NativeInputs,
    rustc_fingerprint: &str,
    android_fingerprint: Option<String>,
) -> (String, Option<String>) {
    let (android_identity, disabled_reason) = match android_fingerprint {
        Some(fingerprint) => (fingerprint, None),
        None => (
            "unavailable".into(),
            Some("Android SDK/NDK/cargo-ndk/JDK identity is unavailable or ambiguous; Android cache reuse is disabled".into()),
        ),
    };
    native
        .external_hashes
        .insert("android.sdk-ndk-toolchain".into(), android_identity.clone());
    let combined = format!("rustc={rustc_fingerprint}\nandroid={android_identity}");
    (
        format!("{:x}", Sha256::digest(combined.as_bytes())),
        disabled_reason,
    )
}

fn combine_cache_hit_disabled_reasons(
    reasons: impl IntoIterator<Item = Option<String>>,
) -> Option<String> {
    let combined = reasons.into_iter().flatten().collect::<Vec<_>>().join("; ");
    (!combined.is_empty()).then_some(combined)
}

fn android_cache_signing_policy(
    root: &Path,
    release: bool,
    native: &mut NativeInputs,
) -> Result<AndroidCacheSigningPolicy> {
    let default_debug_keystore = if release {
        None
    } else {
        default_android_debug_keystore_identity()?
    };
    android_cache_signing_policy_with_debug_identity(root, release, native, default_debug_keystore)
}

fn android_cache_signing_policy_with_debug_identity(
    root: &Path,
    release: bool,
    native: &mut NativeInputs,
    default_debug_keystore: Option<AndroidDebugKeystoreIdentity>,
) -> Result<AndroidCacheSigningPolicy> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving Android signing project root {}", root.display()))?;
    let has_custom_signing = has_custom_android_signing_config(&root, native)?;

    if has_custom_signing {
        let supported = supported_android_signing_config(&root, native)?;
        let identity = if supported {
            android_custom_signing_identity(&root).ok().flatten()
        } else {
            None
        };
        if let Some(identity) = identity {
            let keystore_relative = normalized_relative_path(&identity.keystore_path, &root)?;
            let has_unsupported_sensitive_input =
                native.excluded_sensitive_files.iter().any(|path| {
                    path != ANDROID_KEYSTORE_PROPERTIES_RELATIVE && path != &keystore_relative
                });
            if has_unsupported_sensitive_input {
                return Ok(AndroidCacheSigningPolicy {
                    disabled_reason: Some(
                        "local sensitive Android configuration disables cache reuse".into(),
                    ),
                    debug_keystore_identity: None,
                    signing_identity: None,
                });
            }
            native.external_hashes.insert(
                ANDROID_SIGNING_EXTERNAL_HASH.into(),
                identity.fingerprint.clone(),
            );

            let (disabled_reason, debug_keystore_identity) = if release {
                (None, None)
            } else {
                match android_debug_signing_mode(&root, native)? {
                    AndroidDebugSigningMode::Custom => (None, None),
                    AndroidDebugSigningMode::Default => match default_debug_keystore {
                        Some(identity) => {
                            native.external_hashes.insert(
                                "android.default-debug-keystore".into(),
                                identity.sha256.clone(),
                            );
                            (None, Some(identity))
                        }
                        None => (
                            Some(
                                "default Android debug keystore is missing or non-regular; cache reuse is disabled"
                                    .into(),
                            ),
                            None,
                        ),
                    },
                    AndroidDebugSigningMode::Unknown => (
                        Some(
                            "Android custom/release signing cache reuse is limited to statically proven build variants"
                                .into(),
                        ),
                        None,
                    ),
                }
            };
            return Ok(AndroidCacheSigningPolicy {
                disabled_reason,
                debug_keystore_identity,
                signing_identity: Some(identity),
            });
        }

        native
            .external_hashes
            .insert(ANDROID_SIGNING_EXTERNAL_HASH.into(), "unavailable".into());
        return Ok(AndroidCacheSigningPolicy {
            disabled_reason: Some(
                if supported {
                    "Android custom signing inputs are unavailable or outside the project root; cache reuse is disabled"
                } else {
                    "Android custom signing configuration is not a supported local keystore.properties layout; cache reuse is disabled"
                }
                .into(),
            ),
            debug_keystore_identity: None,
            signing_identity: None,
        });
    }

    let has_unsupported_sensitive_input = native
        .excluded_sensitive_files
        .iter()
        .any(|path| path != ANDROID_KEYSTORE_PROPERTIES_RELATIVE);
    if has_unsupported_sensitive_input {
        return Ok(AndroidCacheSigningPolicy {
            disabled_reason: Some(
                "local sensitive Android configuration disables cache reuse".into(),
            ),
            debug_keystore_identity: None,
            signing_identity: None,
        });
    }

    if native
        .excluded_sensitive_files
        .iter()
        .any(|path| path == ANDROID_KEYSTORE_PROPERTIES_RELATIVE)
    {
        return Ok(AndroidCacheSigningPolicy {
            disabled_reason: Some(
                "local sensitive Android configuration disables cache reuse".into(),
            ),
            debug_keystore_identity: None,
            signing_identity: None,
        });
    }

    let (disabled_reason, debug_keystore_identity) =
        android_cache_hit_eligibility(native, release, false, default_debug_keystore);
    Ok(AndroidCacheSigningPolicy {
        disabled_reason,
        debug_keystore_identity,
        signing_identity: None,
    })
}

fn android_cache_hit_eligibility(
    native: &mut NativeInputs,
    release: bool,
    has_custom_signing: bool,
    identity: Option<AndroidDebugKeystoreIdentity>,
) -> (Option<String>, Option<AndroidDebugKeystoreIdentity>) {
    if release {
        return (
            Some("release APK cache reuse is disabled until signing inputs are modeled".into()),
            None,
        );
    }
    if !native.excluded_sensitive_files.is_empty() {
        return (
            Some("local sensitive Android configuration disables cache reuse".into()),
            None,
        );
    }
    if has_custom_signing {
        return (
            Some("custom Android signing configuration disables cache reuse".into()),
            None,
        );
    }

    match identity {
        Some(identity) => {
            native.external_hashes.insert(
                "android.default-debug-keystore".into(),
                identity.sha256.clone(),
            );
            (None, Some(identity))
        }
        None => {
            native.external_hashes.insert(
                "android.default-debug-keystore".into(),
                "missing-or-non-regular".into(),
            );
            (
                Some("default Android debug keystore is missing or non-regular".into()),
                None,
            )
        }
    }
}

fn has_custom_android_signing_config(root: &Path, native: &NativeInputs) -> Result<bool> {
    const SIGNING_MARKERS: &[&str] = &[
        "signingConfig",
        "signingConfigs",
        "storeFile",
        "storePassword",
        "keyAlias",
        "keyPassword",
    ];

    for (_, script) in android_signing_marker_scripts(root, native)? {
        if SIGNING_MARKERS.iter().any(|marker| script.contains(marker)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn android_signing_marker_scripts<'a>(
    root: &Path,
    native: &'a NativeInputs,
) -> Result<Vec<(&'a String, String)>> {
    let mut scripts = Vec::new();
    for relative in native.files.keys().filter(|path| {
        path.starts_with("mobile/android/gradle/")
            && (path.ends_with(".gradle") || path.ends_with(".gradle.kts"))
    }) {
        let script = fs::read_to_string(root.join(relative))
            .with_context(|| format!("reading Android Gradle script {}", relative))?;
        scripts.push((relative, script));
    }
    Ok(scripts)
}

fn supported_android_signing_config(root: &Path, native: &NativeInputs) -> Result<bool> {
    let scripts = android_signing_marker_scripts(root, native)?;
    let signing_scripts = scripts
        .iter()
        .filter(|(_, script)| {
            [
                "signingConfig",
                "signingConfigs",
                "storeFile",
                "storePassword",
                "keyAlias",
                "keyPassword",
            ]
            .iter()
            .any(|marker| script.contains(marker))
        })
        .collect::<Vec<_>>();
    let [(relative, script)] = signing_scripts.as_slice() else {
        return Ok(false);
    };
    if relative.as_str() != "mobile/android/gradle/app/build.gradle"
        && relative.as_str() != "mobile/android/gradle/app/build.gradle.kts"
    {
        return Ok(false);
    }
    if ![
        "signingConfig",
        "signingConfigs",
        "storeFile",
        "keystoreProperties",
        "keystore.properties",
        "keystoreProperties.load",
    ]
    .iter()
    .all(|marker| script.contains(marker))
    {
        return Ok(false);
    }

    // The supported subset reads the checked-in app build script directly.
    // Applied scripts, custom providers and other plugin-owned signing paths
    // remain bypassed because their hidden inputs cannot be proven here.
    if script.contains("apply from:")
        || script.contains("apply(from")
        || script.contains("signingConfigProvider")
    {
        return Ok(false);
    }
    Ok(true)
}

fn android_debug_signing_mode(
    root: &Path,
    native: &NativeInputs,
) -> Result<AndroidDebugSigningMode> {
    if !supported_android_signing_config(root, native)? {
        return Ok(AndroidDebugSigningMode::Unknown);
    }
    let scripts = android_signing_marker_scripts(root, native)?;
    let [(relative, script)] = scripts.as_slice() else {
        return Ok(AndroidDebugSigningMode::Unknown);
    };
    if relative.as_str() != "mobile/android/gradle/app/build.gradle"
        && relative.as_str() != "mobile/android/gradle/app/build.gradle.kts"
    {
        return Ok(AndroidDebugSigningMode::Unknown);
    }
    let Some(build_types) = gradle_named_block(script, "buildTypes") else {
        return Ok(AndroidDebugSigningMode::Unknown);
    };
    if let Some(debug) = gradle_named_block(build_types, "debug")
        && gradle_contains_identifier(debug, "signingConfig")
    {
        return Ok(AndroidDebugSigningMode::Custom);
    }
    let release_is_custom = gradle_named_block(build_types, "release")
        .is_some_and(|release| gradle_contains_identifier(release, "signingConfig"));
    if release_is_custom {
        Ok(AndroidDebugSigningMode::Default)
    } else {
        Ok(AndroidDebugSigningMode::Unknown)
    }
}

fn gradle_named_block<'a>(source: &'a str, wanted: &str) -> Option<&'a str> {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == wanted
                && let Some(opening) = gradle_skip_trivia(source, end)
                && bytes.get(opening) == Some(&b'{')
            {
                let closing = gradle_matching_brace(source, opening)?;
                return Some(&source[opening + 1..closing]);
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    None
}

fn gradle_contains_identifier(source: &str, wanted: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == wanted {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_next_code_position(source: &str, cursor: usize) -> Option<usize> {
    let position = gradle_skip_trivia(source, cursor)?;
    let byte = *source.as_bytes().get(position)?;
    if byte == b'"' || byte == b'\'' {
        return gradle_next_code_position(source, gradle_skip_string(source, position)?);
    }
    Some(position)
}

fn gradle_skip_trivia(source: &str, mut cursor: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"//") {
            cursor += 2;
            while bytes.get(cursor).is_some_and(|byte| *byte != b'\n') {
                cursor += 1;
            }
            continue;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"/*") {
            cursor = gradle_skip_block_comment(source, cursor)?;
            continue;
        }
        return Some(cursor);
    }
}

fn gradle_skip_block_comment(source: &str, mut cursor: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth: usize = 0;
    while cursor + 1 < bytes.len() {
        match &bytes[cursor..cursor + 2] {
            b"/*" => {
                depth += 1;
                cursor += 2;
            }
            b"*/" => {
                depth = depth.checked_sub(1)?;
                cursor += 2;
                if depth == 0 {
                    return Some(cursor);
                }
            }
            _ => cursor += 1,
        }
    }
    None
}

fn gradle_skip_string(source: &str, opening: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let quote = *bytes.get(opening)?;
    let triple = quote == b'"' && bytes.get(opening..opening + 3) == Some(b"\"\"\"");
    let width = if triple { 3 } else { 1 };
    let mut cursor = opening + width;
    while cursor < bytes.len() {
        if triple {
            if bytes.get(cursor..cursor + 3) == Some(b"\"\"\"") {
                return Some(cursor + 3);
            }
            cursor += 1;
        } else if bytes[cursor] == b'\\' {
            cursor += 2;
        } else if bytes[cursor] == quote {
            return Some(cursor + 1);
        } else {
            cursor += 1;
        }
    }
    None
}

fn gradle_matching_brace(source: &str, opening: usize) -> Option<usize> {
    if source.as_bytes().get(opening) != Some(&b'{') {
        return None;
    }
    let bytes = source.as_bytes();
    let mut cursor = opening + 1;
    let mut depth: usize = 1;
    while cursor < bytes.len() {
        if bytes.get(cursor..cursor + 2) == Some(b"//") {
            cursor = gradle_skip_trivia(source, cursor)?;
        } else if bytes.get(cursor..cursor + 2) == Some(b"/*") {
            cursor = gradle_skip_block_comment(source, cursor)?;
        } else if bytes[cursor] == b'"' || bytes[cursor] == b'\'' {
            cursor = gradle_skip_string(source, cursor)?;
        } else {
            match bytes[cursor] {
                b'{' => {
                    depth += 1;
                    cursor += 1;
                }
                b'}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(cursor);
                    }
                    cursor += 1;
                }
                _ => cursor += 1,
            }
        }
    }
    None
}

fn is_gradle_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_gradle_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn default_android_debug_keystore_identity() -> Result<Option<AndroidDebugKeystoreIdentity>> {
    let home = if cfg!(windows) {
        env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"))
    } else {
        env::var_os("HOME")
    };
    let Some(home) = home else {
        return Ok(None);
    };
    android_debug_keystore_identity_in(&PathBuf::from(home))
}

fn android_debug_keystore_identity_in(home: &Path) -> Result<Option<AndroidDebugKeystoreIdentity>> {
    let path = home.join(".android/debug.keystore");
    let Some(sha256) = android_debug_keystore_hash_at(&path)? else {
        return Ok(None);
    };
    Ok(Some(AndroidDebugKeystoreIdentity { path, sha256 }))
}

fn android_debug_keystore_hash_at(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Ok(None),
        Ok(_) => {
            let bytes = fs::read(path).context("reading Android debug keystore")?;
            Ok(Some(format!("{:x}", Sha256::digest(bytes))))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("inspecting Android debug keystore"),
    }
}

fn normalize_android_abis(abis: &[String]) -> Result<Vec<String>> {
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
    Ok(abis)
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
    build_key_from_inputs(
        &manifest,
        manifest.digest(),
        &native,
        target_triple,
        release,
        toolchain_fingerprint,
        abi,
    )
}

fn build_key_from_inputs(
    manifest: &Inputs,
    source_manifest_hash: String,
    native: &NativeInputs,
    target_triple: String,
    release: bool,
    toolchain_fingerprint: String,
    abi: Option<String>,
) -> Result<BuildKey> {
    let relevant_environment =
        hash_relevant_environment(RELEVANT_ENVIRONMENT.iter().map(|name| {
            (
                (*name).to_string(),
                env::var(name).unwrap_or_else(|_| "<unset>".into()),
            )
        }))?;
    BuildKey::new(BuildKeyMaterial {
        source_manifest_hash,
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
    fn encoded_rustflags_change_the_build_environment_hash() {
        assert!(RELEVANT_ENVIRONMENT.contains(&"CARGO_ENCODED_RUSTFLAGS"));

        let baseline = hash_relevant_environment(
            RELEVANT_ENVIRONMENT
                .iter()
                .map(|name| ((*name).to_string(), "<unset>".to_string())),
        )
        .unwrap();
        let encoded_flags = hash_relevant_environment(RELEVANT_ENVIRONMENT.iter().map(|name| {
            let value = if *name == "CARGO_ENCODED_RUSTFLAGS" {
                "-C\u{1f}opt-level=2"
            } else {
                "<unset>"
            };
            ((*name).to_string(), value.to_string())
        }))
        .unwrap();

        assert_ne!(baseline, encoded_flags);
    }

    #[test]
    fn local_build_script_disables_cache_reuse_without_blocking_input_scanning() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\nbuild = \"build.rs\"\n",
        )
        .unwrap();
        fs::write(root.path().join("app/build.rs"), "fn main() {}\n").unwrap();
        fs::write(root.path().join("app/src/lib.rs"), "pub fn value() {}\n").unwrap();

        let reason = local_build_script_cache_disabled_reason(root.path())
            .unwrap()
            .unwrap();
        assert!(reason.contains("build.rs hidden inputs"));

        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile"])
            .status()
            .unwrap();
        assert!(status.success());
        let plan = desktop_build_plan(root.path(), false).unwrap();
        assert!(
            plan.cache_hit_disabled_reason
                .as_deref()
                .is_some_and(|value| value.contains("build.rs hidden inputs"))
        );
    }

    #[test]
    fn workspace_without_build_script_keeps_cache_reuse_eligible() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();

        assert!(
            local_build_script_cache_disabled_reason(root.path())
                .unwrap()
                .is_none()
        );
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
    fn ios_xcode_sdk_fingerprint_changes_the_toolchain_key_and_missing_identity_disables_cache() {
        let mut first_native = NativeInputs::default();
        let (first, first_disabled) = bind_ios_toolchain_identity(
            &mut first_native,
            "rustc-hash",
            Some("xcode-sdk-a".into()),
        );
        assert!(first_disabled.is_none());
        assert_eq!(
            first_native
                .external_hashes
                .get("ios.xcode-sdk-toolchain")
                .map(String::as_str),
            Some("xcode-sdk-a")
        );

        let mut second_native = NativeInputs::default();
        let (second, second_disabled) = bind_ios_toolchain_identity(
            &mut second_native,
            "rustc-hash",
            Some("xcode-sdk-b".into()),
        );
        assert!(second_disabled.is_none());
        assert_ne!(first, second);

        let mut unavailable_native = NativeInputs::default();
        let (unavailable, disabled_reason) =
            bind_ios_toolchain_identity(&mut unavailable_native, "rustc-hash", None);
        assert!(
            disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("identity is unavailable"))
        );
        assert_ne!(first, unavailable);
        assert_eq!(
            unavailable_native
                .external_hashes
                .get("ios.xcode-sdk-toolchain")
                .map(String::as_str),
            Some("unavailable")
        );
    }

    #[test]
    fn ios_physical_signing_identity_is_keyed_and_missing_inputs_disable_reuse() {
        let mut simulator_native = NativeInputs::default();
        let (simulator_reason, simulator_identity) =
            bind_ios_physical_signing_identity(&mut simulator_native, false).unwrap();
        assert!(simulator_reason.is_none());
        assert!(simulator_identity.is_none());
        assert!(simulator_native.external_hashes.is_empty());

        let mut unavailable_native = NativeInputs::default();
        let (unavailable_reason, unavailable_identity) =
            bind_ios_physical_signing_identity(&mut unavailable_native, true)
                .unwrap_or_else(|error| panic!("unexpected signing probe error: {error:#}"));
        if cfg!(target_os = "macos") {
            // The test host may have no signing identities or profiles; the
            // helper must then conservatively disable physical reuse.
            if unavailable_identity.is_none() {
                assert!(
                    unavailable_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("signing identities"))
                );
                assert_eq!(
                    unavailable_native
                        .external_hashes
                        .get("ios.physical-signing")
                        .map(String::as_str),
                    Some("unavailable")
                );
            }
        } else {
            assert!(unavailable_identity.is_none());
            assert!(
                unavailable_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("signing identities"))
            );
        }

        let mut first = NativeInputs::default();
        let first_identity = IosPhysicalSigningIdentity {
            fingerprint: "signing-a".into(),
        };
        first.external_hashes.insert(
            "ios.physical-signing".into(),
            first_identity.fingerprint.clone(),
        );
        let mut second = NativeInputs::default();
        second
            .external_hashes
            .insert("ios.physical-signing".into(), "signing-b".into());
        assert_ne!(first.digest(), second.digest());

        assert_eq!(
            ios_signing_fingerprint(
                &["BBBB".into(), "AAAA".into(), "AAAA".into()],
                &["profile-b".into(), "profile-a".into(), "profile-a".into()],
            ),
            ios_signing_fingerprint(
                &["AAAA".into(), "BBBB".into()],
                &["profile-a".into(), "profile-b".into()],
            )
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ios_xcode_sdk_fingerprint_reads_the_active_simulator_toolchain() {
        assert!(ios_xcode_sdk_fingerprint("aarch64-apple-ios-sim").is_some());
        assert!(ios_xcode_sdk_fingerprint("aarch64-apple-ios").is_some());
    }

    #[test]
    fn android_toolchain_fingerprint_changes_the_key_and_missing_identity_disables_cache() {
        let mut first_native = NativeInputs::default();
        let (first, first_disabled) = bind_android_toolchain_identity(
            &mut first_native,
            "rustc-hash",
            Some("android-toolchain-a".into()),
        );
        assert!(first_disabled.is_none());
        assert_eq!(
            first_native
                .external_hashes
                .get("android.sdk-ndk-toolchain")
                .map(String::as_str),
            Some("android-toolchain-a")
        );

        let mut second_native = NativeInputs::default();
        let (second, second_disabled) = bind_android_toolchain_identity(
            &mut second_native,
            "rustc-hash",
            Some("android-toolchain-b".into()),
        );
        assert!(second_disabled.is_none());
        assert_ne!(first, second);

        let mut unavailable_native = NativeInputs::default();
        let (unavailable, disabled_reason) =
            bind_android_toolchain_identity(&mut unavailable_native, "rustc-hash", None);
        assert!(
            disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("identity is unavailable"))
        );
        assert_ne!(first, unavailable);
        assert_eq!(
            unavailable_native
                .external_hashes
                .get("android.sdk-ndk-toolchain")
                .map(String::as_str),
            Some("unavailable")
        );
    }

    #[test]
    fn android_sdk_fingerprint_tracks_installed_platform_and_build_tools_revisions() {
        let sdk = tempfile::tempdir().unwrap();
        let platform = sdk.path().join("platforms/android-34");
        let build_tools = sdk.path().join("build-tools/34.0.0");
        fs::create_dir_all(&platform).unwrap();
        fs::create_dir_all(&build_tools).unwrap();
        fs::write(platform.join("source.properties"), "Pkg.Revision=3\n").unwrap();
        fs::write(
            build_tools.join("source.properties"),
            "Pkg.Revision=34.0.0\n",
        )
        .unwrap();

        let first = android_sdk_package_fingerprint(sdk.path()).unwrap();
        assert!(first.contains("platforms/android-34=3"));
        assert!(first.contains("build-tools/34.0.0=34.0.0"));
        assert!(!first.contains(&sdk.path().display().to_string()));

        fs::write(
            build_tools.join("source.properties"),
            "Pkg.Revision=34.0.1\n",
        )
        .unwrap();
        let second = android_sdk_package_fingerprint(sdk.path()).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn android_sdk_fingerprint_rejects_incomplete_or_symlinked_packages() {
        let sdk = tempfile::tempdir().unwrap();
        let platform = sdk.path().join("platforms/android-34");
        let build_tools = sdk.path().join("build-tools/34.0.0");
        fs::create_dir_all(&platform).unwrap();
        fs::create_dir_all(&build_tools).unwrap();
        fs::write(platform.join("source.properties"), "Pkg.Revision=3\n").unwrap();
        assert!(android_sdk_package_fingerprint(sdk.path()).is_none());

        fs::write(
            build_tools.join("source.properties"),
            "Pkg.Revision=34.0.0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let linked_sdk = tempfile::tempdir().unwrap();
            symlink(
                sdk.path().join("platforms"),
                linked_sdk.path().join("platforms"),
            )
            .unwrap();
            fs::create_dir_all(linked_sdk.path().join("build-tools")).unwrap();
            assert!(android_sdk_package_fingerprint(linked_sdk.path()).is_none());
        }
    }

    #[test]
    fn android_toolchain_directory_selection_rejects_conflicting_environment_roots() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        assert_eq!(
            consistent_directories([Some(first.path().to_owned()), Some(first.path().to_owned()),]),
            Some(fs::canonicalize(first.path()).unwrap())
        );
        assert!(
            consistent_directories([
                Some(first.path().to_owned()),
                Some(second.path().to_owned()),
            ])
            .is_none()
        );
    }

    #[test]
    fn android_toolchain_fingerprint_reads_a_complete_active_environment() {
        let fingerprint = android_toolchain_fingerprint();
        let sdk_root = consistent_environment_directory(&["ANDROID_HOME", "ANDROID_SDK_ROOT"]);
        let ndk_home = consistent_environment_directory(&["ANDROID_NDK_HOME"]);
        let ndk_alias_matches = env::var_os("NDK_HOME").is_none()
            || consistent_environment_directory(&["NDK_HOME"]).as_ref() == ndk_home.as_ref();
        let probes_available = command_version("cargo", &["ndk", "--version"], false).is_some()
            && java_version().is_some();
        let package_metadata_available = sdk_root
            .as_deref()
            .and_then(android_sdk_package_fingerprint)
            .is_some()
            && ndk_home
                .as_deref()
                .and_then(|home| android_package_revision(&home.join("source.properties")))
                .is_some();

        if env::var_os("GPUI_REQUIRE_ANDROID_TOOLCHAIN_FINGERPRINT").is_some() {
            assert!(
                fingerprint.is_some(),
                "Android toolchain fingerprint was required but SDK/NDK/JDK/cargo-ndk identity is unavailable"
            );
        } else if ndk_alias_matches && probes_available && package_metadata_available {
            assert!(fingerprint.is_some());
        }
    }

    #[test]
    fn ios_build_plan_uses_a_frozen_workspace_root_and_hash() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile"])
            .status()
            .unwrap();
        assert!(status.success());

        let plan = ios_build_plan(root.path(), false, "aarch64-apple-ios-sim").unwrap();

        assert_ne!(plan.snapshot.root, fs::canonicalize(root.path()).unwrap());
        assert!(plan.snapshot.root.join("Cargo.toml").is_file());
        assert_eq!(
            plan.key.material().source_manifest_hash,
            plan.snapshot.input_hash
        );
        assert!(
            plan.layout.cargo_target_dir.starts_with(
                fs::canonicalize(root.path())
                    .unwrap()
                    .join(".gpui")
                    .join("builds")
                    .join("ios")
            )
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

    #[test]
    fn android_build_plan_uses_a_frozen_workspace_root_and_hash() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        let android_gradle = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&android_gradle).unwrap();
        fs::write(android_gradle.join("build.gradle.kts"), "plugins {}\n").unwrap();
        fs::write(
            android_gradle.parent().unwrap().join("local.properties"),
            "sdk.dir=/private/sdk\n",
        )
        .unwrap();
        fs::write(
            android_gradle.parent().unwrap().join("keystore.properties"),
            "storePassword=secret-value\n",
        )
        .unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile"])
            .status()
            .unwrap();
        assert!(status.success());

        let plan =
            android_build_plan(root.path(), false, &["x86_64".into(), "arm64-v8a".into()]).unwrap();

        assert_ne!(plan.snapshot.root, fs::canonicalize(root.path()).unwrap());
        assert!(plan.snapshot.root.join("Cargo.toml").is_file());
        assert_eq!(
            plan.key.material().source_manifest_hash,
            plan.snapshot.input_hash
        );
        assert_eq!(plan.key.material().abi.as_deref(), Some("arm64-v8a+x86_64"));
        assert_eq!(
            plan.snapshot.manifest.excluded_sensitive_files,
            vec![
                "mobile/android/gradle/keystore.properties",
                "mobile/android/gradle/local.properties",
            ]
        );
        assert!(
            !plan
                .snapshot
                .root
                .join("mobile/android/gradle/local.properties")
                .exists()
        );
        assert!(
            !plan
                .snapshot
                .root
                .join("mobile/android/gradle/keystore.properties")
                .exists()
        );
        assert!(
            plan.cache_hit_disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("local sensitive Android configuration"))
        );
        assert!(
            !serde_json::to_string(&plan.snapshot.manifest)
                .unwrap()
                .contains("secret-value")
        );
        assert!(
            plan.layout.cargo_target_dir.starts_with(
                fs::canonicalize(root.path())
                    .unwrap()
                    .join(".gpui")
                    .join("builds")
                    .join("android")
            )
        );
    }

    #[test]
    fn android_debug_keystore_is_hashed_and_revalidated() {
        let home = tempfile::tempdir().unwrap();
        let android_dir = home.path().join(".android");
        fs::create_dir_all(&android_dir).unwrap();
        let keystore = android_dir.join("debug.keystore");
        fs::write(&keystore, b"debug signing key one").unwrap();

        let first = android_debug_keystore_identity_in(home.path())
            .unwrap()
            .unwrap();
        let mut native = NativeInputs::default();
        let (disabled_reason, identity) =
            android_cache_hit_eligibility(&mut native, false, false, Some(first.clone()));

        assert!(disabled_reason.is_none());
        assert_eq!(identity, Some(first.clone()));
        assert_eq!(
            native.external_hashes.get("android.default-debug-keystore"),
            Some(&first.sha256)
        );
        let first_native_digest = native.digest();
        assert!(first.verify_unchanged().is_ok());

        fs::write(&keystore, b"debug signing key two").unwrap();
        assert!(first.verify_unchanged().is_err());
        let second = android_debug_keystore_identity_in(home.path())
            .unwrap()
            .unwrap();
        assert_ne!(first.sha256, second.sha256);
        let mut second_native = NativeInputs::default();
        let (second_disabled_reason, _) =
            android_cache_hit_eligibility(&mut second_native, false, false, Some(second));
        assert!(second_disabled_reason.is_none());
        assert_ne!(first_native_digest, second_native.digest());
    }

    #[test]
    fn android_cache_policy_bypasses_release_and_sensitive_signing_inputs() {
        let mut release_native = NativeInputs::default();
        let (release_reason, release_identity) =
            android_cache_hit_eligibility(&mut release_native, true, false, None);
        assert!(
            release_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("release APK"))
        );
        assert!(release_identity.is_none());
        assert!(release_native.external_hashes.is_empty());

        let mut sensitive_native = NativeInputs::default();
        sensitive_native
            .excluded_sensitive_files
            .push("mobile/android/gradle/keystore.properties".into());
        let (sensitive_reason, sensitive_identity) =
            android_cache_hit_eligibility(&mut sensitive_native, false, false, None);
        assert!(
            sensitive_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("sensitive Android configuration"))
        );
        assert!(sensitive_identity.is_none());
        assert!(sensitive_native.external_hashes.is_empty());

        let mut custom_native = NativeInputs::default();
        let (custom_reason, custom_identity) =
            android_cache_hit_eligibility(&mut custom_native, false, true, None);
        assert!(
            custom_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("custom Android signing"))
        );
        assert!(custom_identity.is_none());
        assert!(custom_native.external_hashes.is_empty());

        let mut missing_keystore_native = NativeInputs::default();
        let (missing_reason, missing_identity) =
            android_cache_hit_eligibility(&mut missing_keystore_native, false, false, None);
        assert!(
            missing_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("debug keystore is missing"))
        );
        assert!(missing_identity.is_none());
        assert_eq!(
            missing_keystore_native
                .external_hashes
                .get("android.default-debug-keystore")
                .map(String::as_str),
            Some("missing-or-non-regular")
        );
    }

    #[test]
    fn custom_android_gradle_signing_config_is_detected() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            "android { signingConfigs { create(\"release\") {} } }\n",
        )
        .unwrap();

        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(has_custom_android_signing_config(root.path(), &native).unwrap());
    }

    #[test]
    fn android_debug_variant_signing_is_detected_conservatively() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                val keystoreProperties = java.util.Properties()
                keystoreProperties.load(FileInputStream("keystore.properties"))
                android {
                    signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"]) } }
                    buildTypes {
                        release { signingConfig = signingConfigs.getByName("release") }
                        debug { signingConfig = signingConfigs.getByName("release") }
                    }
                }
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert_eq!(
            android_debug_signing_mode(root.path(), &native).unwrap(),
            AndroidDebugSigningMode::Custom
        );

        fs::write(
            app.join("build.gradle.kts"),
            r#"
                val keystoreProperties = java.util.Properties()
                keystoreProperties.load(FileInputStream("keystore.properties"))
                android {
                    signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"]) } }
                    buildTypes {
                        release { signingConfig = signingConfigs.getByName("release") }
                        debug { isDebuggable = true }
                    }
                }
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert_eq!(
            android_debug_signing_mode(root.path(), &native).unwrap(),
            AndroidDebugSigningMode::Default
        );
    }

    #[test]
    fn android_debug_signing_mode_proves_release_only_signing() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                val keystoreProperties = java.util.Properties()
                keystoreProperties.load(FileInputStream("keystore.properties"))
                android {
                    signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"]) } }
                    buildTypes {
                        release { signingConfig = signingConfigs.getByName("release") }
                        debug { isDebuggable = true }
                    }
                }
            "#,
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=release.jks\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"release-keystore").unwrap();

        let native = NativeInputs::scan(root.path()).unwrap();
        assert_eq!(
            android_debug_signing_mode(root.path(), &native).unwrap(),
            AndroidDebugSigningMode::Default
        );
    }

    #[test]
    fn supported_android_signing_identity_is_hashed_and_revalidated() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                plugins { id("com.android.application") }
                val keystoreProperties = java.util.Properties()
                keystoreProperties.load(FileInputStream("keystore.properties"))
                android {
                    signingConfigs {
                        create("release") {
                            storeFile = file(keystoreProperties["storeFile"] as String)
                        }
                    }
                    buildTypes { release { signingConfig = signingConfigs.getByName("release") } }
                }
            "#,
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=release.jks\nstorePassword=secret-value\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"keystore-one").unwrap();

        let mut native = NativeInputs::scan(root.path()).unwrap();
        let policy = android_cache_signing_policy(root.path(), true, &mut native).unwrap();
        assert!(policy.disabled_reason.is_none());
        assert!(policy.debug_keystore_identity.is_none());
        let identity = policy.signing_identity.unwrap();
        assert_eq!(identity.fingerprint().len(), 64);
        assert_eq!(
            native
                .external_hashes
                .get(ANDROID_SIGNING_EXTERNAL_HASH)
                .map(String::as_str),
            Some(identity.fingerprint())
        );
        assert!(identity.verify_unchanged().is_ok());
        assert!(
            !serde_json::to_string(&native)
                .unwrap()
                .contains("secret-value")
        );
        let first_native_digest = native.digest();

        fs::write(app.join("release.jks"), b"keystore-two").unwrap();
        assert!(identity.verify_unchanged().is_err());
        let mut second_native = NativeInputs::scan(root.path()).unwrap();
        let second_policy =
            android_cache_signing_policy(root.path(), true, &mut second_native).unwrap();
        assert!(second_policy.signing_identity.is_some());
        assert_ne!(first_native_digest, second_native.digest());
    }

    #[test]
    fn android_signing_snapshot_copies_only_supported_inputs() {
        let source = tempfile::tempdir().unwrap();
        let snapshot = tempfile::tempdir().unwrap();
        let app = source.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"]) } }
                keystoreProperties.load(FileInputStream("keystore.properties"))
                signingConfig
            "#,
        )
        .unwrap();
        fs::write(
            source.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=release.jks\nstorePassword=secret-value\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"private-keystore").unwrap();

        let expected = android_custom_signing_fingerprint(source.path())
            .unwrap()
            .unwrap();
        let copied = prepare_android_signing_snapshot(source.path(), snapshot.path())
            .unwrap()
            .unwrap();
        assert_eq!(copied, expected);
        assert!(
            snapshot
                .path()
                .join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE)
                .is_file()
        );
        assert!(
            snapshot
                .path()
                .join("mobile/android/gradle/app/release.jks")
                .is_file()
        );
        assert_eq!(
            android_custom_signing_fingerprint(snapshot.path())
                .unwrap()
                .as_deref(),
            Some(expected.as_str())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(snapshot.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn android_preview_policy_exposes_signing_fingerprint_but_disables_live_cache() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"]) } }
                keystoreProperties.load(FileInputStream("keystore.properties"))
                signingConfig
            "#,
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=release.jks\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"private-keystore").unwrap();

        let policy = android_preview_cache_policy(root.path(), false).unwrap();
        assert!(
            policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("statically proven"))
        );
        assert_eq!(
            policy.android_signing_fingerprint,
            android_custom_signing_fingerprint(root.path()).unwrap()
        );
        assert!(policy.debug_keystore_hash.is_none());
    }

    #[test]
    fn release_only_signing_binds_the_effective_debug_keystore() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                val keystoreProperties = java.util.Properties()
                keystoreProperties.load(FileInputStream("keystore.properties"))
                android {
                    signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"]) } }
                    buildTypes {
                        release { signingConfig = signingConfigs.getByName("release") }
                        debug { isDebuggable = true }
                    }
                }
            "#,
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=release.jks\nstorePassword=secret-value\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"private-release-keystore").unwrap();
        let default_debug = AndroidDebugKeystoreIdentity {
            path: root.path().join(".android/debug.keystore"),
            sha256: "d".repeat(64),
        };

        let mut native = NativeInputs::scan(root.path()).unwrap();
        let policy = android_cache_signing_policy_with_debug_identity(
            root.path(),
            false,
            &mut native,
            Some(default_debug.clone()),
        )
        .unwrap();

        assert!(policy.disabled_reason.is_none());
        assert_eq!(policy.debug_keystore_identity, Some(default_debug.clone()));
        let signing = policy.signing_identity.unwrap();
        assert_eq!(
            native.external_hashes.get(ANDROID_SIGNING_EXTERNAL_HASH),
            Some(&signing.fingerprint)
        );
        assert_eq!(
            native.external_hashes.get("android.default-debug-keystore"),
            Some(&default_debug.sha256)
        );

        let mut without_debug = NativeInputs::scan(root.path()).unwrap();
        let policy = android_cache_signing_policy_with_debug_identity(
            root.path(),
            false,
            &mut without_debug,
            None,
        )
        .unwrap();
        assert!(
            policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("debug keystore"))
        );
        assert!(policy.debug_keystore_identity.is_none());
    }

    #[test]
    fn android_preview_policy_allows_explicit_custom_debug_signing_cache() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                signingConfigs { create("debugCustom") { storeFile = file(keystoreProperties["storeFile"]) } }
                keystoreProperties.load(FileInputStream("keystore.properties"))
                android {
                    buildTypes {
                        debug { signingConfig = signingConfigs.getByName("debugCustom") }
                    }
                }
            "#,
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=debug.jks\n",
        )
        .unwrap();
        fs::write(app.join("debug.jks"), b"private-debug-keystore").unwrap();

        let policy = android_preview_cache_policy(root.path(), false).unwrap();
        assert!(
            !policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("custom/release signing cache reuse"))
        );
        assert!(policy.android_signing_fingerprint.is_some());
        assert!(policy.debug_keystore_hash.is_none());
    }

    #[test]
    fn android_custom_signing_rejects_external_and_symlink_keystores() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            "signingConfigs { create(\"release\") { storeFile = file(keystoreProperties[\"storeFile\"]) } }\nkeystoreProperties.load(FileInputStream(\"keystore.properties\"))\nsigningConfig\n",
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=../outside.jks\n",
        )
        .unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("outside.jks"), b"outside").unwrap();
        fs::remove_file(root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE)).unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            format!(
                "storeFile={}\n",
                outside.path().join("outside.jks").display()
            ),
        )
        .unwrap();

        let mut native = NativeInputs::scan(root.path()).unwrap();
        let policy = android_cache_signing_policy(root.path(), true, &mut native).unwrap();
        assert!(policy.signing_identity.is_none());
        assert!(
            policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("unavailable"))
        );

        #[cfg(unix)]
        {
            let inside = app.join("link.jks");
            std::os::unix::fs::symlink(outside.path().join("outside.jks"), &inside).unwrap();
            fs::write(
                root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
                "storeFile=link.jks\n",
            )
            .unwrap();
            let mut native = NativeInputs::default();
            native.files.insert(
                "mobile/android/gradle/app/build.gradle.kts".into(),
                "script".into(),
            );
            native.excluded_sensitive_files.extend([
                ANDROID_KEYSTORE_PROPERTIES_RELATIVE.into(),
                "mobile/android/gradle/app/link.jks".into(),
            ]);
            let policy = android_cache_signing_policy(root.path(), true, &mut native).unwrap();
            assert!(policy.signing_identity.is_none());
            assert!(policy.disabled_reason.is_some());
        }
    }

    #[test]
    fn complex_android_signing_scripts_remain_cache_disabled() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        let build_logic = root.path().join("mobile/android/gradle/build-logic");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&build_logic).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            "signingConfigs { create(\"release\") { storeFile = file(keystoreProperties[\"storeFile\"]) } }\nkeystoreProperties.load(FileInputStream(\"keystore.properties\"))\nsigningConfig\n",
        )
        .unwrap();
        fs::write(
            build_logic.join("signing.gradle.kts"),
            "signingConfig = remoteSigningConfig()\n",
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=release.jks\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"keystore").unwrap();

        let mut native = NativeInputs::scan(root.path()).unwrap();
        let policy = android_cache_signing_policy(root.path(), true, &mut native).unwrap();
        assert!(policy.signing_identity.is_none());
        assert!(
            policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("not a supported"))
        );
    }

    #[test]
    fn android_build_plan_copies_supported_signing_inputs_without_manifest_secrets() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                plugins { id("com.android.application") }
                val keystoreProperties = java.util.Properties()
                keystoreProperties.load(FileInputStream("keystore.properties"))
                android {
                    signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"] as String) } }
                    buildTypes { release { signingConfig = signingConfigs.getByName("release") } }
                }
            "#,
        )
        .unwrap();
        fs::write(
            root.path().join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE),
            "storeFile=release.jks\nstorePassword=secret-value\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"private-keystore-bytes").unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile"])
            .status()
            .unwrap();
        assert!(status.success());

        let plan = android_build_plan(root.path(), true, &["arm64-v8a".into()]).unwrap();
        assert!(plan.signing_identity.is_some());
        let snapshot_properties = plan
            .snapshot
            .root
            .join(ANDROID_KEYSTORE_PROPERTIES_RELATIVE);
        let snapshot_keystore = plan
            .snapshot
            .root
            .join("mobile/android/gradle/app/release.jks");
        assert!(snapshot_properties.is_file());
        assert!(snapshot_keystore.is_file());
        assert!(
            plan.snapshot
                .manifest
                .excluded_sensitive_files
                .contains(&ANDROID_KEYSTORE_PROPERTIES_RELATIVE.to_string())
        );
        assert!(
            plan.snapshot
                .manifest
                .excluded_sensitive_files
                .contains(&"mobile/android/gradle/app/release.jks".to_string())
        );
        let serialized_manifest = serde_json::to_string(&plan.snapshot.manifest).unwrap();
        assert!(!serialized_manifest.contains("secret-value"));
        assert!(!serialized_manifest.contains("private-keystore-bytes"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&snapshot_properties)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&snapshot_keystore)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn desktop_build_plan_uses_a_frozen_workspace_root_and_hash() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile"])
            .status()
            .unwrap();
        assert!(status.success());

        let plan = desktop_build_plan(root.path(), false).unwrap();

        assert_ne!(plan.snapshot.root, fs::canonicalize(root.path()).unwrap());
        assert!(plan.snapshot.root.join("Cargo.toml").is_file());
        assert_eq!(
            plan.key.material().source_manifest_hash,
            plan.snapshot.input_hash
        );
        assert!(
            plan.layout.cargo_target_dir.starts_with(
                fs::canonicalize(root.path())
                    .unwrap()
                    .join(".gpui")
                    .join("builds")
                    .join("desktop")
            )
        );
    }
}
