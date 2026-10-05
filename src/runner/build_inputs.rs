//! Build-key input collection for local build command paths.

use super::build_key::{BuildKey, BuildKeyMaterial, hash_relevant_environment};
use super::output_layout::{BuildOutputLayout, BuildPlatform};
use crate::devserver::inputs::{Inputs, NativeInputs};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const RELEVANT_ENVIRONMENT: &[&str] = &[
    "ANDROID_HOME",
    "ANDROID_NDK_HOME",
    "ANDROID_SDK_ROOT",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_CODEGEN_UNITS",
    "CARGO_PROFILE_DEV_DEBUG",
    "CARGO_PROFILE_DEV_DEBUG_ASSERTIONS",
    "CARGO_PROFILE_DEV_INCREMENTAL",
    "CARGO_PROFILE_DEV_OPT_LEVEL",
    "CARGO_PROFILE_DEV_OVERFLOW_CHECKS",
    "CARGO_PROFILE_DEV_PANIC",
    "CARGO_PROFILE_DEV_RPATH",
    "CARGO_PROFILE_DEV_SPLIT_DEBUGINFO",
    "CARGO_PROFILE_DEV_STRIP",
    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
    "CARGO_PROFILE_RELEASE_DEBUG",
    "CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS",
    "CARGO_PROFILE_RELEASE_INCREMENTAL",
    "CARGO_PROFILE_RELEASE_LTO",
    "CARGO_PROFILE_RELEASE_OPT_LEVEL",
    "CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS",
    "CARGO_PROFILE_RELEASE_PANIC",
    "CARGO_PROFILE_RELEASE_RPATH",
    "CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO",
    "CARGO_PROFILE_RELEASE_STRIP",
    "RUSTC",
    "RUSTC_BOOTSTRAP",
    "RUSTC_WORKSPACE_WRAPPER",
    "CC",
    "CXX",
    "AR",
    "CFLAGS",
    "CXXFLAGS",
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
    "RUSTUP_TOOLCHAIN",
    "SDKROOT",
    "JAVA_HOME",
];
const FINGERPRINTED_BUILD_TOOLS: &[&str] = &[
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CC",
    "CXX",
    "AR",
];
const MAX_BUILD_TOOL_FINGERPRINT_BYTES: u64 = 64 * 1024 * 1024;
const BUILD_TOOL_CACHE_DISABLED_REASON: &str =
    "configured compiler/linker tool could not be safely fingerprinted; cache reuse is disabled";

const ANDROID_KEYSTORE_PROPERTIES_RELATIVE: &str = "mobile/android/gradle/keystore.properties";
const ANDROID_GRADLE_WRAPPER_PROPERTIES_RELATIVE: &str =
    "mobile/android/gradle/gradle/wrapper/gradle-wrapper.properties";
const ANDROID_GRADLE_VERIFICATION_METADATA_RELATIVE: &str =
    "mobile/android/gradle/gradle/verification-metadata.xml";
const ANDROID_SIGNING_EXTERNAL_HASH: &str = "android.custom-signing";
const ANDROID_SIGNING_MARKERS: &[&str] = &[
    "signingConfig",
    "signingConfigs",
    "storeFile",
    "storePassword",
    "keyAlias",
    "keyPassword",
];
const ANDROID_GRADLE_GLOBAL_CONFIG_CACHE_DISABLED_REASON: &str =
    "Android global Gradle configuration is not modeled; cache reuse is disabled";
const ANDROID_GRADLE_DISTRIBUTION_FINGERPRINT_DOMAIN: &[u8] =
    b"gpui-android-gradle-wrapper-distributions-v1\0";
pub const ANDROID_GRADLE_DISTRIBUTION_CHANGED_REASON: &str = "Android Gradle wrapper distribution changed since the BuildKey was planned; cache reuse is disabled";
const ANDROID_GRADLE_RELATIVE_USER_HOME_CACHE_DISABLED_REASON: &str =
    "relative GRADLE_USER_HOME resolution is context-dependent; cache reuse is disabled";
const ANDROID_GRADLE_LOCAL_BUILD_LOGIC_CACHE_DISABLED_REASON: &str =
    "local Gradle build logic inputs are not modeled; cache reuse is disabled";
const ANDROID_GRADLE_APP_SCRIPT_IO_CACHE_DISABLED_REASON: &str = "Android Gradle app script uses unmodeled file, environment, network, or process I/O; cache reuse is disabled";
const ANDROID_GRADLE_UNKNOWN_PLUGIN_CACHE_DISABLED_REASON: &str =
    "Android Gradle plugin signing behavior is not modeled; cache reuse is disabled";
const ANDROID_GRADLE_CUSTOM_REPOSITORY_CACHE_DISABLED_REASON: &str =
    "Android Gradle custom repository behavior is not modeled; cache reuse is disabled";
const ANDROID_GRADLE_GLOBAL_ENVIRONMENT_NAMES: &[&str] = &[
    "GRADLE_HOME",
    "GRADLE_OPTS",
    "JAVA_OPTS",
    "JAVA_TOOL_OPTIONS",
    "JAVACMD",
    "JDK_JAVA_OPTIONS",
    "_JAVA_OPTIONS",
];
const ANDROID_GRADLE_USER_CONFIG_PATHS: &[&str] = &[
    "gradle.properties",
    "init.gradle",
    "init.gradle.kts",
    "init.d",
];
const MAX_ANDROID_GRADLE_DISTRIBUTION_SCAN_ENTRIES: usize = 4096;

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
    pub gradle_distribution_identity: Option<AndroidGradleDistributionIdentity>,
    pub debug_keystore_identity: Option<AndroidDebugKeystoreIdentity>,
    pub signing_identity: Option<AndroidSigningIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AndroidGradleDistributionIdentity {
    gradle_user_home: PathBuf,
    fingerprint: String,
}

impl AndroidGradleDistributionIdentity {
    pub fn from_parts(gradle_user_home: PathBuf, fingerprint: String) -> Self {
        Self {
            gradle_user_home,
            fingerprint,
        }
    }

    pub fn gradle_user_home(&self) -> &Path {
        &self.gradle_user_home
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn matches_fingerprint(&self) -> bool {
        !android_gradle_user_configuration_is_present(&self.gradle_user_home)
            && android_gradle_distribution_fingerprint_for(&self.gradle_user_home).as_deref()
                == Some(self.fingerprint.as_str())
    }

    pub fn matches_current(&self) -> bool {
        let Some(current_home) = android_gradle_absolute_user_home_from_environment() else {
            return false;
        };
        current_home == self.gradle_user_home
            && !has_unmodeled_android_gradle_environment(env::vars_os())
            && self.matches_fingerprint()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AndroidPreviewCachePolicy {
    pub key: BuildKey,
    pub disabled_reason: Option<String>,
    pub debug_keystore_hash: Option<String>,
    pub android_signing_fingerprint: Option<String>,
    pub gradle_distribution_identity: Option<AndroidGradleDistributionIdentity>,
}

struct BuildKeyInputs<'a> {
    project_root: &'a Path,
    manifest: &'a Inputs,
    source_manifest_hash: String,
    native: &'a NativeInputs,
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
        .map(|(key, _)| key)
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
    let (key, wrapper_disabled_reason) = build_key_from_inputs(
        BuildKeyInputs {
            project_root: &snapshot.root,
            manifest: &snapshot.manifest,
            source_manifest_hash: snapshot.input_hash.clone(),
            native: &native,
        },
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
        cache_hit_disabled_reason: combine_cache_hit_disabled_reasons([
            frozen.cache_hit_disabled_reason,
            wrapper_disabled_reason,
        ]),
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
    let (key, wrapper_disabled_reason) = build_key_from_inputs(
        BuildKeyInputs {
            project_root: &snapshot.root,
            manifest: &snapshot.manifest,
            source_manifest_hash: snapshot.input_hash.clone(),
            native: &native,
        },
        rust_target.to_string(),
        release,
        toolchain_fingerprint,
        None,
    )?;
    let cache_hit_disabled_reason =
        combine_cache_hit_disabled_reasons([cache_hit_disabled_reason, wrapper_disabled_reason]);
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
        BuildKeyInputs {
            project_root: &root,
            manifest: &manifest,
            source_manifest_hash: manifest.digest(),
            native: &native,
        },
        rust_target.to_string(),
        release,
        toolchain_fingerprint,
        None,
    )
    .map(|(key, _)| key)
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
        android_toolchain_fingerprint(&root),
    );
    build_key_from_inputs(
        BuildKeyInputs {
            project_root: &root,
            manifest: &manifest,
            source_manifest_hash: manifest.digest(),
            native: &native,
        },
        target_triple,
        release,
        toolchain_fingerprint,
        Some(abi_set),
    )
    .map(|(key, _)| key)
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
    abi: &str,
) -> Result<AndroidPreviewCachePolicy> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving Android preview root: {}", root.display()))?;
    let target_triple = android_rust_target(abi)?;
    let mut native = NativeInputs::scan(&root)?;
    let build_script_disabled_reason = local_build_script_cache_disabled_reason(&root)?;
    let wrapper_disabled_reason = android_gradle_wrapper_cache_disabled_reason(&root, &native)?;
    let verification_disabled_reason =
        android_gradle_verification_cache_disabled_reason(&root, &native);
    let dynamic_dependency_disabled_reason =
        android_dynamic_dependency_cache_disabled_reason(&root, &native)?;
    let local_build_logic_disabled_reason =
        android_gradle_local_build_logic_cache_disabled_reason(&native);
    let app_script_io_disabled_reason =
        android_gradle_app_script_io_cache_disabled_reason(&root, &native)?;
    let unknown_plugin_disabled_reason =
        android_gradle_unknown_plugin_cache_disabled_reason(&root, &native)?;
    let custom_repository_disabled_reason =
        android_gradle_custom_repository_cache_disabled_reason(&root, &native)?;
    let global_configuration_disabled_reason =
        android_gradle_global_configuration_cache_disabled_reason(&root);
    let relative_user_home_disabled_reason =
        android_gradle_relative_user_home_cache_disabled_reason(
            env::var_os("GRADLE_USER_HOME").as_deref(),
        );
    let signing_policy = android_cache_signing_policy(&root, release, &mut native)?;
    let (_, rustc_fingerprint) = rustc_identity()?;
    let android_identity = android_toolchain_identity(&root);
    let (toolchain_fingerprint, toolchain_disabled_reason) = bind_android_toolchain_identity(
        &mut native,
        &rustc_fingerprint,
        android_identity
            .as_ref()
            .map(|(fingerprint, _)| fingerprint.clone()),
    );
    let signing_disabled_reason = if release {
        Some("Android release APK is not a live preview cache target".into())
    } else {
        signing_policy.disabled_reason
    };
    let manifest = Inputs::scan_stable(&root, 2)?;
    let (key, wrapper_environment_disabled_reason) = build_key_from_inputs(
        BuildKeyInputs {
            project_root: &root,
            manifest: &manifest,
            source_manifest_hash: manifest.digest(),
            native: &native,
        },
        target_triple.to_owned(),
        release,
        toolchain_fingerprint,
        Some(abi.to_owned()),
    )?;
    Ok(AndroidPreviewCachePolicy {
        key,
        disabled_reason: combine_cache_hit_disabled_reasons([
            compiler_tool_cache_disabled_reason(&root, Some(target_triple)),
            toolchain_disabled_reason,
            wrapper_disabled_reason,
            verification_disabled_reason,
            dynamic_dependency_disabled_reason,
            local_build_logic_disabled_reason,
            app_script_io_disabled_reason,
            unknown_plugin_disabled_reason,
            custom_repository_disabled_reason,
            global_configuration_disabled_reason,
            relative_user_home_disabled_reason,
            signing_disabled_reason,
            build_script_disabled_reason,
            wrapper_environment_disabled_reason,
        ]),
        debug_keystore_hash: signing_policy
            .debug_keystore_identity
            .map(|identity| identity.sha256),
        android_signing_fingerprint: signing_policy
            .signing_identity
            .map(|identity| identity.fingerprint),
        gradle_distribution_identity: android_identity.map(|(_, identity)| identity),
    })
}

/// Returns a conservative cache gate for consumers that derive a BuildKey
/// without a full desktop/iOS build plan, such as matrix preview planning.
pub fn compiler_tool_cache_disabled_reason(
    project_root: &Path,
    target_triple: Option<&str>,
) -> Option<String> {
    let path_environment = env::var("PATH").ok();
    let path_extensions = env::var("PATHEXT").ok();
    let mut remaining = MAX_BUILD_TOOL_FINGERPRINT_BYTES;
    let mut names = FINGERPRINTED_BUILD_TOOLS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    if let Some(target_triple) = target_triple {
        names.extend(
            target_triple
                .split('+')
                .map(normalize_target_environment_name)
                .map(|target| format!("CARGO_TARGET_{target}_LINKER")),
        );
    }
    names
        .into_iter()
        .find_map(|name| {
            let value = env::var(&name).ok()?;
            build_tool_command_fingerprint(
                project_root,
                &value,
                path_environment.as_deref(),
                path_extensions.as_deref(),
                &mut remaining,
            )
            .err()
            .map(|_| BUILD_TOOL_CACHE_DISABLED_REASON.to_owned())
        })
        .or_else(|| non_unicode_build_tool_cache_disabled_reason(target_triple))
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
    let wrapper_disabled_reason =
        android_gradle_wrapper_cache_disabled_reason(&snapshot.root, &native)?;
    let verification_disabled_reason =
        android_gradle_verification_cache_disabled_reason(&snapshot.root, &native);
    let dynamic_dependency_disabled_reason =
        android_dynamic_dependency_cache_disabled_reason(&snapshot.root, &native)?;
    let local_build_logic_disabled_reason =
        android_gradle_local_build_logic_cache_disabled_reason(&native);
    let app_script_io_disabled_reason =
        android_gradle_app_script_io_cache_disabled_reason(&snapshot.root, &native)?;
    let unknown_plugin_disabled_reason =
        android_gradle_unknown_plugin_cache_disabled_reason(&snapshot.root, &native)?;
    let custom_repository_disabled_reason =
        android_gradle_custom_repository_cache_disabled_reason(&snapshot.root, &native)?;
    let global_configuration_disabled_reason =
        android_gradle_global_configuration_cache_disabled_reason(&snapshot.root);
    native
        .excluded_sensitive_files
        .extend(snapshot.manifest.excluded_sensitive_files.iter().cloned());
    native.excluded_sensitive_files.sort();
    native.excluded_sensitive_files.dedup();
    let signing_policy = android_cache_signing_policy(&root, release, &mut native)?;
    if let Some(identity) = &signing_policy.signing_identity {
        identity.copy_into_snapshot(&snapshot, &root)?;
    }
    let android_identity = android_toolchain_identity(&snapshot.root);
    let (toolchain_fingerprint, toolchain_disabled_reason) = bind_android_toolchain_identity(
        &mut native,
        &rustc_fingerprint,
        android_identity
            .as_ref()
            .map(|(fingerprint, _)| fingerprint.clone()),
    );
    let cache_hit_disabled_reason = combine_cache_hit_disabled_reasons([
        toolchain_disabled_reason,
        wrapper_disabled_reason,
        verification_disabled_reason,
        dynamic_dependency_disabled_reason,
        local_build_logic_disabled_reason,
        app_script_io_disabled_reason,
        unknown_plugin_disabled_reason,
        custom_repository_disabled_reason,
        global_configuration_disabled_reason,
        signing_policy.disabled_reason,
        build_script_disabled_reason,
    ]);
    let (key, wrapper_environment_disabled_reason) = build_key_from_inputs(
        BuildKeyInputs {
            project_root: &snapshot.root,
            manifest: &snapshot.manifest,
            source_manifest_hash: snapshot.input_hash.clone(),
            native: &native,
        },
        target_triple,
        release,
        toolchain_fingerprint,
        Some(abi_set),
    )?;
    let cache_hit_disabled_reason = combine_cache_hit_disabled_reasons([
        cache_hit_disabled_reason,
        wrapper_environment_disabled_reason,
    ]);
    let layout =
        BuildOutputLayout::for_key(&root.join(".gpui/builds"), &key, BuildPlatform::Android)?;
    layout.prepare()?;
    Ok(AndroidBuildPlan {
        key,
        layout,
        snapshot,
        cache_hit_disabled_reason,
        gradle_distribution_identity: android_identity.map(|(_, identity)| identity),
        debug_keystore_identity: signing_policy.debug_keystore_identity,
        signing_identity: signing_policy.signing_identity,
    })
}

fn local_build_script_cache_disabled_reason(root: &Path) -> Result<Option<String>> {
    let manifest = Inputs::scan(root)?;
    let source_paths = manifest
        .sources
        .keys()
        .chain(manifest.external_sources.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let has_build_script = manifest
        .sources
        .keys()
        .chain(manifest.external_sources.keys())
        .filter(|path| Path::new(path).file_name() == Some(std::ffi::OsStr::new("Cargo.toml")))
        .any(|manifest_path| {
            let Ok(contents) = fs::read_to_string(root.join(manifest_path)) else {
                return true;
            };
            let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
                return true;
            };
            let Some(package) = value.get("package") else {
                return false;
            };
            match package.get("build") {
                Some(toml::Value::Boolean(enabled)) => *enabled,
                Some(toml::Value::String(_)) => true,
                Some(_) => true,
                None => {
                    let default_script = Path::new(manifest_path)
                        .parent()
                        .map(|parent| parent.join("build.rs"))
                        .unwrap_or_else(|| PathBuf::from("build.rs"));
                    source_paths.contains(&default_script.to_string_lossy().into_owned())
                }
            }
        });
    if has_build_script {
        Ok(Some(
            "local Cargo build.rs hidden inputs/build-script declarations are not modeled; BuildKey cache reuse is disabled".into(),
        ))
    } else {
        Ok(None)
    }
}

fn android_gradle_wrapper_distribution_checksum(
    root: &Path,
    native: &NativeInputs,
) -> Result<Option<String>> {
    if !native
        .files
        .contains_key(ANDROID_GRADLE_WRAPPER_PROPERTIES_RELATIVE)
    {
        return Ok(None);
    }
    let path = root.join(ANDROID_GRADLE_WRAPPER_PROPERTIES_RELATIVE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "checking Android Gradle wrapper properties {}",
                    path.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path).with_context(|| {
        format!(
            "reading Android Gradle wrapper properties {}",
            path.display()
        )
    })?;
    let mut checksum = None;
    let mut distribution_base = None;
    let mut distribution_path = None;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let (key, value) = android_gradle_wrapper_property_key_value(line);
        if key.contains('\\') {
            // Escaped Java-properties keys can override the wrapper location
            // while looking unrelated to a line-oriented parser.
            return Ok(None);
        }
        match key {
            "distributionBase" => {
                if distribution_base.replace(value).is_some() || value != "GRADLE_USER_HOME" {
                    return Ok(None);
                }
            }
            "distributionPath" => {
                if distribution_path.replace(value).is_some() || value != "wrapper/dists" {
                    return Ok(None);
                }
            }
            "distributionSha256Sum" => {
                if checksum.is_some()
                    || value.len() != 64
                    || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Ok(None);
                }
                checksum = Some(value.to_ascii_lowercase());
            }
            _ => {}
        }
    }
    if distribution_base != Some("GRADLE_USER_HOME") || distribution_path != Some("wrapper/dists") {
        return Ok(None);
    }
    Ok(checksum)
}

fn android_gradle_wrapper_property_key_value(line: &str) -> (&str, &str) {
    let boundary = line
        .char_indices()
        .find(|(_, character)| matches!(character, '=' | ':') || character.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(line.len());
    let key = &line[..boundary];
    let remainder = line[boundary..].trim_start();
    let value = remainder
        .strip_prefix('=')
        .or_else(|| remainder.strip_prefix(':'))
        .unwrap_or(remainder)
        .trim();
    (key, value)
}

fn android_gradle_wrapper_cache_disabled_reason(
    root: &Path,
    native: &NativeInputs,
) -> Result<Option<String>> {
    Ok(android_gradle_wrapper_distribution_checksum(root, native)?
        .is_none()
        .then(|| {
            "Android Gradle wrapper checksum or installation layout is unsupported; cache reuse is disabled".into()
        }))
}

fn android_gradle_global_configuration_cache_disabled_reason(root: &Path) -> Option<String> {
    let gradle_user_home = android_gradle_user_home(
        root,
        env::var_os("GRADLE_USER_HOME").as_deref(),
        android_default_user_home().as_deref(),
    );
    android_gradle_global_configuration_cache_disabled_reason_for(
        gradle_user_home.as_deref(),
        has_unmodeled_android_gradle_environment(env::vars_os()),
    )
}

fn android_gradle_relative_user_home_cache_disabled_reason(
    gradle_user_home: Option<&OsStr>,
) -> Option<String> {
    gradle_user_home
        .filter(|path| !Path::new(path).is_absolute())
        .map(|_| ANDROID_GRADLE_RELATIVE_USER_HOME_CACHE_DISABLED_REASON.into())
}

fn android_gradle_global_configuration_cache_disabled_reason_for(
    gradle_user_home: Option<&Path>,
    has_unmodeled_environment: bool,
) -> Option<String> {
    if has_unmodeled_environment
        || gradle_user_home.is_none()
        || gradle_user_home.is_some_and(android_gradle_user_configuration_is_present)
    {
        Some(ANDROID_GRADLE_GLOBAL_CONFIG_CACHE_DISABLED_REASON.into())
    } else {
        None
    }
}

fn android_default_user_home() -> Option<OsString> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = env::var_os("HOME");
    home
}

fn android_gradle_absolute_user_home_from_environment() -> Option<PathBuf> {
    match env::var_os("GRADLE_USER_HOME") {
        Some(path) if Path::new(&path).is_absolute() => Some(PathBuf::from(path)),
        Some(_) => None,
        None => {
            let home = PathBuf::from(android_default_user_home()?);
            home.is_absolute().then(|| home.join(".gradle"))
        }
    }
}

fn android_gradle_user_home(
    root: &Path,
    gradle_user_home: Option<&OsStr>,
    default_user_home: Option<&OsStr>,
) -> Option<PathBuf> {
    let path = match gradle_user_home {
        Some(path) => {
            if path == OsStr::new("") {
                return None;
            }
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                root.join("mobile/android/gradle").join(path)
            }
        }
        None => {
            let home = PathBuf::from(default_user_home?);
            if !home.is_absolute() {
                return None;
            }
            home.join(".gradle")
        }
    };
    path.is_absolute().then_some(path)
}

fn android_gradle_user_configuration_is_present(gradle_user_home: &Path) -> bool {
    match fs::symlink_metadata(gradle_user_home) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if fs::metadata(gradle_user_home).is_err() {
                return true;
            }
        }
        Ok(metadata) if !metadata.is_dir() => return true,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    }

    let user_home_config_is_present = ANDROID_GRADLE_USER_CONFIG_PATHS.iter().any(|relative| {
        match fs::symlink_metadata(gradle_user_home.join(relative)) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => true,
        }
    });
    user_home_config_is_present
        || android_gradle_distribution_init_script_is_present(gradle_user_home)
}

fn android_gradle_distribution_init_script_is_present(gradle_user_home: &Path) -> bool {
    // Gradle wrapper installs use wrapper/dists/<distribution>/<hash>/<gradle-root>;
    // inspect only that bounded layout and fail closed on unknown entries.
    let distributions = gradle_user_home.join("wrapper/dists");
    match fs::symlink_metadata(&distributions) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => return true,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    }

    let mut remaining_entries = MAX_ANDROID_GRADLE_DISTRIBUTION_SCAN_ENTRIES;
    let mut pending = vec![(distributions, 0_u8)];
    while let Some((directory, depth)) = pending.pop() {
        if depth == 3 {
            if android_gradle_installation_init_scripts_are_present(
                &directory,
                &mut remaining_entries,
            ) {
                return true;
            }
            continue;
        }

        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => return true,
        };
        for entry in entries {
            if remaining_entries == 0 {
                return true;
            }
            remaining_entries -= 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return true,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => return true,
            };
            if file_type.is_symlink() {
                return true;
            }
            if file_type.is_dir() {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    false
}

fn android_gradle_installation_init_scripts_are_present(
    gradle_installation: &Path,
    remaining_entries: &mut usize,
) -> bool {
    let init_directory = gradle_installation.join("init.d");
    match fs::symlink_metadata(&init_directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => return true,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    }

    let entries = match fs::read_dir(&init_directory) {
        Ok(entries) => entries,
        Err(_) => return true,
    };
    for entry in entries {
        if *remaining_entries == 0 {
            return true;
        }
        *remaining_entries -= 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return true,
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => return true,
        };
        if file_type.is_symlink() {
            return true;
        }
        let is_distribution_readme = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("readme.txt"));
        if !is_distribution_readme || !file_type.is_file() {
            return true;
        }
    }
    false
}

fn has_unmodeled_android_gradle_environment(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> bool {
    variables.into_iter().any(|(name, _)| {
        let Some(name) = name.to_str() else {
            return false;
        };
        ANDROID_GRADLE_GLOBAL_ENVIRONMENT_NAMES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
            || name
                .get(.."ORG_GRADLE_PROJECT_".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ORG_GRADLE_PROJECT_"))
    })
}

fn android_gradle_verification_cache_disabled_reason(
    root: &Path,
    native: &NativeInputs,
) -> Option<String> {
    let reason = "Android Gradle dependency verification metadata is missing or not strict; cache reuse is disabled";
    let verified = (|| {
        if !native
            .files
            .contains_key(ANDROID_GRADLE_VERIFICATION_METADATA_RELATIVE)
        {
            return Some(false);
        }
        let path = root.join(ANDROID_GRADLE_VERIFICATION_METADATA_RELATIVE);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Some(false);
        }
        let contents = fs::read_to_string(path).ok()?;
        let document = roxmltree::Document::parse(&contents).ok()?;
        let root = document.root_element();
        if root.tag_name().name() != "verification-metadata"
            || root
                .descendants()
                .any(|node| node.is_element() && node.tag_name().name() == "trusted-artifacts")
        {
            return Some(false);
        }
        let configuration = root
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "configuration")?;
        let strict_metadata = configuration
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "verify-metadata")
            .and_then(|node| node.text())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
        let components = root
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "components")?;
        let component_nodes = components
            .children()
            .filter(|node| node.is_element())
            .collect::<Vec<_>>();
        let artifacts_are_hashed = !component_nodes.is_empty()
            && component_nodes.iter().all(|component| {
                let artifacts = component
                    .children()
                    .filter(|node| node.is_element() && node.tag_name().name() == "artifact")
                    .collect::<Vec<_>>();
                !artifacts.is_empty()
                    && artifacts.iter().all(|artifact| {
                        let sha256 = artifact
                            .children()
                            .filter(|node| node.is_element() && node.tag_name().name() == "sha256")
                            .collect::<Vec<_>>();
                        sha256.len() == 1
                            && sha256[0].attribute("value").is_some_and(|value| {
                                value.len() == 64
                                    && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                    })
            });
        Some(strict_metadata && artifacts_are_hashed)
    })()
    .unwrap_or(false);
    (!verified).then(|| reason.into())
}

fn android_dynamic_dependency_cache_disabled_reason(
    root: &Path,
    native: &NativeInputs,
) -> Result<Option<String>> {
    for relative in native
        .files
        .keys()
        .filter(|path| is_android_gradle_dependency_input(path))
    {
        let source = fs::read_to_string(root.join(relative))
            .with_context(|| format!("reading Android Gradle dependency input {relative}"))?;
        if contains_android_dynamic_dependency(&source) {
            return Ok(Some(format!(
                "Android Gradle dynamic/changing dependency input '{relative}' is not cache-stable; cache reuse is disabled"
            )));
        }
    }
    Ok(None)
}

fn android_gradle_local_build_logic_cache_disabled_reason(native: &NativeInputs) -> Option<String> {
    native
        .files
        .keys()
        .any(|path| is_android_gradle_local_build_logic_input(path))
        .then(|| ANDROID_GRADLE_LOCAL_BUILD_LOGIC_CACHE_DISABLED_REASON.into())
}

fn android_gradle_app_script_io_cache_disabled_reason(
    root: &Path,
    native: &NativeInputs,
) -> Result<Option<String>> {
    for relative in native
        .files
        .keys()
        .filter(|path| is_android_gradle_app_script_input(path))
    {
        let source = fs::read_to_string(root.join(relative))
            .with_context(|| format!("reading Android Gradle app script {relative}"))?;
        if contains_android_gradle_unmodeled_io(&source) {
            return Ok(Some(format!(
                "{ANDROID_GRADLE_APP_SCRIPT_IO_CACHE_DISABLED_REASON}: '{relative}'"
            )));
        }
    }
    Ok(None)
}

fn android_gradle_unknown_plugin_cache_disabled_reason(
    root: &Path,
    native: &NativeInputs,
) -> Result<Option<String>> {
    for relative in native
        .files
        .keys()
        .filter(|path| is_android_gradle_app_script_input(path))
    {
        let source = fs::read_to_string(root.join(relative))
            .with_context(|| format!("reading Android Gradle plugin declarations {relative}"))?;
        if android_gradle_script_has_unknown_plugin(&source) {
            return Ok(Some(format!(
                "{ANDROID_GRADLE_UNKNOWN_PLUGIN_CACHE_DISABLED_REASON}: '{relative}'"
            )));
        }
    }
    Ok(None)
}

fn android_gradle_custom_repository_cache_disabled_reason(
    root: &Path,
    native: &NativeInputs,
) -> Result<Option<String>> {
    for relative in native
        .files
        .keys()
        .filter(|path| is_android_gradle_app_script_input(path))
    {
        let source = fs::read_to_string(root.join(relative))
            .with_context(|| format!("reading Android Gradle repositories {relative}"))?;
        if android_gradle_script_has_custom_repository(&source) {
            return Ok(Some(format!(
                "{ANDROID_GRADLE_CUSTOM_REPOSITORY_CACHE_DISABLED_REASON}: '{relative}'"
            )));
        }
    }
    Ok(None)
}

fn android_gradle_script_has_custom_repository(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == "repositories" {
                let Some(opening) = gradle_skip_trivia(source, end) else {
                    return true;
                };
                if bytes.get(opening) != Some(&b'{') {
                    return true;
                }
                let Some(closing) = gradle_matching_brace(source, opening) else {
                    return true;
                };
                if android_gradle_repository_block_has_custom_entry(&source[opening + 1..closing]) {
                    return true;
                }
                cursor = closing + 1;
                continue;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn android_gradle_repository_block_has_custom_entry(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if !matches!(
                &source[position..end],
                "google" | "mavenCentral" | "gradlePluginPortal"
            ) {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn android_gradle_script_has_unknown_plugin(source: &str) -> bool {
    if android_gradle_buildscript_has_unknown_classpath(source) {
        return true;
    }

    if let Some(plugins) = gradle_named_block(source, "plugins")
        && android_gradle_plugins_block_has_unknown_plugin(plugins)
    {
        return true;
    }

    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            if identifier == "apply" && gradle_call_has_unknown_android_plugin(source, end) {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }

    source.lines().any(|line| {
        let line = line.trim();
        (line.starts_with("apply plugin:") || line.starts_with("apply plugin ="))
            && !line.contains("com.android.application")
            && !line.contains("com.android.library")
    })
}

fn android_gradle_buildscript_has_unknown_classpath(source: &str) -> bool {
    let Some(buildscript) = gradle_named_block(source, "buildscript") else {
        return false;
    };

    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(buildscript, cursor) {
        let bytes = buildscript.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &buildscript[position..end] == "classpath" {
                let Some(coordinate) = gradle_first_string_argument(buildscript, end) else {
                    return true;
                };
                if !android_gradle_buildscript_classpath_is_known(&coordinate) {
                    return true;
                }
            }
            if &buildscript[position..end] == "add" {
                match gradle_classpath_add_coordinate(buildscript, end) {
                    Some(Some(coordinate))
                        if !android_gradle_buildscript_classpath_is_known(&coordinate) =>
                    {
                        return true;
                    }
                    Some(None) => return true,
                    None | Some(Some(_)) => {}
                }
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_classpath_add_coordinate(source: &str, method_end: usize) -> Option<Option<String>> {
    let opening = gradle_skip_trivia(source, method_end)?;
    if source.as_bytes().get(opening) != Some(&b'(') {
        return None;
    }
    let first = gradle_skip_trivia(source, opening + 1)?;
    let first_value = gradle_string_literal_at(source, first)?;
    if first_value != "classpath" {
        return None;
    }
    let first_closing = gradle_skip_string(source, first)?;
    let comma = gradle_skip_trivia(source, first_closing)?;
    if source.as_bytes().get(comma) != Some(&b',') {
        return Some(None);
    }
    let second = gradle_skip_trivia(source, comma + 1)?;
    Some(gradle_string_literal_at(source, second))
}

fn android_gradle_buildscript_classpath_is_known(coordinate: &str) -> bool {
    let Some(version) = coordinate.strip_prefix("com.android.tools.build:gradle:") else {
        return false;
    };
    !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn android_gradle_plugins_block_has_unknown_plugin(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            match identifier {
                "id" => {
                    if !gradle_first_string_argument(source, end)
                        .is_some_and(|value| android_gradle_plugin_is_known(&value))
                    {
                        return true;
                    }
                }
                "apply" => {
                    if gradle_call_has_unknown_android_plugin(source, end) {
                        return true;
                    }
                }
                "version" | "false" | "true" => {}
                // Version-catalog aliases and Kotlin/Groovy/Java plugin
                // functions hide the resolved plugin identity.
                _ => return true,
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn android_gradle_plugin_is_known(plugin: &str) -> bool {
    matches!(plugin, "com.android.application" | "com.android.library")
}

fn gradle_call_has_unknown_android_plugin(source: &str, method_end: usize) -> bool {
    let Some(opening) = gradle_skip_trivia(source, method_end) else {
        return true;
    };
    if source.as_bytes().get(opening) != Some(&b'(') {
        return false;
    }
    let Some(argument) = gradle_skip_trivia(source, opening + 1) else {
        return true;
    };
    let Some(plugin) = gradle_named_string_argument(source, argument, "plugin") else {
        return true;
    };
    !android_gradle_plugin_is_known(&plugin)
}

fn gradle_named_string_argument(source: &str, argument: usize, name: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut cursor = argument;
    let mut end = cursor;
    while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
        end += 1;
    }
    if &source[cursor..end] != name {
        return None;
    }
    cursor = gradle_skip_trivia(source, end)?;
    if bytes.get(cursor) != Some(&b'=') {
        return None;
    }
    let value = gradle_skip_trivia(source, cursor + 1)?;
    gradle_string_literal_at(source, value)
}

fn gradle_string_literal_at(source: &str, opening: usize) -> Option<String> {
    let quote = *source.as_bytes().get(opening)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    let closing = gradle_skip_string(source, opening)?;
    let width = if quote == b'"' && source.as_bytes().get(opening..opening + 3) == Some(b"\"\"\"") {
        3
    } else {
        1
    };
    let value = &source[opening + width..closing - width];
    (!value.contains('\\') && !value.contains("${")).then(|| value.to_owned())
}

fn is_android_gradle_app_script_input(path: &str) -> bool {
    let path = Path::new(path);
    let normalized = path.to_string_lossy();
    path.starts_with("mobile/android/gradle")
        && (normalized.ends_with(".gradle") || normalized.ends_with(".gradle.kts"))
        && !is_android_gradle_local_build_logic_input(&normalized)
}

const ANDROID_GRADLE_MANAGED_PROPERTIES: &[&str] =
    &["gpui.abis", "gpui.buildDir", "gpui.jniLibsDir"];
const ANDROID_GRADLE_MANAGED_ENVIRONMENT: &[&str] = &["GPUI_ANDROID_ABIS", "ANDROID_NDK_HOME"];
const ANDROID_GRADLE_FILE_IO_IDENTIFIERS: &[&str] = &[
    "FileInputStream",
    "FileOutputStream",
    "FileReader",
    "FileWriter",
    "RandomAccessFile",
    "FileChannel",
    "AsynchronousFileChannel",
    "SeekableByteChannel",
    "FileSystem",
    "FileSystems",
    "FileSystemProvider",
    "DirectoryStream",
    "SecureDirectoryStream",
    "WatchService",
    "WatchKey",
    "WatchEvent",
    "Watchable",
    "StandardWatchEventKinds",
    "newWatchService",
    "Files",
    "fileTree",
    "zipTree",
    "tarTree",
    "fileContents",
    "fromArchiveEntry",
    "projectDirectory",
    "projectDir",
    "rootDir",
    "gradleLocalProperties",
    "fromFile",
    "fromUri",
    "readText",
    "readBytes",
    "writeText",
    "writeBytes",
    "getText",
    "setText",
    "withReader",
    "withWriter",
    "withInputStream",
    "withOutputStream",
    "appendText",
    "appendBytes",
    "readLines",
    "eachLine",
    "forEachLine",
    "inputStream",
    "outputStream",
    "newInputStream",
    "newOutputStream",
    "newBufferedReader",
    "newBufferedWriter",
    "asFile",
    "getAsFile",
    "getAsFileTree",
    "exists",
    "isFile",
    "isDirectory",
    "listFiles",
    "walkFileTree",
    "FileVisitor",
    "SimpleFileVisitor",
    "FileVisitResult",
    "FileVisitOption",
    "BasicFileAttributes",
    "PathMatcher",
    "getPathMatcher",
    "walk",
    "walkTopDown",
    "walkBottomUp",
    "readAttributes",
    "copy",
    "sync",
    "ant",
    "ClassLoader",
    "ServiceLoader",
    "getResource",
    "getResourceAsStream",
    "getSystemResource",
    "getSystemResourceAsStream",
    "getPath",
    "getCanonicalFile",
    "getCanonicalPath",
    "getAbsolutePath",
    "toPath",
    "toFile",
    "loadClass",
    "forName",
];
const ANDROID_GRADLE_NETWORK_IDENTIFIERS: &[&str] = &[
    "URL",
    "URI",
    "URLClassLoader",
    "HttpURLConnection",
    "URLConnection",
    "HttpClient",
    "HttpRequest",
    "HttpResponse",
    "OkHttpClient",
    "Socket",
    "ServerSocket",
    "DatagramSocket",
    "uri",
];
const ANDROID_GRADLE_FILE_METADATA_IDENTIFIERS: &[&str] = &[
    "canRead",
    "canWrite",
    "canExecute",
    "isHidden",
    "length",
    "lastModified",
    "getFreeSpace",
    "getTotalSpace",
    "getUsableSpace",
    "list",
    "createNewFile",
    "mkdir",
    "mkdirs",
    "delete",
    "deleteOnExit",
    "renameTo",
    "setLastModified",
    "setReadOnly",
    "setWritable",
    "setReadable",
    "setExecutable",
    "isReadable",
    "isWritable",
    "isExecutable",
    "isRegularFile",
    "isSameFile",
    "isSymbolicLink",
    "notExists",
    "getLastModifiedTime",
    "setLastModifiedTime",
    "getFileStore",
    "getFileStores",
    "getFileAttributeView",
    "getRootDirectories",
    "getAttribute",
    "setAttribute",
    "getOwner",
    "setOwner",
    "getPosixFilePermissions",
    "setPosixFilePermissions",
    "readSymbolicLink",
    "createSymbolicLink",
    "createLink",
    "createDirectory",
    "createDirectories",
    "createFile",
    "createTempFile",
    "createTempDirectory",
    "deleteIfExists",
    "move",
    "newByteChannel",
    "newDirectoryStream",
    "readAllBytes",
    "readAllLines",
    "lines",
    "write",
    "find",
    "probeContentType",
    "toRealPath",
];
const ANDROID_GRADLE_ARCHIVE_CONSTRUCTOR_IDENTIFIERS: &[&str] = &[
    "ZipFile",
    "JarFile",
    "ZipInputStream",
    "JarInputStream",
    "ZipOutputStream",
    "JarOutputStream",
    "ZipFileSystemProvider",
];
const ANDROID_GRADLE_FILE_BACKED_CONSTRUCTOR_IDENTIFIERS: &[&str] =
    &["Scanner", "PrintStream", "PrintWriter"];

fn contains_android_gradle_unmodeled_io(source: &str) -> bool {
    // The code-token checks below skip strings, but ${...} expressions can
    // execute arbitrary reads during configuration. Treat their evaluation as
    // unmodeled even when it happens to be pure. Simple $name references keep
    // the existing policy for the code that supplies their values.
    if gradle_contains_expression_interpolation(source) {
        return true;
    }

    if gradle_provider_call_uses_unmodeled_value(
        source,
        "gradleProperty",
        ANDROID_GRADLE_MANAGED_PROPERTIES,
    ) || gradle_provider_call_uses_unmodeled_value(
        source,
        "environmentVariable",
        ANDROID_GRADLE_MANAGED_ENVIRONMENT,
    ) {
        return true;
    }

    if gradle_file_api_uses_unmodeled_path(source) {
        return true;
    }

    if gradle_path_constructor_uses_unmodeled_path(source) {
        return true;
    }

    if gradle_file_collection_from_uses_unmodeled_path(source) {
        return true;
    }

    if gradle_source_root_api_uses_unmodeled_path(source) {
        return true;
    }

    if gradle_file_metadata_api_uses_unmodeled_path(source) {
        return true;
    }

    if gradle_constructor_uses_unmodeled_io(source, ANDROID_GRADLE_ARCHIVE_CONSTRUCTOR_IDENTIFIERS)
        || gradle_constructor_uses_unmodeled_io(
            source,
            ANDROID_GRADLE_FILE_BACKED_CONSTRUCTOR_IDENTIFIERS,
        )
    {
        return true;
    }

    if [
        &["System", "getenv"][..],
        &["System", "getProperty"][..],
        &["System", "getProperties"][..],
        &["System", "setProperty"][..],
        &["System", "clearProperty"][..],
        &["project", "property"][..],
        &["gradle", "startParameter", "projectProperties"][..],
        &["providers", "of"][..],
        &["providers", "provider"][..],
    ]
    .iter()
    .any(|sequence| gradle_contains_identifier_sequence(source, sequence))
        || gradle_contains_identifier(source, "findProperty")
        || gradle_contains_identifier(source, "systemProperty")
        || gradle_contains_identifier(source, "projectProperties")
        || gradle_contains_identifier(source, "ProcessBuilder")
        || gradle_contains_identifier(source, "Runtime")
        || gradle_contains_identifier(source, "exec")
        || gradle_contains_identifier(source, "javaexec")
        || gradle_contains_identifier(source, "commandLine")
        || gradle_contains_identifier(source, "ValueSource")
        || gradle_contains_identifier_sequence(source, &["apply", "from"])
        || gradle_contains_identifier(source, "includeBuild")
    {
        return true;
    }

    if ANDROID_GRADLE_NETWORK_IDENTIFIERS
        .iter()
        .any(|identifier| gradle_contains_identifier(source, identifier))
    {
        return true;
    }

    ANDROID_GRADLE_FILE_IO_IDENTIFIERS.iter().any(|identifier| {
        gradle_contains_identifier(source, identifier)
            && !android_gradle_identifier_is_managed_io(source, identifier)
    })
}

fn gradle_contains_expression_interpolation(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    while let Some(position) = gradle_skip_trivia(source, cursor) {
        match bytes.get(position) {
            Some(b'\'') => {
                let Some(end) = gradle_skip_string(source, position) else {
                    break;
                };
                cursor = end;
            }
            Some(b'"') => {
                let triple = bytes.get(position..position + 3) == Some(b"\"\"\"");
                let width = if triple { 3 } else { 1 };
                cursor = position + width;
                while cursor < bytes.len() {
                    if !triple && bytes[cursor] == b'\\' {
                        cursor += 2;
                    } else if bytes.get(cursor..cursor + width) == Some(&b"\"\"\""[..width]) {
                        cursor += width;
                        break;
                    } else if bytes.get(cursor..cursor + 2) == Some(b"${") {
                        return true;
                    } else {
                        cursor += 1;
                    }
                }
            }
            Some(_) => cursor = position + 1,
            None => break,
        }
    }
    false
}

fn gradle_file_api_uses_unmodeled_path(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            if matches!(identifier, "file" | "files")
                && gradle_file_call_has_path_argument(source, end)
                && !gradle_file_call_is_managed(source, identifier, end)
            {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_path_constructor_uses_unmodeled_path(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            if identifier == "File" && gradle_call_has_opening_parenthesis(source, end) {
                return true;
            }
            if (identifier == "Paths" && gradle_member_call_has_name(source, end, "get"))
                || (identifier == "Path" && gradle_member_call_has_name(source, end, "of"))
            {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_file_collection_from_uses_unmodeled_path(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == "from" && gradle_from_call_has_argument(source, end) {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_from_call_has_argument(source: &str, method_end: usize) -> bool {
    let Some(argument) = gradle_skip_trivia(source, method_end) else {
        return false;
    };
    match source.as_bytes().get(argument) {
        Some(b'(' | b'[' | b'{') => true,
        Some(byte) if is_gradle_identifier_start(*byte) => true,
        Some(b'"' | b'\'') => true,
        _ => false,
    }
}

fn gradle_member_call_has_name(source: &str, member_owner_end: usize, member: &str) -> bool {
    let Some(dot) = gradle_skip_trivia(source, member_owner_end) else {
        return false;
    };
    if source.as_bytes().get(dot) != Some(&b'.') {
        return false;
    }
    let Some(member_start) = gradle_skip_trivia(source, dot + 1) else {
        return false;
    };
    let Some(member_end) = member_start.checked_add(member.len()) else {
        return false;
    };
    source.get(member_start..member_end) == Some(member)
        && gradle_call_has_opening_parenthesis(source, member_end)
}

fn gradle_source_root_api_uses_unmodeled_path(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            if matches!(identifier, "srcDir" | "srcDirs" | "setSrcDirs") {
                let managed_template_root = identifier == "srcDirs"
                    && gradle_call_has_opening_parenthesis(source, end)
                    && gradle_source_root_call_is_managed(source, identifier, end);
                if !managed_template_root {
                    return true;
                }
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_file_metadata_api_uses_unmodeled_path(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            let receiver = source[..position].trim_end();
            if ANDROID_GRADLE_FILE_METADATA_IDENTIFIERS.contains(&identifier)
                && receiver.ends_with('.')
                && gradle_call_has_opening_parenthesis(source, end)
            {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_constructor_uses_unmodeled_io(source: &str, identifiers: &[&str]) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            if identifiers.contains(&identifier) && gradle_call_has_opening_parenthesis(source, end)
            {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_source_root_call_is_managed(source: &str, identifier: &str, method_end: usize) -> bool {
    identifier == "srcDirs"
        && gradle_call_has_exact_argument(source, method_end, "gpuiJniLibsDir")
        && gradle_call_has_string_argument(source, "gradleProperty", "gpui.jniLibsDir")
        && gradle_contains_identifiers_in_order(source, &["sourceSets", "jniLibs", "srcDirs"])
}

fn gradle_file_call_is_managed(source: &str, identifier: &str, method_end: usize) -> bool {
    if identifier != "file" {
        return false;
    }

    if gradle_call_has_exact_argument(source, method_end, "configuredBuildDir") {
        return gradle_call_has_string_argument(source, "gradleProperty", "gpui.buildDir");
    }

    if gradle_call_has_string_argument(source, "environmentVariable", "ANDROID_NDK_HOME")
        && source.contains("ndkDirectory")
        && source.contains("source.properties")
        && (gradle_call_has_exact_argument(source, method_end, "ndkHome.get()")
            || gradle_call_has_exact_argument(source, method_end, "ndkHome")
            || gradle_call_has_exact_argument(source, method_end, "\"ndk\""))
    {
        return true;
    }

    false
}

fn gradle_call_has_opening_parenthesis(source: &str, method_end: usize) -> bool {
    gradle_skip_trivia(source, method_end).and_then(|opening| source.as_bytes().get(opening))
        == Some(&b'(')
}

fn gradle_file_call_has_path_argument(source: &str, method_end: usize) -> bool {
    let Some(argument) = gradle_skip_trivia(source, method_end) else {
        return false;
    };
    match source.as_bytes().get(argument) {
        Some(b'(' | b'[' | b'{' | b'"' | b'\'') => true,
        Some(byte) if is_gradle_identifier_start(*byte) => true,
        _ => false,
    }
}

fn gradle_call_has_exact_argument(source: &str, method_end: usize, expected: &str) -> bool {
    let Some(opening) = gradle_skip_trivia(source, method_end) else {
        return false;
    };
    if source.as_bytes().get(opening) != Some(&b'(') {
        return false;
    }
    let Some(argument) = gradle_skip_trivia(source, opening + 1) else {
        return false;
    };
    let Some(end) = argument.checked_add(expected.len()) else {
        return false;
    };
    source.get(argument..end) == Some(expected)
        && gradle_skip_trivia(source, end).and_then(|closing| source.as_bytes().get(closing))
            == Some(&b')')
}

fn gradle_contains_identifiers_in_order(source: &str, sequence: &[&str]) -> bool {
    if sequence.is_empty() {
        return false;
    }
    let mut matched = 0;
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == sequence[matched] {
                matched += 1;
                if matched == sequence.len() {
                    return true;
                }
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn android_gradle_identifier_is_managed_io(source: &str, identifier: &str) -> bool {
    match identifier {
        "FileInputStream" => {
            gradle_contains_identifier_sequence(source, &["keystoreProperties", "load"])
                && gradle_file_input_stream_calls_are_managed(source)
        }
        "inputStream" => {
            gradle_call_has_string_argument(source, "environmentVariable", "ANDROID_NDK_HOME")
                && gradle_ndk_input_stream_calls_are_managed(source)
        }
        _ => false,
    }
}

// An exception covers each specific read, not every use of an I/O identifier
// in a script that happens to contain one managed read.
fn gradle_file_input_stream_calls_are_managed(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut found_call = false;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == "FileInputStream" {
                if let Some(closing) =
                    gradle_single_string_call_end(source, end, "keystore.properties")
                {
                    found_call = true;
                    cursor = closing;
                    continue;
                }
                if !gradle_is_plain_import(source, position, "java.io.FileInputStream") {
                    return false;
                }
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    found_call
}

fn gradle_ndk_input_stream_calls_are_managed(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut found_call = false;
    let mut previous_is_dot = false;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            if identifier == "ndkDirectory"
                && !previous_is_dot
                && let Some(closing) = gradle_managed_ndk_stream_end(source, end)
            {
                found_call = true;
                cursor = closing;
                previous_is_dot = false;
                continue;
            }
            if identifier == "inputStream"
                && !gradle_is_plain_import(source, position, "kotlin.io.inputStream")
            {
                return false;
            }
            cursor = end;
            previous_is_dot = false;
        } else {
            previous_is_dot = bytes[position] == b'.';
            cursor = position + 1;
        }
    }
    found_call
}

fn gradle_managed_ndk_stream_end(source: &str, receiver_end: usize) -> Option<usize> {
    let resolve = gradle_member_identifier_end(source, receiver_end, "resolve")?;
    let resolved = gradle_single_string_call_end(source, resolve, "source.properties")?;
    let stream = gradle_member_identifier_end(source, resolved, "inputStream")?;
    let opening = gradle_skip_trivia(source, stream)?;
    if source.as_bytes().get(opening) != Some(&b'(') {
        return None;
    }
    let closing = gradle_skip_trivia(source, opening + 1)?;
    (source.as_bytes().get(closing) == Some(&b')')).then_some(closing + 1)
}

fn gradle_member_identifier_end(source: &str, receiver_end: usize, member: &str) -> Option<usize> {
    let dot = gradle_skip_trivia(source, receiver_end)?;
    if source.as_bytes().get(dot) != Some(&b'.') {
        return None;
    }
    let start = gradle_skip_trivia(source, dot + 1)?;
    let end = start.checked_add(member.len())?;
    (source.get(start..end) == Some(member)).then_some(end)
}

fn gradle_single_string_call_end(source: &str, method_end: usize, expected: &str) -> Option<usize> {
    let opening = gradle_skip_trivia(source, method_end)?;
    if source.as_bytes().get(opening) != Some(&b'(') {
        return None;
    }
    let argument = gradle_skip_trivia(source, opening + 1)?;
    if gradle_string_literal_at(source, argument)? != expected {
        return None;
    }
    let literal_end = gradle_skip_string(source, argument)?;
    let closing = gradle_skip_trivia(source, literal_end)?;
    (source.as_bytes().get(closing) == Some(&b')')).then_some(closing + 1)
}

fn gradle_is_plain_import(source: &str, position: usize, qualified_name: &str) -> bool {
    let start = source[..position]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let end = source[position..]
        .find('\n')
        .map_or(source.len(), |newline| position + newline);
    let line = &source[start..end];
    if !line.trim_start().starts_with("import") {
        return false;
    }
    let mut code = String::new();
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(line, cursor) {
        code.push(line.as_bytes()[position] as char);
        cursor = position + 1;
    }
    code.trim_end_matches(';') == format!("import{qualified_name}")
}

fn gradle_provider_call_uses_unmodeled_value(
    source: &str,
    method: &str,
    managed_values: &[&str],
) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == method
                && let Some(argument) = gradle_first_string_argument(source, end)
                && !managed_values.contains(&argument.as_str())
            {
                return true;
            }
            if &source[position..end] == method
                && gradle_call_is_non_literal_or_missing_argument(source, end)
            {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_call_has_string_argument(source: &str, method: &str, expected: &str) -> bool {
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            if &source[position..end] == method
                && gradle_first_string_argument(source, end).as_deref() == Some(expected)
            {
                return true;
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn gradle_call_is_non_literal_or_missing_argument(source: &str, method_end: usize) -> bool {
    let Some(opening) = gradle_skip_trivia(source, method_end) else {
        return true;
    };
    source.as_bytes().get(opening) != Some(&b'(')
        || gradle_skip_trivia(source, opening + 1)
            .and_then(|argument| source.as_bytes().get(argument))
            .is_none_or(|byte| !matches!(byte, b'"' | b'\''))
}

fn gradle_first_string_argument(source: &str, method_end: usize) -> Option<String> {
    let opening = gradle_skip_trivia(source, method_end)?;
    if source.as_bytes().get(opening) != Some(&b'(') {
        return None;
    }
    let argument = gradle_skip_trivia(source, opening + 1)?;
    let quote = *source.as_bytes().get(argument)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    let closing = gradle_skip_string(source, argument)?;
    let width = if quote == b'"' && source.as_bytes().get(argument..argument + 3) == Some(b"\"\"\"")
    {
        3
    } else {
        1
    };
    let value = &source[argument + width..closing - width];
    (!value.contains('\\') && !value.contains("${")).then(|| value.to_owned())
}

fn gradle_contains_identifier_sequence(source: &str, sequence: &[&str]) -> bool {
    if sequence.is_empty() {
        return false;
    }
    let mut matched = 0;
    let mut cursor = 0;
    while let Some(position) = gradle_next_code_position(source, cursor) {
        let bytes = source.as_bytes();
        if is_gradle_identifier_start(bytes[position]) {
            let mut end = position + 1;
            while end < bytes.len() && is_gradle_identifier_continue(bytes[end]) {
                end += 1;
            }
            let identifier = &source[position..end];
            if identifier == sequence[matched] {
                matched += 1;
                if matched == sequence.len() {
                    return true;
                }
            } else {
                matched = usize::from(identifier == sequence[0]);
            }
            cursor = end;
        } else {
            cursor = position + 1;
        }
    }
    false
}

fn is_android_gradle_local_build_logic_input(path: &str) -> bool {
    let path = Path::new(path);
    path.strip_prefix("mobile/android/gradle/buildSrc").is_ok()
        || path
            .strip_prefix("mobile/android/gradle/build-logic")
            .is_ok()
}

fn is_android_gradle_dependency_input(path: &str) -> bool {
    let path = Path::new(path);
    if !path.starts_with("mobile/android/gradle") {
        return false;
    }
    match path.extension().and_then(OsStr::to_str) {
        Some("gradle" | "kts" | "toml") => true,
        Some("kt" | "groovy" | "java") => path.components().any(|component| {
            let name = component.as_os_str();
            name == OsStr::new("buildSrc") || name == OsStr::new("build-logic")
        }),
        _ => false,
    }
}

fn contains_android_dynamic_dependency(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    let compact = lower
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if lower.contains("snapshot")
        || lower.contains("latest.release")
        || lower.contains("latest.integration")
        || lower.contains("cachedynamicversionsfor")
        || lower.contains("cachechangingmodulesfor")
        || compact.contains("changing=true")
        || compact.contains("changing:true")
        || compact.contains("ischanging=true")
        || compact.contains("ischanging:true")
        || compact.contains("setchanging(true)")
    {
        return true;
    }

    source.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        let dependency_declaration = [
            "implementation",
            "api(",
            "runtimeonly",
            "classpath",
            "version",
            "plugin",
            "dependency",
            "constraint",
        ]
        .iter()
        .any(|marker| lower.contains(marker));
        let version_value = line.contains('=') || dependency_declaration;
        version_value
            && (quoted_value_contains(line, '+') || quoted_value_contains_dynamic_range(line))
    })
}

fn quoted_value_contains(source: &str, wanted: char) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(delimiter) if character == delimiter => quote = None,
            Some(_) if character == wanted => return true,
            None if matches!(character, '\'' | '"') => quote = Some(character),
            _ => {}
        }
    }
    false
}

fn quoted_value_contains_dynamic_range(source: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let mut value = String::new();
    for character in source.chars() {
        if escaped {
            if quote.is_some() {
                value.push(character);
            }
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(delimiter) if character == delimiter => {
                if (value.contains('[') || value.contains('('))
                    && value.contains(',')
                    && (value.contains(']') || value.contains(')'))
                {
                    return true;
                }
                value.clear();
                quote = None;
            }
            Some(_) => value.push(character),
            None if matches!(character, '\'' | '"') => quote = Some(character),
            _ => {}
        }
    }
    false
}

fn android_toolchain_fingerprint(project_root: &Path) -> Option<String> {
    android_toolchain_identity(project_root).map(|(fingerprint, _)| fingerprint)
}

fn android_toolchain_identity(
    project_root: &Path,
) -> Option<(String, AndroidGradleDistributionIdentity)> {
    let sdk_root = consistent_environment_directory(&["ANDROID_HOME", "ANDROID_SDK_ROOT"])?;
    let ndk_home = consistent_environment_directory(&["ANDROID_NDK_HOME"])?;
    if env::var_os("NDK_HOME").is_some()
        && consistent_environment_directory(&["NDK_HOME"]).as_ref() != Some(&ndk_home)
    {
        return None;
    }
    let sdk_packages = android_sdk_package_fingerprint(&sdk_root, project_root)?;
    let ndk_revision = android_package_revision(&ndk_home.join("source.properties"))?;
    let ndk_compiler_tools = android_ndk_compiler_tool_fingerprint(&ndk_home)?;
    let ndk_compiler_resources =
        android_ndk_compiler_resource_fingerprint(&android_ndk_host_root(&ndk_home)?)?;
    let cargo_ndk = command_version("cargo", &["ndk", "--version"], false)?;
    let (java, java_home) = java_runtime_details()?;
    let java_runtime = java_runtime_fingerprint(&java_home)?;
    let gradle_distribution = android_gradle_distribution_identity(project_root)?;
    let identity = format!(
        "sdk-packages={sdk_packages}\nndk-revision={ndk_revision}\nndk-compiler-tools={ndk_compiler_tools}\nndk-compiler-resources={ndk_compiler_resources}\ncargo-ndk={cargo_ndk}\njava={java}\njava-runtime={java_runtime}\ngradle-wrapper-distributions={}",
        gradle_distribution.fingerprint()
    );
    Some((
        format!("{:x}", Sha256::digest(identity.as_bytes())),
        gradle_distribution,
    ))
}

fn android_gradle_distribution_identity(
    project_root: &Path,
) -> Option<AndroidGradleDistributionIdentity> {
    let gradle_user_home = android_gradle_user_home(
        project_root,
        env::var_os("GRADLE_USER_HOME").as_deref(),
        android_default_user_home().as_deref(),
    )?;
    android_gradle_distribution_identity_for(&gradle_user_home)
}

fn android_gradle_distribution_identity_for(
    gradle_user_home: &Path,
) -> Option<AndroidGradleDistributionIdentity> {
    let fingerprint = android_gradle_distribution_fingerprint_for(gradle_user_home)?;
    Some(AndroidGradleDistributionIdentity {
        gradle_user_home: gradle_user_home.to_owned(),
        fingerprint,
    })
}

pub(crate) fn android_gradle_distribution_fingerprint_for(
    gradle_user_home: &Path,
) -> Option<String> {
    android_gradle_distribution_fingerprint_with_budget(
        gradle_user_home,
        &mut DirectoryFingerprintBudget::default(),
    )
}

fn android_gradle_distribution_fingerprint_with_budget(
    gradle_user_home: &Path,
    budget: &mut DirectoryFingerprintBudget,
) -> Option<String> {
    let distributions = gradle_user_home.join("wrapper/dists");
    let metadata = match fs::symlink_metadata(&distributions) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return None,
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }

    let mut pending = vec![(distributions.clone(), 0_u8)];
    let mut installations = Vec::new();
    let mut wrapper_files = Vec::new();
    while let Some((directory, depth)) = pending.pop() {
        if depth == 3 {
            let relative = directory.strip_prefix(&distributions).ok()?;
            let relative = relative.to_str()?.replace('\\', "/");
            let fingerprint = bounded_directory_fingerprint(
                &directory,
                b"gpui-android-gradle-installation-v1\0",
                budget,
            )?;
            installations.push((relative, fingerprint));
            continue;
        }

        let mut children = fs::read_dir(&directory)
            .ok()?
            .map(|entry| entry.ok())
            .collect::<Option<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let file_type = entry.file_type().ok()?;
            if file_type.is_symlink() {
                return None;
            }
            if file_type.is_dir() {
                budget.record(0)?;
                pending.push((entry.path(), depth + 1));
            } else if file_type.is_file() {
                let name = entry.file_name().into_string().ok()?;
                if name.to_ascii_lowercase().ends_with(".lck") {
                    budget.record(0)?;
                    continue;
                }
                let path = entry.path();
                let relative = path.strip_prefix(&distributions).ok()?;
                let relative = relative.to_str()?.replace('\\', "/");
                let metadata = entry.metadata().ok()?;
                budget.record(metadata.len())?;
                wrapper_files.push((relative, hash_file_contents(&path).ok()?));
            } else {
                return None;
            }
        }
    }
    if installations.is_empty() {
        // The wrapper may install Gradle during this build. Do not publish a
        // cache entry whose BuildKey was computed without the runtime contents.
        return None;
    }

    installations.sort();
    wrapper_files.sort();
    let mut digest = Sha256::new();
    digest.update(ANDROID_GRADLE_DISTRIBUTION_FINGERPRINT_DOMAIN);
    for (relative, fingerprint) in installations {
        digest.update(b"d");
        digest.update((relative.len() as u64).to_be_bytes());
        digest.update(relative.as_bytes());
        digest.update(fingerprint.as_bytes());
    }
    for (relative, fingerprint) in wrapper_files {
        digest.update(b"f");
        digest.update((relative.len() as u64).to_be_bytes());
        digest.update(relative.as_bytes());
        digest.update(fingerprint.as_bytes());
    }
    Some(format!("{:x}", digest.finalize()))
}

fn android_ndk_host_tag_candidates() -> &'static [&'static str] {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => &["darwin-arm64", "darwin-x86_64"],
        ("macos", _) => &["darwin-x86_64", "darwin-arm64"],
        ("linux", "aarch64") => &["linux-aarch64", "linux-x86_64"],
        ("linux", _) => &["linux-x86_64", "linux-aarch64"],
        ("windows", _) => &["windows-x86_64"],
        _ => &[],
    }
}

fn android_ndk_host_root(ndk_home: &Path) -> Option<PathBuf> {
    let prebuilt = ndk_home.join("toolchains/llvm/prebuilt");
    let host_root = android_ndk_host_tag_candidates()
        .iter()
        .map(|tag| prebuilt.join(tag))
        .find(|path| path.is_dir())?;
    Some(host_root)
}

fn android_ndk_compiler_tool_fingerprint(ndk_home: &Path) -> Option<String> {
    let host_root = android_ndk_host_root(ndk_home)?;
    let bin = host_root.join("bin");
    let required_tools = [
        "clang",
        "clang++",
        "lld",
        "ld.lld",
        "llvm-ar",
        "llvm-ranlib",
        "llvm-strip",
        "llvm-objcopy",
        "llvm-nm",
        "llvm-readelf",
    ];
    let mut entries = Vec::new();
    let mut content_hashes = BTreeMap::new();
    for name in required_tools {
        let path = android_ndk_tool_path(&bin, name)?;
        entries.push(android_ndk_tool_entry(&bin, &path, &mut content_hashes)?);
    }

    let directory = fs::read_dir(&bin).ok()?;
    for entry in directory {
        let entry = entry.ok()?;
        let name = entry.file_name().into_string().ok()?;
        let normalized = name.strip_suffix(".exe").unwrap_or(&name);
        if !(normalized.ends_with("-clang") || normalized.ends_with("-clang++"))
            || !(normalized.contains("-linux-android") || normalized.contains("-linux-androideabi"))
        {
            continue;
        }
        entries.push(android_ndk_tool_entry(
            &bin,
            &entry.path(),
            &mut content_hashes,
        )?);
    }
    entries.sort();
    Some(format!(
        "host={};{}",
        host_root.file_name()?.to_string_lossy(),
        entries.join("\n")
    ))
}

fn android_ndk_compiler_resource_fingerprint(host_root: &Path) -> Option<String> {
    let sysroot = host_root.join("sysroot");
    let sysroot_fingerprint = android_ndk_directory_fingerprint(&sysroot)?;
    let clang_root = host_root.join("lib/clang");
    let metadata = fs::symlink_metadata(&clang_root).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let mut clang_versions = Vec::new();
    for entry in fs::read_dir(&clang_root).ok()? {
        let entry = entry.ok()?;
        let version = entry.file_name().into_string().ok()?;
        let version_metadata = fs::symlink_metadata(entry.path()).ok()?;
        if version_metadata.file_type().is_symlink() || !version_metadata.is_dir() {
            return None;
        }
        let include = entry.path().join("include");
        clang_versions.push(format!(
            "{version}={}",
            android_ndk_directory_fingerprint(&include)?
        ));
    }
    if clang_versions.is_empty() {
        return None;
    }
    clang_versions.sort();
    Some(format!(
        "sysroot={sysroot_fingerprint}\nclang-includes={}",
        clang_versions.join("\n")
    ))
}

#[derive(Clone, Copy)]
struct DirectoryFingerprintBudget {
    entries: usize,
    bytes: u64,
    max_entries: usize,
    max_bytes: u64,
}

impl DirectoryFingerprintBudget {
    fn new(max_entries: usize, max_bytes: u64) -> Self {
        Self {
            entries: 0,
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    fn record(&mut self, bytes: u64) -> Option<()> {
        self.entries = self.entries.checked_add(1)?;
        self.bytes = self.bytes.checked_add(bytes)?;
        (self.entries <= self.max_entries && self.bytes <= self.max_bytes).then_some(())
    }
}

impl Default for DirectoryFingerprintBudget {
    fn default() -> Self {
        Self::new(100_000, 512 * 1024 * 1024)
    }
}

fn android_ndk_directory_fingerprint(root: &Path) -> Option<String> {
    bounded_directory_fingerprint(
        root,
        b"gpui-android-ndk-directory-v1\0",
        &mut DirectoryFingerprintBudget::default(),
    )
}

fn bounded_directory_fingerprint(
    root: &Path,
    domain: &[u8],
    budget: &mut DirectoryFingerprintBudget,
) -> Option<String> {
    let metadata = fs::symlink_metadata(root).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let canonical_root = fs::canonicalize(root).ok()?;
    let mut pending = vec![canonical_root.clone()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut children = fs::read_dir(&directory)
            .ok()?
            .map(|entry| entry.ok())
            .collect::<Option<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let path = entry.path();
            let relative = path.strip_prefix(&canonical_root).ok()?;
            let relative = relative.to_str()?.replace('\\', "/");
            let metadata = fs::symlink_metadata(&path).ok()?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return None;
            }
            if metadata.is_dir() {
                budget.record(0)?;
                entries.push((relative, path.clone(), b'd'));
                pending.push(path);
            } else if metadata.is_file() {
                budget.record(metadata.len())?;
                entries.push((relative, path, b'f'));
            } else {
                return None;
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut digest = Sha256::new();
    digest.update(domain);
    for (relative, path, kind) in entries {
        digest.update([kind]);
        digest.update((relative.len() as u64).to_be_bytes());
        digest.update(relative.as_bytes());
        if kind == b'f' {
            let content_hash = hash_file_contents(&path).ok()?;
            digest.update(content_hash.as_bytes());
        }
    }
    Some(format!("{:x}", digest.finalize()))
}

fn android_ndk_tool_path(bin: &Path, name: &str) -> Option<PathBuf> {
    let path = bin.join(name);
    if path.exists() {
        return Some(path);
    }
    cfg!(windows)
        .then(|| bin.join(format!("{name}.exe")))
        .filter(|path| path.exists())
}

fn android_ndk_tool_entry(
    bin: &Path,
    path: &Path,
    content_hashes: &mut BTreeMap<PathBuf, String>,
) -> Option<String> {
    let name = path
        .strip_prefix(bin)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let link_target = fs::read_link(path)
        .ok()
        .map(|target| target.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let resolved = fs::canonicalize(path).ok()?;
    let metadata = fs::symlink_metadata(&resolved).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let digest = if let Some(digest) = content_hashes.get(&resolved) {
        digest.clone()
    } else {
        let digest = hash_file_contents(&resolved).ok()?;
        content_hashes.insert(resolved, digest.clone());
        digest
    };
    Some(format!("{name}|link={link_target}|sha256={digest}"))
}

fn hash_file_contents(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
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

fn android_sdk_package_fingerprint(sdk_root: &Path, project_root: &Path) -> Option<String> {
    let mut packages = Vec::new();
    let mut budget = DirectoryFingerprintBudget::default();
    let (platforms, build_tools) = android_required_sdk_packages(sdk_root, project_root)?;
    for (category, required) in [("platforms", platforms), ("build-tools", build_tools)] {
        let category_path = sdk_root.join(category);
        let category_metadata = fs::symlink_metadata(&category_path).ok()?;
        if category_metadata.file_type().is_symlink() || !category_metadata.is_dir() {
            return None;
        }
        for name in required {
            let package_path = category_path.join(&name);
            let metadata = fs::symlink_metadata(&package_path).ok()?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return None;
            }
            let revision = android_package_revision(&package_path.join("source.properties"))?;
            let contents = bounded_directory_fingerprint(
                &package_path,
                b"gpui-android-sdk-package-v1\0",
                &mut budget,
            )?;
            packages.push(format!("{category}/{name}={revision};contents={contents}"));
        }
    }
    packages.sort();
    Some(packages.join("\n"))
}

fn android_required_sdk_packages(
    sdk_root: &Path,
    project_root: &Path,
) -> Option<(Vec<String>, Vec<String>)> {
    let manifest = [
        project_root.join("mobile/android/gradle/app/build.gradle.kts"),
        project_root.join("gradle/app/build.gradle.kts"),
    ]
    .into_iter()
    .find(|path| path.is_file())?;
    let metadata = fs::symlink_metadata(&manifest).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let source = fs::read_to_string(manifest).ok()?;
    let compile_sdk = android_gradle_integer_setting(&source, "compileSdk")?;
    let build_tools = match android_gradle_setting_value(&source, "buildToolsVersion")? {
        Some(value) => vec![android_gradle_string_literal(value)?],
        None => vec![android_latest_build_tools_package(sdk_root)?],
    };
    Some((vec![format!("android-{compile_sdk}")], build_tools))
}

fn android_gradle_integer_setting(source: &str, setting: &str) -> Option<u32> {
    android_gradle_setting_value(source, setting)?.and_then(|value| value.parse::<u32>().ok())
}

fn android_gradle_setting_value<'a>(source: &'a str, setting: &str) -> Option<Option<&'a str>> {
    let mut values = source.lines().filter_map(|line| {
        let line = line
            .split_once("//")
            .map_or(line, |(before, _)| before)
            .trim();
        if let Some((name, value)) = line.split_once('=') {
            return (name.trim() == setting).then_some(value.trim());
        }
        let value = line.strip_prefix(setting)?;
        value
            .starts_with(char::is_whitespace)
            .then_some(value.trim())
    });
    let Some(value) = values.next() else {
        return Some(None);
    };
    values.next().is_none().then_some(Some(value))
}

fn android_gradle_string_literal(value: &str) -> Option<String> {
    let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    android_sdk_version_is_numeric(value).then(|| value.to_owned())
}

fn android_sdk_version_is_numeric(version: &str) -> bool {
    !version.is_empty()
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn android_latest_build_tools_package(sdk_root: &Path) -> Option<String> {
    let directory = sdk_root.join("build-tools");
    let metadata = fs::symlink_metadata(&directory).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let mut versions = fs::read_dir(directory)
        .ok()?
        .map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect::<Option<Vec<_>>>()?;
    versions.retain(|version| android_sdk_version_is_numeric(version));
    versions.sort_by_cached_key(|version| {
        version
            .split('.')
            .filter_map(|part| part.parse::<u32>().ok())
            .collect::<Vec<_>>()
    });
    versions.pop()
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

fn java_runtime_details() -> Option<(String, PathBuf)> {
    let (program, args) = match env::var_os("JAVA_HOME") {
        Some(home) if !home.is_empty() => (
            PathBuf::from(home)
                .join("bin")
                .join(if cfg!(windows) { "java.exe" } else { "java" }),
            vec!["-XshowSettings:properties", "-version"],
        ),
        Some(_) => return None,
        None => (
            PathBuf::from("java"),
            vec!["-XshowSettings:properties", "-version"],
        ),
    };
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut version = None;
    let mut home = None;
    for line in stdout.lines().chain(stderr.lines()) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "java.version" => version = Some(value.trim().to_owned()),
            "java.home" => home = Some(PathBuf::from(value.trim())),
            _ => {}
        }
    }
    let version = version.filter(|value| !value.is_empty())?;
    let home = home.filter(|path| !path.as_os_str().is_empty())?;
    Some((version, home))
}

fn java_runtime_fingerprint(java_home: &Path) -> Option<String> {
    bounded_java_directory_fingerprint(
        java_home,
        b"gpui-android-java-runtime-v1\0",
        &mut DirectoryFingerprintBudget::default(),
    )
}

fn bounded_java_directory_fingerprint(
    root: &Path,
    domain: &[u8],
    budget: &mut DirectoryFingerprintBudget,
) -> Option<String> {
    let metadata = fs::symlink_metadata(root).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let canonical_root = fs::canonicalize(root).ok()?;
    let mut pending = vec![(
        PathBuf::new(),
        canonical_root.clone(),
        vec![canonical_root.clone()],
    )];
    let mut entries = Vec::new();
    let mut file_hashes = BTreeMap::<PathBuf, String>::new();
    while let Some((logical_directory, directory, ancestors)) = pending.pop() {
        let mut children = fs::read_dir(&directory)
            .ok()?
            .map(|entry| entry.ok())
            .collect::<Option<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let path = entry.path();
            let name = entry.file_name();
            let logical_path = logical_directory.join(name);
            let relative = logical_path.to_str()?.replace('\\', "/");
            let metadata = fs::symlink_metadata(&path).ok()?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                let resolved = fs::canonicalize(&path).ok()?;
                let resolved_metadata = fs::symlink_metadata(&resolved).ok()?;
                if !resolved_metadata.is_dir() && !resolved_metadata.is_file() {
                    return None;
                }
                let target = if resolved.starts_with(&canonical_root) {
                    "internal-link"
                } else if resolved_metadata.is_dir() {
                    "external-directory-link"
                } else if resolved_metadata.is_file() {
                    "external-file-link"
                } else {
                    return None;
                };
                budget.record(0)?;
                entries.push((relative.clone(), b'l', Some(target), None));
                if resolved_metadata.is_dir() {
                    if ancestors.contains(&resolved) {
                        return None;
                    }
                    let mut child_ancestors = ancestors.clone();
                    child_ancestors.push(resolved.clone());
                    pending.push((logical_path, resolved, child_ancestors));
                } else if resolved_metadata.is_file() {
                    let content_hash = if let Some(hash) = file_hashes.get(&resolved) {
                        hash.clone()
                    } else {
                        budget.record(resolved_metadata.len())?;
                        let hash = hash_file_contents(&resolved).ok()?;
                        file_hashes.insert(resolved, hash.clone());
                        hash
                    };
                    entries.push((relative, b'h', None, Some(content_hash)));
                }
            } else if metadata.is_dir() {
                budget.record(0)?;
                entries.push((relative, b'd', None, None));
                let canonical = fs::canonicalize(&path).ok()?;
                if ancestors.contains(&canonical) {
                    return None;
                }
                let mut child_ancestors = ancestors.clone();
                child_ancestors.push(canonical.clone());
                pending.push((logical_path, canonical, child_ancestors));
            } else if metadata.is_file() {
                let resolved = fs::canonicalize(&path).ok()?;
                let content_hash = if let Some(hash) = file_hashes.get(&resolved) {
                    hash.clone()
                } else {
                    budget.record(metadata.len())?;
                    let hash = hash_file_contents(&resolved).ok()?;
                    file_hashes.insert(resolved, hash.clone());
                    hash
                };
                entries.push((relative, b'f', None, Some(content_hash)));
            } else {
                return None;
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));

    let mut digest = Sha256::new();
    digest.update(domain);
    for (relative, kind, target, content_hash) in entries {
        digest.update([kind]);
        digest.update((relative.len() as u64).to_be_bytes());
        digest.update(relative.as_bytes());
        if let Some(target) = target {
            digest.update((target.len() as u64).to_be_bytes());
            digest.update(target.as_bytes());
        }
        if let Some(content_hash) = content_hash {
            digest.update(content_hash.as_bytes());
        }
    }
    Some(format!("{:x}", digest.finalize()))
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
            Some("Android SDK/NDK/cargo-ndk/JDK/Gradle distribution identity is unavailable or ambiguous; Android cache reuse is disabled".into()),
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
    let included_build_logic = android_included_build_logic(&root, native)?;

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
                disabled_reason: combine_cache_hit_disabled_reasons([
                    disabled_reason,
                    included_build_logic.then(|| {
                        "Android Gradle included build logic is outside the signing input closure; cache reuse is disabled".into()
                    }),
                ]),
                debug_keystore_identity,
                signing_identity: Some(identity),
            });
        }

        native
            .external_hashes
            .insert(ANDROID_SIGNING_EXTERNAL_HASH.into(), "unavailable".into());
        return Ok(AndroidCacheSigningPolicy {
            disabled_reason: combine_cache_hit_disabled_reasons([
                Some(
                    if supported {
                        "Android custom signing inputs are unavailable or outside the project root; cache reuse is disabled"
                    } else {
                        "Android custom signing configuration is not a supported local keystore.properties layout; cache reuse is disabled"
                    }
                    .into(),
                ),
                included_build_logic.then(|| {
                    "Android Gradle included build logic is outside the signing input closure; cache reuse is disabled".into()
                }),
            ]),
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
        disabled_reason: combine_cache_hit_disabled_reasons([
            disabled_reason,
            included_build_logic.then(|| {
                "Android Gradle included build logic is outside the signing input closure; cache reuse is disabled".into()
            }),
        ]),
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
    if release {
        // With no signing configuration or sensitive signing input, the
        // release variant is the deterministic unsigned APK produced by
        // Gradle. There is no keystore identity to add to the BuildKey.
        return (None, None);
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
    for (_, script) in android_signing_marker_sources(root, native)? {
        if contains_android_signing_marker(&script) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn contains_android_signing_marker(source: &str) -> bool {
    ANDROID_SIGNING_MARKERS
        .iter()
        .any(|marker| source.contains(marker))
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

/// Includes local Gradle plugin implementation sources when looking for
/// signing configuration. A convention plugin can mutate variant signing
/// without mentioning it in app/build.gradle(.kts), so cache eligibility must
/// not treat a marker in buildSrc/build-logic code as an ordinary debug build.
fn android_signing_marker_sources<'a>(
    root: &Path,
    native: &'a NativeInputs,
) -> Result<Vec<(&'a String, String)>> {
    let mut sources = android_signing_marker_scripts(root, native)?;
    for relative in native.files.keys().filter(|path| {
        path.starts_with("mobile/android/gradle/")
            && matches!(
                Path::new(path.as_str()).extension().and_then(OsStr::to_str),
                Some("kt" | "groovy" | "java")
            )
    }) {
        let source = fs::read_to_string(root.join(relative))
            .with_context(|| format!("reading Android Gradle plugin source {relative}"))?;
        sources.push((relative, source));
    }
    Ok(sources)
}

fn android_included_build_logic(root: &Path, native: &NativeInputs) -> Result<bool> {
    for (relative, script) in android_signing_marker_scripts(root, native)? {
        if (relative.ends_with("settings.gradle") || relative.ends_with("settings.gradle.kts"))
            && gradle_contains_identifier(&script, "includeBuild")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn supported_android_signing_config(root: &Path, native: &NativeInputs) -> Result<bool> {
    let sources = android_signing_marker_sources(root, native)?;
    let signing_sources = sources
        .iter()
        .filter(|(_, source)| contains_android_signing_marker(source))
        .collect::<Vec<_>>();
    let [(relative, script)] = signing_sources.as_slice() else {
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
) -> Result<(BuildKey, Option<String>)> {
    let manifest = Inputs::scan_stable(root, 2)?;
    let native = NativeInputs::scan(root)?;
    build_key_from_inputs(
        BuildKeyInputs {
            project_root: root,
            manifest: &manifest,
            source_manifest_hash: manifest.digest(),
            native: &native,
        },
        target_triple,
        release,
        toolchain_fingerprint,
        abi,
    )
}

fn build_key_from_inputs(
    inputs: BuildKeyInputs<'_>,
    target_triple: String,
    release: bool,
    toolchain_fingerprint: String,
    abi: Option<String>,
) -> Result<(BuildKey, Option<String>)> {
    let (relevant_environment, wrapper_disabled_reason) =
        relevant_build_environment_hash(inputs.project_root, &target_triple, |name| {
            env::var(name).ok()
        })?;
    let non_unicode_disabled_reason =
        non_unicode_build_tool_cache_disabled_reason(Some(&target_triple));
    let key = BuildKey::new(BuildKeyMaterial {
        source_manifest_hash: inputs.source_manifest_hash,
        cargo_lock_hash: inputs
            .manifest
            .sources
            .get("Cargo.lock")
            .cloned()
            .unwrap_or_else(|| "missing".into()),
        target_triple,
        profile: if release { "release" } else { "dev" }.into(),
        features: Vec::new(),
        abi,
        native_config_hash: inputs.native.digest(),
        toolchain_fingerprint,
        relevant_env_hash: relevant_environment,
        preview_registry_hash: "none".into(),
    })?;
    Ok((
        key,
        combine_cache_hit_disabled_reasons([wrapper_disabled_reason, non_unicode_disabled_reason]),
    ))
}

fn non_unicode_build_tool_cache_disabled_reason(target_triple: Option<&str>) -> Option<String> {
    if FINGERPRINTED_BUILD_TOOLS
        .iter()
        .any(|name| env::var_os(name).is_some() && env::var(name).is_err())
    {
        return Some(BUILD_TOOL_CACHE_DISABLED_REASON.to_owned());
    }
    target_triple.and_then(|targets| {
        targets
            .split('+')
            .map(normalize_target_environment_name)
            .any(|target| {
                [
                    format!("CARGO_TARGET_{target}_LINKER"),
                    format!("CARGO_TARGET_{target}_RUSTFLAGS"),
                ]
                .iter()
                .any(|name| env::var_os(name).is_some() && env::var(name).is_err())
            })
            .then(|| BUILD_TOOL_CACHE_DISABLED_REASON.to_owned())
    })
}

fn is_fingerprinted_build_tool(name: &str) -> bool {
    FINGERPRINTED_BUILD_TOOLS.contains(&name)
        || (name.starts_with("CARGO_TARGET_") && name.ends_with("_LINKER"))
}

fn relevant_build_environment_hash(
    project_root: &Path,
    target_triple: &str,
    mut read_environment: impl FnMut(&str) -> Option<String>,
) -> Result<(String, Option<String>)> {
    let path_environment = read_environment("PATH");
    let path_extensions = read_environment("PATHEXT");
    let mut tool_disabled_reason = None;
    let mut remaining = MAX_BUILD_TOOL_FINGERPRINT_BYTES;
    let mut names = RELEVANT_ENVIRONMENT
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    for target in target_triple
        .split('+')
        .map(normalize_target_environment_name)
    {
        names.push(format!("CARGO_TARGET_{target}_LINKER"));
        names.push(format!("CARGO_TARGET_{target}_RUSTFLAGS"));
    }

    hash_relevant_environment(names.into_iter().map(|name| {
        let value = read_environment(&name).unwrap_or_else(|| "<unset>".into());
        let value = if is_fingerprinted_build_tool(&name) && value != "<unset>" {
            match build_tool_command_fingerprint(
                project_root,
                &value,
                path_environment.as_deref(),
                path_extensions.as_deref(),
                &mut remaining,
            ) {
                Ok(fingerprint) => format!("{value}\ntool-fingerprint={fingerprint}"),
                Err(_) => {
                    tool_disabled_reason = Some(BUILD_TOOL_CACHE_DISABLED_REASON.into());
                    format!("{value}\ntool-fingerprint=unavailable")
                }
            }
        } else {
            value
        };
        (name, value)
    }))
    .map(|hash| (hash, tool_disabled_reason))
}

fn normalize_target_environment_name(target_triple: &str) -> String {
    target_triple
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn build_tool_command_fingerprint(
    project_root: &Path,
    value: &str,
    path_environment: Option<&str>,
    path_extensions: Option<&str>,
    remaining: &mut u64,
) -> Result<String> {
    if value.is_empty()
        || value.starts_with('-')
        || value.chars().any(|character| {
            character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"'
                        | '|'
                        | ';'
                        | '&'
                        | '>'
                        | '<'
                        | '$'
                        | '`'
                        | '('
                        | ')'
                        | '%'
                        | '!'
                        | '^'
                        | '*'
                        | '?'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '#'
                        | '~'
                )
        })
    {
        bail!("configured compiler/linker command is not a single executable path");
    }
    let path = resolve_build_tool_path(project_root, value, path_environment, path_extensions)
        .context("resolving configured compiler/linker tool")?;
    let canonical_path = fs::canonicalize(&path).context("canonicalizing configured build tool")?;
    let digest = fingerprint_build_tool_file(&canonical_path, remaining)?;
    Ok(format!("{}:{digest}", canonical_path.display()))
}

fn fingerprint_build_tool_file(path: &Path, remaining: &mut u64) -> Result<String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => bail!("configured build tool is not a regular file"),
        Err(error) => return Err(error).context("inspecting configured build tool"),
    };
    if !is_executable_build_tool(path, &metadata) {
        bail!("configured build tool is not executable");
    }
    if metadata.len() > *remaining {
        bail!("configured build tool exceeds the fingerprint budget");
    }
    let reserved_bytes = metadata.len();
    *remaining -= reserved_bytes;
    let mut file =
        open_build_tool_for_fingerprinting(path).context("opening configured build tool")?;
    let opened_identity =
        build_tool_file_identity(&file).context("identifying configured build tool")?;
    let opened_metadata = file.metadata().context("checking configured build tool")?;
    if !opened_metadata.is_file()
        || !same_build_tool_file(&metadata, &opened_metadata)
        || opened_metadata.len() != reserved_bytes
    {
        bail!("configured build tool changed while fingerprinting");
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.by_ref()
        .take(reserved_bytes)
        .read_to_end(&mut bytes)
        .context("reading configured build tool")?;
    let final_metadata = file
        .metadata()
        .context("rechecking configured build tool")?;
    let final_identity =
        build_tool_file_identity(&file).context("reidentifying configured build tool")?;
    if bytes.len() as u64 != opened_metadata.len()
        || !same_build_tool_file(&opened_metadata, &final_metadata)
        || opened_identity != final_identity
    {
        bail!("configured build tool changed while fingerprinting");
    }
    let path_file =
        open_build_tool_for_fingerprinting(path).context("reopening configured build tool")?;
    let path_metadata = path_file
        .metadata()
        .context("checking reopened configured build tool")?;
    let path_identity = build_tool_file_identity(&path_file)
        .context("identifying reopened configured build tool")?;
    if !same_build_tool_file(&opened_metadata, &path_metadata) || opened_identity != path_identity {
        bail!("configured build tool path changed while fingerprinting");
    }
    Ok(format!(
        "sha256={:x};bytes={}",
        Sha256::digest(bytes),
        opened_metadata.len()
    ))
}

#[cfg(windows)]
fn open_build_tool_for_fingerprinting(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
        .open(path)
}

#[cfg(not(windows))]
fn open_build_tool_for_fingerprinting(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

fn build_tool_file_identity(file: &fs::File) -> std::io::Result<Option<(u32, u64)>> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
        if success == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Some((
            information.dwVolumeSerialNumber,
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        )))
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(None)
    }
}

fn is_executable_build_tool(path: &Path, metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        let _ = path;
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        #[cfg(windows)]
        {
            let extension = path.extension().and_then(OsStr::to_str);
            let allowed_extensions =
                env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
            extension.is_some_and(|extension| {
                allowed_extensions
                    .split(';')
                    .map(|extension| extension.trim_start_matches('.'))
                    .any(|allowed| allowed.eq_ignore_ascii_case(extension))
            })
        }
        #[cfg(not(windows))]
        {
            true
        }
    }
}

fn same_build_tool_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    if left.len() != right.len()
        || left.modified().ok() != right.modified().ok()
        || left.created().ok() != right.created().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.mode() == right.mode()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(windows)]
    {
        let _ = (left, right);
        true
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}

fn resolve_build_tool_path(
    project_root: &Path,
    value: &str,
    path_environment: Option<&str>,
    path_extensions: Option<&str>,
) -> Option<PathBuf> {
    let candidate = Path::new(value);
    if candidate.is_absolute() || value.contains('/') || value.contains('\\') {
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            project_root.join(candidate)
        };
        return fs::metadata(&candidate)
            .is_ok_and(|metadata| metadata.is_file())
            .then_some(candidate);
    }

    let extensions = if cfg!(windows) && candidate.extension().is_none() {
        path_extensions
            .unwrap_or(".COM;.EXE;.BAT;.CMD")
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        vec![String::new()]
    };
    path_environment
        .map(OsStr::new)
        .into_iter()
        .flat_map(env::split_paths)
        .flat_map(|directory| {
            let directory = if directory.is_absolute() {
                directory
            } else {
                project_root.join(directory)
            };
            extensions
                .iter()
                .map(|extension| directory.join(format!("{}{extension}", candidate.display())))
                .collect::<Vec<_>>()
        })
        .find(|path| fs::metadata(path).is_ok_and(|metadata| metadata.is_file()))
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

    fn write_test_build_tool(path: &Path, contents: &[u8]) {
        fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

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
    fn cargo_build_flags_and_profile_overrides_change_the_build_environment_hash() {
        let names = [
            "CARGO_BUILD_RUSTFLAGS",
            "CARGO_PROFILE_DEV_CODEGEN_UNITS",
            "CARGO_PROFILE_DEV_DEBUG",
            "CARGO_PROFILE_DEV_DEBUG_ASSERTIONS",
            "CARGO_PROFILE_DEV_INCREMENTAL",
            "CARGO_PROFILE_DEV_OPT_LEVEL",
            "CARGO_PROFILE_DEV_OVERFLOW_CHECKS",
            "CARGO_PROFILE_DEV_PANIC",
            "CARGO_PROFILE_DEV_RPATH",
            "CARGO_PROFILE_DEV_SPLIT_DEBUGINFO",
            "CARGO_PROFILE_DEV_STRIP",
            "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
            "CARGO_PROFILE_RELEASE_DEBUG",
            "CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS",
            "CARGO_PROFILE_RELEASE_INCREMENTAL",
            "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_PROFILE_RELEASE_OPT_LEVEL",
            "CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS",
            "CARGO_PROFILE_RELEASE_PANIC",
            "CARGO_PROFILE_RELEASE_RPATH",
            "CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO",
            "CARGO_PROFILE_RELEASE_STRIP",
        ];
        for name in names {
            assert!(
                RELEVANT_ENVIRONMENT.contains(&name),
                "{name} must be allowlisted"
            );
        }

        let root = tempfile::tempdir().unwrap();
        let baseline =
            relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
                if name == "PATH" {
                    None
                } else {
                    Some("<unset>".into())
                }
            })
            .unwrap();

        for name in names {
            let changed =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |candidate| {
                    if candidate == name {
                        Some("changed-profile-input".into())
                    } else if candidate == "PATH" {
                        None
                    } else {
                        Some("<unset>".into())
                    }
                })
                .unwrap();
            assert_ne!(baseline.0, changed.0, "{name} must affect the build key");
        }
    }

    #[test]
    fn native_compiler_environment_changes_the_build_environment_hash() {
        for name in ["CC", "CXX", "AR", "CFLAGS", "CXXFLAGS"] {
            assert!(RELEVANT_ENVIRONMENT.contains(&name));
        }

        let root = tempfile::tempdir().unwrap();
        let baseline =
            relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
                if name == "PATH" {
                    None
                } else {
                    Some("<unset>".into())
                }
            })
            .unwrap();
        for name in ["CC", "CXX", "AR", "CFLAGS", "CXXFLAGS"] {
            let changed =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |candidate| {
                    if candidate == name {
                        Some("changed-native-input".into())
                    } else if candidate == "PATH" {
                        None
                    } else {
                        Some("<unset>".into())
                    }
                })
                .unwrap();
            assert_ne!(baseline.0, changed.0, "{name} must affect the build key");
        }
    }

    #[test]
    fn target_specific_linker_and_flags_and_workspace_wrapper_change_the_key() {
        assert!(RELEVANT_ENVIRONMENT.contains(&"RUSTC_WORKSPACE_WRAPPER"));

        let values = BTreeMap::from([
            (
                "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER".to_string(),
                "<unset>".to_string(),
            ),
            (
                "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS".to_string(),
                "<unset>".to_string(),
            ),
            ("RUSTC_WORKSPACE_WRAPPER".to_string(), "<unset>".to_string()),
        ]);
        let root = tempfile::tempdir().unwrap();
        let baseline =
            relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
                values.get(name).cloned()
            })
            .unwrap();

        for (name, changed_value) in [
            ("CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER", "custom-linker"),
            (
                "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS",
                "-Ctarget-feature=+neon",
            ),
            ("RUSTC_WORKSPACE_WRAPPER", "workspace-wrapper"),
        ] {
            let changed =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |candidate| {
                    if candidate == name {
                        Some(changed_value.to_string())
                    } else {
                        values.get(candidate).cloned()
                    }
                })
                .unwrap();
            assert_ne!(baseline, changed, "{name} must affect the build key");
        }

        let other_target =
            relevant_build_environment_hash(root.path(), "x86_64-apple-darwin", |name| {
                values.get(name).cloned()
            })
            .unwrap();
        assert_ne!(baseline, other_target);
    }

    #[test]
    fn wrapper_content_changes_the_build_environment_hash() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let wrapper = bin.join(if cfg!(windows) {
            "rust-wrapper.exe"
        } else {
            "rust-wrapper"
        });
        let wrapper_name = wrapper.file_name().unwrap().to_string_lossy().to_string();
        write_test_build_tool(&wrapper, b"wrapper-v1");
        let values = BTreeMap::from([
            ("PATH".to_string(), bin.display().to_string()),
            ("RUSTC_WRAPPER".to_string(), wrapper_name),
        ]);

        let first = relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
            values.get(name).cloned()
        })
        .unwrap();
        write_test_build_tool(&wrapper, b"wrapper-v2");
        let second = relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
            values.get(name).cloned()
        })
        .unwrap();

        assert_ne!(first.0, second.0);
        assert_eq!(first.1, None);
        assert_eq!(second.1, None);
    }

    #[test]
    fn rustc_selector_content_changes_the_build_environment_hash() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let rustc = bin.join(if cfg!(windows) {
            "selected-rustc.exe"
        } else {
            "selected-rustc"
        });
        let rustc_name = rustc.file_name().unwrap().to_string_lossy().to_string();
        write_test_build_tool(&rustc, b"rustc-v1");
        let values = BTreeMap::from([
            ("PATH".to_string(), bin.display().to_string()),
            ("RUSTC".to_string(), rustc_name),
        ]);

        let first = relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
            values.get(name).cloned()
        })
        .unwrap();
        write_test_build_tool(&rustc, b"rustc-v2");
        let second = relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
            values.get(name).cloned()
        })
        .unwrap();

        assert_ne!(first.0, second.0);
        assert_eq!(first.1, None);
        assert_eq!(second.1, None);
    }

    #[test]
    fn rust_toolchain_selector_environment_changes_the_build_environment_hash() {
        for name in ["RUSTC", "RUSTC_BOOTSTRAP", "RUSTUP_TOOLCHAIN"] {
            assert!(RELEVANT_ENVIRONMENT.contains(&name));
        }

        let root = tempfile::tempdir().unwrap();
        let values = BTreeMap::from([
            ("RUSTC_BOOTSTRAP".to_string(), "0".to_string()),
            ("RUSTUP_TOOLCHAIN".to_string(), "stable".to_string()),
        ]);
        let baseline =
            relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
                values.get(name).cloned()
            })
            .unwrap();

        for (name, changed_value) in [("RUSTC_BOOTSTRAP", "1"), ("RUSTUP_TOOLCHAIN", "nightly")] {
            let changed =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |candidate| {
                    if candidate == name {
                        Some(changed_value.to_string())
                    } else {
                        values.get(candidate).cloned()
                    }
                })
                .unwrap();
            assert_ne!(baseline.0, changed.0, "{name} must affect the build key");
        }
    }

    #[test]
    fn target_linker_content_changes_the_build_environment_hash() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let linker = bin.join(if cfg!(windows) {
            "test-linker.exe"
        } else {
            "test-linker"
        });
        write_test_build_tool(&linker, b"linker-v1");
        let target_name = "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER";
        let values = BTreeMap::from([
            ("PATH".to_string(), bin.display().to_string()),
            (
                target_name.to_string(),
                linker.file_name().unwrap().to_string_lossy().into(),
            ),
        ]);
        let target = "x86_64-linux-android";
        let first =
            relevant_build_environment_hash(root.path(), target, |name| values.get(name).cloned())
                .unwrap();
        write_test_build_tool(&linker, b"linker-v2");
        let second =
            relevant_build_environment_hash(root.path(), target, |name| values.get(name).cloned())
                .unwrap();

        assert_ne!(first.0, second.0);
        assert_eq!(first.1, None);
        assert_eq!(second.1, None);
    }

    #[test]
    fn native_compiler_and_archiver_content_changes_the_build_environment_hash() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        for (name, filename) in [("CC", "test-cc"), ("CXX", "test-cxx"), ("AR", "test-ar")] {
            let filename = if cfg!(windows) {
                format!("{filename}.exe")
            } else {
                filename.to_owned()
            };
            let tool = bin.join(&filename);
            write_test_build_tool(&tool, b"tool-v1");
            let values = BTreeMap::from([
                ("PATH".to_string(), bin.display().to_string()),
                (name.to_string(), filename),
            ]);
            let first =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |candidate| {
                    values.get(candidate).cloned()
                })
                .unwrap();
            write_test_build_tool(&tool, b"tool-v2");
            let second =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |candidate| {
                    values.get(candidate).cloned()
                })
                .unwrap();

            assert_ne!(first.0, second.0, "{name} content must affect the key");
            assert_eq!(first.1, None, "{name}");
            assert_eq!(second.1, None, "{name}");
        }
    }

    #[test]
    fn combined_target_triples_fingerprint_each_target_linker() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let linker = bin.join(if cfg!(windows) {
            "android-linker.exe"
        } else {
            "android-linker"
        });
        write_test_build_tool(&linker, b"linker-v1");
        let values = BTreeMap::from([
            ("PATH".to_string(), bin.display().to_string()),
            (
                "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER".to_string(),
                linker.file_name().unwrap().to_string_lossy().into(),
            ),
        ]);
        let first = relevant_build_environment_hash(
            root.path(),
            "aarch64-linux-android+x86_64-linux-android",
            |name| values.get(name).cloned(),
        )
        .unwrap();
        write_test_build_tool(&linker, b"linker-v2");
        let second = relevant_build_environment_hash(
            root.path(),
            "aarch64-linux-android+x86_64-linux-android",
            |name| values.get(name).cloned(),
        )
        .unwrap();

        assert_ne!(first.0, second.0);
        assert_eq!(first.1, None);
        assert_eq!(second.1, None);
    }

    #[test]
    fn unsafe_or_unavailable_build_tools_disable_only_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let executable = bin.join(if cfg!(windows) {
            "test-compiler.exe"
        } else {
            "test-compiler"
        });
        write_test_build_tool(&executable, b"compiler");
        let command = executable.display().to_string();

        for unsafe_command in [
            "missing-compiler".to_string(),
            format!("{command} --target=example"),
            format!("{command} | other-tool"),
        ] {
            let result =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
                    match name {
                        "CC" => Some(unsafe_command.clone()),
                        "PATH" => Some(bin.display().to_string()),
                        _ => None,
                    }
                })
                .expect("unfingerprintable tools must not block BuildKey creation");
            assert!(result.1.is_some(), "{unsafe_command}");
        }

        let rustc_result = relevant_build_environment_hash(
            root.path(),
            "aarch64-apple-darwin",
            |name| match name {
                "RUSTC" => Some("missing-rustc".into()),
                _ => None,
            },
        )
        .expect("an unavailable selected rustc must not block BuildKey creation");
        assert!(rustc_result.1.is_some());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let non_executable = bin.join("non-executable");
            fs::write(&non_executable, b"compiler").unwrap();
            fs::set_permissions(&non_executable, fs::Permissions::from_mode(0o644)).unwrap();
            let result =
                relevant_build_environment_hash(root.path(), "aarch64-apple-darwin", |name| {
                    match name {
                        "CC" => Some(non_executable.display().to_string()),
                        _ => None,
                    }
                })
                .expect("non-executable tools must not block BuildKey creation");
            assert!(result.1.is_some());
        }

        let oversized = root.path().join(if cfg!(windows) {
            "large-compiler.exe"
        } else {
            "large-compiler"
        });
        write_test_build_tool(&oversized, b"x");
        fs::File::options()
            .write(true)
            .open(&oversized)
            .unwrap()
            .set_len(MAX_BUILD_TOOL_FINGERPRINT_BYTES + 1)
            .unwrap();
        let result =
            relevant_build_environment_hash(
                root.path(),
                "aarch64-apple-darwin",
                |name| match name {
                    "CC" => Some(oversized.display().to_string()),
                    _ => None,
                },
            )
            .expect("over-budget tools must not block BuildKey creation");
        assert!(result.1.is_some());
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
    fn build_script_cache_bypass_follows_cargo_package_build_declaration() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(root.path().join("build.rs"), "fn main() {}\n").unwrap();

        assert!(
            local_build_script_cache_disabled_reason(root.path())
                .unwrap()
                .is_some()
        );

        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\nbuild=false\n",
        )
        .unwrap();
        assert!(
            local_build_script_cache_disabled_reason(root.path())
                .unwrap()
                .is_none()
        );

        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\nbuild='scripts/codegen.rs'\n",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("scripts")).unwrap();
        fs::write(root.path().join("scripts/codegen.rs"), "fn main() {}\n").unwrap();
        assert!(
            local_build_script_cache_disabled_reason(root.path())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn frozen_desktop_plan_keeps_cache_eligible_when_build_script_is_disabled() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\nedition='2024'\nbuild=false\n",
        )
        .unwrap();
        fs::write(root.path().join("src/lib.rs"), "pub fn value() {}\n").unwrap();
        fs::write(root.path().join("build.rs"), "fn main() {}\n").unwrap();
        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap();
        assert!(status.success());

        let plan = desktop_build_plan(root.path(), false).unwrap();
        assert!(plan.cache_hit_disabled_reason.is_none());
        assert!(plan.snapshot.root.join("build.rs").is_file());
    }

    #[test]
    fn external_path_package_build_script_disables_frozen_desktop_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        fs::create_dir_all(external.path().join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            format!(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nexternal = {{ path = {:?} }}\n",
                external.path()
            ),
        )
        .unwrap();
        fs::write(root.path().join("app/src/lib.rs"), "pub fn value() {}\n").unwrap();
        fs::write(
            external.path().join("Cargo.toml"),
            "[package]\nname = \"external\"\nversion = \"0.1.0\"\nedition = \"2024\"\nbuild = \"scripts/build-helper.rs\"\n",
        )
        .unwrap();
        fs::create_dir_all(external.path().join("scripts")).unwrap();
        fs::write(
            external.path().join("scripts/build-helper.rs"),
            "fn main() {}\n",
        )
        .unwrap();
        fs::write(external.path().join("src/lib.rs"), "pub fn external() {}\n").unwrap();

        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap();
        assert!(status.success());

        let plan = desktop_build_plan(root.path(), false).unwrap();
        assert!(
            plan.cache_hit_disabled_reason
                .as_deref()
                .is_some_and(|value| value.contains("build.rs hidden inputs"))
        );
        assert!(
            plan.snapshot
                .root
                .join("external/0000/scripts/build-helper.rs")
                .is_file()
        );
        assert!(
            !plan
                .cache_hit_disabled_reason
                .as_deref()
                .unwrap()
                .contains(external.path().to_str().unwrap())
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
    fn android_ndk_compiler_tool_fingerprint_tracks_active_tool_contents() {
        let ndk = tempfile::tempdir().unwrap();
        let host = android_ndk_host_tag_candidates().first().unwrap();
        let bin = ndk
            .path()
            .join("toolchains/llvm/prebuilt")
            .join(host)
            .join("bin");
        fs::create_dir_all(&bin).unwrap();
        for name in [
            "clang",
            "clang++",
            "lld",
            "ld.lld",
            "llvm-ar",
            "llvm-ranlib",
            "llvm-strip",
            "llvm-objcopy",
            "llvm-nm",
            "llvm-readelf",
            "aarch64-linux-android-clang",
            "x86_64-linux-android-clang++",
        ] {
            fs::write(bin.join(name), name.as_bytes()).unwrap();
        }

        let first = android_ndk_compiler_tool_fingerprint(ndk.path()).unwrap();
        assert!(first.contains("aarch64-linux-android-clang"));
        assert!(first.contains("x86_64-linux-android-clang++"));

        fs::write(bin.join("clang"), b"replacement compiler").unwrap();
        let second = android_ndk_compiler_tool_fingerprint(ndk.path()).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn android_ndk_compiler_resource_fingerprint_tracks_sysroot_and_headers() {
        let ndk = tempfile::tempdir().unwrap();
        let host = android_ndk_host_tag_candidates().first().unwrap();
        let host_root = ndk.path().join("toolchains/llvm/prebuilt").join(host);
        let sysroot_header = host_root.join("sysroot/usr/include/android/api.h");
        let clang_header = host_root.join("lib/clang/18/include/stdint.h");
        fs::create_dir_all(sysroot_header.parent().unwrap()).unwrap();
        fs::create_dir_all(clang_header.parent().unwrap()).unwrap();
        fs::write(&sysroot_header, b"Android API header v1").unwrap();
        fs::write(&clang_header, b"Clang builtin header v1").unwrap();

        let first = android_ndk_compiler_resource_fingerprint(&host_root).unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let copied_host = other_root.path().join("host");
        fs::create_dir_all(copied_host.join("sysroot/usr/include/android")).unwrap();
        fs::create_dir_all(copied_host.join("lib/clang/18/include")).unwrap();
        fs::write(
            copied_host.join("sysroot/usr/include/android/api.h"),
            b"Android API header v1",
        )
        .unwrap();
        fs::write(
            copied_host.join("lib/clang/18/include/stdint.h"),
            b"Clang builtin header v1",
        )
        .unwrap();
        assert_eq!(
            first,
            android_ndk_compiler_resource_fingerprint(&copied_host).unwrap()
        );

        fs::write(&sysroot_header, b"Android API header v2").unwrap();
        let second = android_ndk_compiler_resource_fingerprint(&host_root).unwrap();
        assert_ne!(first, second);

        fs::write(&sysroot_header, b"Android API header v1").unwrap();
        fs::write(&clang_header, b"Clang builtin header v2").unwrap();
        let third = android_ndk_compiler_resource_fingerprint(&host_root).unwrap();
        assert_ne!(first, third);
    }

    #[cfg(unix)]
    #[test]
    fn android_ndk_compiler_resource_fingerprint_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let ndk = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let host_root = ndk.path().join("host");
        fs::create_dir_all(&host_root).unwrap();
        fs::create_dir_all(outside.path().join("usr/include")).unwrap();
        fs::write(outside.path().join("usr/include/stdio.h"), b"header").unwrap();
        fs::create_dir_all(host_root.join("sysroot")).unwrap();
        symlink(outside.path().join("usr"), host_root.join("sysroot/usr")).unwrap();
        assert!(android_ndk_compiler_resource_fingerprint(&host_root).is_none());
    }

    fn write_sdk_project(root: &Path, compile_sdk: u32, build_tools: Option<&str>) {
        let manifest = root.join("mobile/android/gradle/app/build.gradle.kts");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let build_tools = build_tools
            .map(|version| format!("    buildToolsVersion = \"{version}\"\n"))
            .unwrap_or_default();
        fs::write(
            manifest,
            format!("android {{\n    compileSdk = {compile_sdk}\n{build_tools}}}\n"),
        )
        .unwrap();
    }

    fn write_android_verification_metadata(root: &Path, verification_metadata: &str) {
        let path = root.join(ANDROID_GRADLE_VERIFICATION_METADATA_RELATIVE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, verification_metadata).unwrap();
    }

    const STRICT_ANDROID_VERIFICATION_METADATA: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<verification-metadata>
  <configuration><verify-metadata>true</verify-metadata></configuration>
  <components>
    <component group="com.example" name="plugin" version="1.0">
      <artifact name="plugin-1.0.jar">
        <sha256 value="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" />
      </artifact>
    </component>
  </components>
</verification-metadata>
"#;

    fn write_sdk_packages(sdk: &Path, compile_sdk: u32, build_tools: &str, marker: &[u8]) {
        for (relative, contents) in [
            (
                format!("platforms/android-{compile_sdk}/source.properties"),
                format!("Pkg.Revision={compile_sdk}\n").into_bytes(),
            ),
            (
                format!("platforms/android-{compile_sdk}/android.jar"),
                marker.to_vec(),
            ),
            (
                format!("build-tools/{build_tools}/source.properties"),
                format!("Pkg.Revision={build_tools}\n").into_bytes(),
            ),
            (format!("build-tools/{build_tools}/aapt2"), marker.to_vec()),
        ] {
            let path = sdk.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    #[test]
    fn bounded_directory_fingerprint_rejects_trees_over_the_shared_budget() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("one"), b"1").unwrap();
        fs::write(root.path().join("two"), b"2").unwrap();
        let mut budget = DirectoryFingerprintBudget::new(1, 8);

        assert!(
            bounded_directory_fingerprint(root.path(), b"test-directory-v1\0", &mut budget)
                .is_none()
        );
    }

    #[test]
    fn android_java_runtime_fingerprint_tracks_content_and_ignores_install_root() {
        let first_root = tempfile::tempdir().unwrap();
        let first_java = first_root.path().join("Contents/Home");
        fs::create_dir_all(first_java.join("bin")).unwrap();
        fs::create_dir_all(first_java.join("lib/server")).unwrap();
        fs::write(first_java.join("release"), "JAVA_VERSION=\"21.0.1\"\n").unwrap();
        fs::write(first_java.join("bin/java"), b"java launcher").unwrap();
        fs::write(
            first_java.join("lib/server/libjvm.dylib"),
            b"JVM runtime v1",
        )
        .unwrap();

        let first = java_runtime_fingerprint(&first_java).unwrap();
        assert!(!first.contains(&first_root.path().display().to_string()));

        let second_root = tempfile::tempdir().unwrap();
        let second_java = second_root.path().join("Contents/Home");
        fs::create_dir_all(second_java.join("bin")).unwrap();
        fs::create_dir_all(second_java.join("lib/server")).unwrap();
        fs::write(second_java.join("release"), "JAVA_VERSION=\"21.0.1\"\n").unwrap();
        fs::write(second_java.join("bin/java"), b"java launcher").unwrap();
        fs::write(
            second_java.join("lib/server/libjvm.dylib"),
            b"JVM runtime v1",
        )
        .unwrap();
        assert_eq!(first, java_runtime_fingerprint(&second_java).unwrap());

        fs::write(
            second_java.join("lib/server/libjvm.dylib"),
            b"JVM runtime v2",
        )
        .unwrap();
        assert_ne!(first, java_runtime_fingerprint(&second_java).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn android_java_runtime_fingerprint_tracks_internal_symlink_targets() {
        use std::os::unix::fs::symlink;

        let java_root = tempfile::tempdir().unwrap();
        fs::create_dir_all(java_root.path().join("lib/modules")).unwrap();
        fs::create_dir_all(java_root.path().join("jmods")).unwrap();
        fs::write(java_root.path().join("lib/runtime.bin"), b"JVM runtime v1").unwrap();
        fs::write(java_root.path().join("jmods/java.base.jmod"), b"module v1").unwrap();
        symlink("runtime.bin", java_root.path().join("lib/runtime-link.bin")).unwrap();
        symlink(
            "../../jmods",
            java_root.path().join("lib/modules/jmods-alias"),
        )
        .unwrap();

        let first = java_runtime_fingerprint(java_root.path())
            .expect("internal JDK symlinks should be fingerprinted");
        fs::write(java_root.path().join("lib/runtime.bin"), b"JVM runtime v2").unwrap();
        assert_ne!(first, java_runtime_fingerprint(java_root.path()).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn android_java_runtime_fingerprint_allows_external_file_links_without_paths() {
        use std::os::unix::fs::symlink;

        let java_root = tempfile::tempdir().unwrap();
        fs::create_dir_all(java_root.path().join("lib")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("libjvm.dylib"), b"JVM runtime").unwrap();
        symlink(
            outside.path().join("libjvm.dylib"),
            java_root.path().join("lib/libjvm.dylib"),
        )
        .unwrap();

        let fingerprint = java_runtime_fingerprint(java_root.path()).unwrap();
        assert!(!fingerprint.contains(&outside.path().display().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn android_java_runtime_fingerprint_tracks_external_directory_links() {
        use std::os::unix::fs::symlink;

        let java_root = tempfile::tempdir().unwrap();
        fs::create_dir_all(java_root.path().join("lib")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("nested")).unwrap();
        fs::write(
            outside.path().join("nested/runtime.properties"),
            b"runtime-v1",
        )
        .unwrap();
        symlink(
            outside.path().join("nested"),
            java_root.path().join("lib/nested"),
        )
        .unwrap();

        let first = java_runtime_fingerprint(java_root.path()).unwrap();
        assert!(!first.contains(&outside.path().display().to_string()));
        fs::write(
            outside.path().join("nested/runtime.properties"),
            b"runtime-v2",
        )
        .unwrap();
        assert_ne!(first, java_runtime_fingerprint(java_root.path()).unwrap());
    }

    #[test]
    fn android_sdk_fingerprint_tracks_installed_platform_and_build_tools_revisions() {
        let sdk = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_sdk_project(project.path(), 34, None);
        write_sdk_packages(sdk.path(), 34, "34.0.0", b"SDK package v1");

        let first = android_sdk_package_fingerprint(sdk.path(), project.path()).unwrap();
        assert!(first.contains("platforms/android-34=3"));
        assert!(first.contains("build-tools/34.0.0=34.0.0"));
        assert!(!first.contains(&sdk.path().display().to_string()));

        write_sdk_packages(sdk.path(), 33, "33.0.0", b"inactive SDK package");
        assert_eq!(
            first,
            android_sdk_package_fingerprint(sdk.path(), project.path()).unwrap()
        );

        fs::write(
            sdk.path().join("build-tools/34.0.0/aapt2"),
            b"replacement build tool",
        )
        .unwrap();
        let replaced_content = android_sdk_package_fingerprint(sdk.path(), project.path()).unwrap();
        assert_ne!(first, replaced_content);

        fs::write(
            sdk.path().join("build-tools/34.0.0/source.properties"),
            "Pkg.Revision=34.0.1\n",
        )
        .unwrap();
        let second = android_sdk_package_fingerprint(sdk.path(), project.path()).unwrap();
        assert_ne!(replaced_content, second);

        let other_sdk = tempfile::tempdir().unwrap();
        write_sdk_packages(other_sdk.path(), 34, "34.0.0", b"SDK package v1");
        assert_eq!(
            first,
            android_sdk_package_fingerprint(other_sdk.path(), project.path()).unwrap()
        );
    }

    #[test]
    fn android_sdk_fingerprint_requires_a_literal_project_sdk_selection() {
        let sdk = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_sdk_packages(sdk.path(), 34, "34.0.0", b"SDK package");
        let manifest = project
            .path()
            .join("mobile/android/gradle/app/build.gradle.kts");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(
            manifest,
            "android { compileSdk = libs.versions.compileSdk.get() }\n",
        )
        .unwrap();

        assert!(android_sdk_package_fingerprint(sdk.path(), project.path()).is_none());
    }

    #[test]
    fn android_sdk_fingerprint_uses_explicit_build_tools_and_numeric_latest_default() {
        let sdk = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_sdk_packages(sdk.path(), 34, "34.0.0", b"selected");
        write_sdk_packages(sdk.path(), 34, "35.0.0", b"newer");

        write_sdk_project(project.path(), 34, None);
        let default_fingerprint = android_sdk_package_fingerprint(sdk.path(), project.path())
            .expect("latest installed numeric build-tools package should be selected");
        assert!(default_fingerprint.contains("build-tools/35.0.0="));
        assert!(!default_fingerprint.contains("build-tools/34.0.0="));

        write_sdk_project(project.path(), 34, Some("34.0.0"));
        let explicit_fingerprint = android_sdk_package_fingerprint(sdk.path(), project.path())
            .expect("explicit build-tools package should be selected");
        assert!(explicit_fingerprint.contains("build-tools/34.0.0="));
        assert!(!explicit_fingerprint.contains("build-tools/35.0.0="));

        fs::write(
            sdk.path().join("build-tools/35.0.0/aapt2"),
            b"changed but inactive",
        )
        .unwrap();
        assert_eq!(
            explicit_fingerprint,
            android_sdk_package_fingerprint(sdk.path(), project.path()).unwrap()
        );
    }

    #[test]
    fn android_sdk_fingerprint_rejects_incomplete_or_symlinked_packages() {
        let sdk = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_sdk_project(project.path(), 34, None);
        write_sdk_packages(sdk.path(), 34, "34.0.0", b"SDK package");
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
            assert!(android_sdk_package_fingerprint(linked_sdk.path(), project.path()).is_none());

            let linked_package_sdk = tempfile::tempdir().unwrap();
            let linked_platform = linked_package_sdk.path().join("platforms/android-34");
            let linked_build_tools = linked_package_sdk.path().join("build-tools/34.0.0");
            fs::create_dir_all(&linked_platform).unwrap();
            fs::create_dir_all(&linked_build_tools).unwrap();
            fs::write(
                linked_platform.join("source.properties"),
                "Pkg.Revision=3\n",
            )
            .unwrap();
            fs::write(
                linked_build_tools.join("source.properties"),
                "Pkg.Revision=34.0.0\n",
            )
            .unwrap();
            symlink(
                sdk.path().join("platforms/android-34/source.properties"),
                linked_platform.join("android.jar"),
            )
            .unwrap();
            assert!(
                android_sdk_package_fingerprint(linked_package_sdk.path(), project.path())
                    .is_none()
            );
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
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/android");
        let fingerprint = android_toolchain_fingerprint(&project_root);
        let sdk_root = consistent_environment_directory(&["ANDROID_HOME", "ANDROID_SDK_ROOT"]);
        let ndk_home = consistent_environment_directory(&["ANDROID_NDK_HOME"]);
        let gradle_user_home = android_gradle_user_home(
            &project_root,
            env::var_os("GRADLE_USER_HOME").as_deref(),
            android_default_user_home().as_deref(),
        );
        let gradle_distribution_available = gradle_user_home
            .as_deref()
            .is_some_and(test_gradle_distribution_is_installed);
        let ndk_alias_matches = env::var_os("NDK_HOME").is_none()
            || consistent_environment_directory(&["NDK_HOME"]).as_ref() == ndk_home.as_ref();
        let probes_available = command_version("cargo", &["ndk", "--version"], false).is_some()
            && java_runtime_details().is_some();
        let package_metadata_available = sdk_root
            .as_deref()
            .and_then(|root| android_sdk_package_fingerprint(root, &project_root))
            .is_some()
            && ndk_home
                .as_deref()
                .and_then(|home| android_package_revision(&home.join("source.properties")))
                .is_some();

        if env::var_os("GPUI_REQUIRE_ANDROID_TOOLCHAIN_FINGERPRINT").is_some() {
            assert!(
                fingerprint.is_some(),
                "Android toolchain fingerprint was required but SDK/NDK/JDK/cargo-ndk/Gradle distribution identity is unavailable"
            );
        } else if ndk_alias_matches
            && probes_available
            && package_metadata_available
            && gradle_distribution_available
        {
            assert!(fingerprint.is_some());
        }
    }

    fn test_gradle_distribution_is_installed(gradle_user_home: &Path) -> bool {
        let distributions = gradle_user_home.join("wrapper/dists");
        if !fs::symlink_metadata(&distributions)
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_dir())
        {
            return false;
        }
        let mut pending = vec![(distributions, 0_u8)];
        while let Some((directory, depth)) = pending.pop() {
            if depth == 3 {
                return true;
            }
            let Ok(entries) = fs::read_dir(directory) else {
                return false;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    return false;
                };
                let Ok(file_type) = entry.file_type() else {
                    return false;
                };
                if file_type.is_symlink() {
                    return false;
                }
                if file_type.is_dir() {
                    pending.push((entry.path(), depth + 1));
                }
            }
        }
        false
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
    fn android_gradle_global_user_configuration_disables_cache_reuse_without_reading_it() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("caches")).unwrap();
        fs::create_dir_all(home.path().join("wrapper/dists")).unwrap();
        assert!(!android_gradle_user_configuration_is_present(home.path()));

        for relative in ["gradle.properties", "init.gradle", "init.gradle.kts"] {
            let path = home.path().join(relative);
            fs::write(&path, "password=do-not-disclose\n").unwrap();
            assert!(android_gradle_user_configuration_is_present(home.path()));
            fs::remove_file(path).unwrap();
        }

        fs::create_dir(home.path().join("init.d")).unwrap();
        assert!(android_gradle_user_configuration_is_present(home.path()));

        let reason =
            android_gradle_global_configuration_cache_disabled_reason_for(Some(home.path()), false)
                .unwrap();
        assert_eq!(reason, ANDROID_GRADLE_GLOBAL_CONFIG_CACHE_DISABLED_REASON);
        assert!(!reason.contains("do-not-disclose"));
        assert!(!reason.contains(home.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn android_gradle_global_cache_gate_fails_closed_for_unknown_home_and_environment() {
        assert!(
            android_gradle_global_configuration_cache_disabled_reason_for(None, false).is_some()
        );

        let home = tempfile::tempdir().unwrap();
        assert!(
            android_gradle_global_configuration_cache_disabled_reason_for(Some(home.path()), true,)
                .is_some()
        );

        for name in [
            "GRADLE_HOME",
            "GRADLE_OPTS",
            "JAVA_OPTS",
            "JAVA_TOOL_OPTIONS",
            "JAVACMD",
            "JDK_JAVA_OPTIONS",
            "_JAVA_OPTIONS",
            "ORG_GRADLE_PROJECT_signingPassword",
        ] {
            assert!(has_unmodeled_android_gradle_environment([(
                OsString::from(name),
                OsString::from("secret-value"),
            )]));
        }
        assert!(!has_unmodeled_android_gradle_environment([(
            OsString::from("GRADLE_USER_HOME"),
            OsString::from("/private/gradle-home"),
        )]));
    }

    #[test]
    fn android_gradle_wrapper_distribution_init_scripts_disable_cache_reuse() {
        let home = tempfile::tempdir().unwrap();
        let init_dir = home
            .path()
            .join("wrapper/dists/gradle-9.4.1-bin/test-hash/gradle-9.4.1/init.d");
        fs::create_dir_all(&init_dir).unwrap();
        fs::write(init_dir.join("readme.txt"), "Add init scripts here.").unwrap();
        assert!(!android_gradle_user_configuration_is_present(home.path()));

        fs::write(
            init_dir.join("enterprise.init.gradle"),
            "// injected config\n",
        )
        .unwrap();
        assert!(android_gradle_user_configuration_is_present(home.path()));
    }

    #[test]
    fn android_gradle_distribution_fingerprint_tracks_installed_contents() {
        let home = tempfile::tempdir().unwrap();
        assert!(android_gradle_distribution_fingerprint_for(home.path()).is_none());

        let installation = home
            .path()
            .join("wrapper/dists/gradle-9.4.1-bin/opaque-hash/gradle-9.4.1");
        fs::create_dir_all(installation.join("lib")).unwrap();
        fs::write(installation.join("lib/gradle-core.jar"), b"official bytes").unwrap();
        let install_marker = installation
            .parent()
            .unwrap()
            .join("gradle-9.4.1-bin.zip.ok");
        fs::write(&install_marker, b"verified distribution").unwrap();
        let first = android_gradle_distribution_fingerprint_for(home.path()).unwrap();
        let identity = android_gradle_distribution_identity_for(home.path()).unwrap();
        assert_eq!(first.len(), 64);
        assert!(!first.contains(home.path().to_string_lossy().as_ref()));
        assert!(identity.matches_fingerprint());

        let user_config = home.path().join("gradle.properties");
        fs::write(&user_config, "org.gradle.jvmargs=-Xmx2g\n").unwrap();
        assert!(!identity.matches_fingerprint());
        fs::remove_file(user_config).unwrap();
        assert!(identity.matches_fingerprint());

        fs::write(installation.join("lib/gradle-core.jar"), b"modified bytes").unwrap();
        let modified = android_gradle_distribution_fingerprint_for(home.path()).unwrap();
        assert_ne!(first, modified);
        assert!(!identity.matches_fingerprint());

        fs::write(&install_marker, b"changed distribution marker").unwrap();
        let changed_marker = android_gradle_distribution_fingerprint_for(home.path()).unwrap();
        assert_ne!(modified, changed_marker);

        let inactive = home
            .path()
            .join("wrapper/dists/gradle-8.10-bin/other-hash/gradle-8.10");
        fs::create_dir_all(&inactive).unwrap();
        fs::write(inactive.join("gradle-launcher.jar"), b"other installation").unwrap();
        let with_inactive_distribution =
            android_gradle_distribution_fingerprint_for(home.path()).unwrap();
        assert_ne!(changed_marker, with_inactive_distribution);
    }

    #[test]
    fn android_gradle_distribution_fingerprint_fails_closed_on_budget_and_links() {
        let home = tempfile::tempdir().unwrap();
        let installation = home
            .path()
            .join("wrapper/dists/gradle-9.4.1-bin/opaque-hash/gradle-9.4.1");
        fs::create_dir_all(installation.join("lib")).unwrap();
        fs::write(installation.join("lib/gradle-core.jar"), b"contents").unwrap();

        let mut budget = DirectoryFingerprintBudget::new(2, 8);
        assert!(
            android_gradle_distribution_fingerprint_with_budget(home.path(), &mut budget).is_none()
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("gradle-core.jar", installation.join("lib/alias.jar"))
                .unwrap();
            assert!(android_gradle_distribution_fingerprint_for(home.path()).is_none());
        }
    }

    #[test]
    fn android_gradle_user_home_resolves_override_and_default() {
        let project = tempfile::tempdir().unwrap();
        let default_home = tempfile::tempdir().unwrap();
        let gradle_dir = project.path().join("mobile/android/gradle");

        assert_eq!(
            android_gradle_user_home(project.path(), Some(OsStr::new("custom-home")), None,),
            Some(gradle_dir.join("custom-home"))
        );
        assert_eq!(
            android_gradle_user_home(project.path(), None, Some(default_home.path().as_os_str()),),
            Some(default_home.path().join(".gradle"))
        );
        assert!(android_gradle_user_home(project.path(), Some(OsStr::new("")), None).is_none());
        assert!(
            android_gradle_relative_user_home_cache_disabled_reason(Some(OsStr::new("relative")))
                .is_some()
        );
        assert!(
            android_gradle_relative_user_home_cache_disabled_reason(Some(
                default_home.path().as_os_str()
            ))
            .is_none()
        );
        assert!(android_gradle_relative_user_home_cache_disabled_reason(None).is_none());
    }

    #[test]
    fn android_wrapper_distribution_checksum_is_required_for_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let gradle = root.path().join("mobile/android/gradle");
        fs::create_dir_all(gradle.join("gradle/wrapper")).unwrap();
        let properties = gradle.join("gradle/wrapper/gradle-wrapper.properties");

        fs::write(
            &properties,
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.4.1-bin.zip\n",
        )
        .unwrap();
        let mut native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_wrapper_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );

        let checksum = "2AB2958F2A1E51120C326CAD6F385153BB11EE93B3C216C5FCCEbfdfbb7ec6cb";
        let expected_checksum = checksum.to_ascii_lowercase();
        fs::write(
            &properties,
            format!(
                "distributionBase=GRADLE_USER_HOME\ndistributionPath=wrapper/dists\ndistributionUrl=https\\://services.gradle.org/distributions/gradle-9.4.1-bin.zip\ndistributionSha256Sum={checksum}\n"
            ),
        )
        .unwrap();
        native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_wrapper_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            android_gradle_wrapper_distribution_checksum(root.path(), &native)
                .unwrap()
                .as_deref(),
            Some(expected_checksum.as_str())
        );

        fs::write(
            &properties,
            format!(
                "distributionBase=PROJECT\ndistributionPath=wrapper/dists\ndistributionUrl=https\\://services.gradle.org/distributions/gradle-9.4.1-bin.zip\ndistributionSha256Sum={checksum}\n"
            ),
        )
        .unwrap();
        native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_wrapper_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );

        fs::write(
            &properties,
            format!(
                "distributionBase=GRADLE_USER_HOME\ndistributionPath=wrapper/dists\ndistribution\\u0050ath=custom\ndistributionSha256Sum={checksum}\n"
            ),
        )
        .unwrap();
        native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_wrapper_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );

        fs::write(
            &properties,
            format!("distributionSha256Sum={checksum}\ndistributionSha256Sum={checksum}\n"),
        )
        .unwrap();
        native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_wrapper_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );

        fs::write(&properties, "distributionSha256Sum=not-a-sha256\n").unwrap();
        native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_wrapper_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn android_gradle_dependency_verification_requires_strict_sha256_metadata() {
        let root = tempfile::tempdir().unwrap();
        let mut native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_verification_cache_disabled_reason(root.path(), &native)
                .is_some_and(|reason| reason.contains("metadata is missing or not strict"))
        );

        write_android_verification_metadata(root.path(), STRICT_ANDROID_VERIFICATION_METADATA);
        native = NativeInputs::scan(root.path()).unwrap();
        assert!(android_gradle_verification_cache_disabled_reason(root.path(), &native).is_none());

        for invalid in [
            STRICT_ANDROID_VERIFICATION_METADATA.replace("<verify-metadata>true", "<verify-metadata>false"),
            STRICT_ANDROID_VERIFICATION_METADATA.replace(
                "value=\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
                "value=\"bad\"",
            ),
            STRICT_ANDROID_VERIFICATION_METADATA.replace(
                "<components>",
                "<configuration><trusted-artifacts><trust group=\"*\" name=\"*\" /></trusted-artifacts></configuration><components>",
            ),
            "<verification-metadata><components>malformed</verification-metadata>".into(),
        ] {
            write_android_verification_metadata(root.path(), &invalid);
            let native = NativeInputs::scan(root.path()).unwrap();
            assert!(android_gradle_verification_cache_disabled_reason(root.path(), &native)
                .is_some());
        }

        let template_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("templates/android/gradle/gradle/verification-metadata.xml");
        let project = tempfile::tempdir().unwrap();
        let destination = project
            .path()
            .join(ANDROID_GRADLE_VERIFICATION_METADATA_RELATIVE);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(template_root, &destination).unwrap();
        let native = NativeInputs::scan(project.path()).unwrap();
        assert!(
            android_gradle_verification_cache_disabled_reason(project.path(), &native).is_none()
        );
    }

    #[test]
    fn android_wrapper_properties_are_bound_to_build_key() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.path().join("Cargo.lock"), "# lock\n").unwrap();
        let properties = root.path().join(ANDROID_GRADLE_WRAPPER_PROPERTIES_RELATIVE);
        fs::create_dir_all(properties.parent().unwrap()).unwrap();
        fs::write(
            &properties,
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.4.1-bin.zip\ndistributionSha256Sum=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        )
        .unwrap();
        let first = android_build_key(root.path(), false, &["arm64-v8a".into()]).unwrap();

        fs::write(
            &properties,
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.4.1-bin.zip\ndistributionSha256Sum=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        )
        .unwrap();
        let second = android_build_key(root.path(), false, &["arm64-v8a".into()]).unwrap();

        assert_ne!(first.key_hash(), second.key_hash());
    }

    #[test]
    fn android_dynamic_dependency_inputs_disable_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        let script = app.join("build.gradle.kts");
        fs::write(
            &script,
            "dependencies { implementation(\"com.example:fixed:1.2.3\") }\n",
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            "android { defaultConfig { ndk { abiFilters += gpuiAbis } } }\n",
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            "dependencies { implementation(\"com.example:dynamic:1.+\") }\n",
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        let reason = android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
            .unwrap()
            .unwrap();
        assert!(reason.contains("mobile/android/gradle/app/build.gradle.kts"));

        fs::write(
            &script,
            "configurations.all { resolutionStrategy.cacheChangingModulesFor(0, \"seconds\") }\n",
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );

        for changing_rule in [
            "metadataRule { details.changing=true }",
            "metadataRule { details.isChanging : true }",
            "metadataRule { details.setChanging(true) }",
        ] {
            fs::write(&script, changing_rule).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            assert!(
                android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                    .unwrap()
                    .is_some(),
                "changing module rule was missed: {changing_rule}"
            );
        }

        fs::write(
            &script,
            "dependencies { implementation(\"com.example:range:[1.0,2.0)\") }\n",
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );

        fs::write(&script, "plugins {}\n").unwrap();
        let catalog = root
            .path()
            .join("mobile/android/gradle/gradle/libs.versions.toml");
        fs::create_dir_all(catalog.parent().unwrap()).unwrap();
        fs::write(&catalog, "[versions]\nagp = \"9.1.+\"\n").unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );
        fs::remove_file(&catalog).unwrap();

        let plugin_sources = [
            (
                "mobile/android/gradle/buildSrc/src/main/kotlin/DynamicDepsPlugin.kt",
                "dependencies.add(\"implementation\", \"com.example:dynamic:1.+\")\n",
            ),
            (
                "mobile/android/gradle/build-logic/src/main/groovy/DynamicDepsPlugin.groovy",
                "dependencies { implementation 'com.example:dynamic:1.+' }\n",
            ),
            (
                "mobile/android/gradle/buildSrc/src/main/java/DynamicDepsPlugin.java",
                "getProject().getDependencies().add(\"implementation\", \"com.example:dynamic:1.+\");\n",
            ),
        ];
        for (relative, source) in plugin_sources {
            let plugin_source = root.path().join(relative);
            fs::create_dir_all(plugin_source.parent().unwrap()).unwrap();
            fs::write(&plugin_source, source).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            let reason = android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .unwrap_or_else(|| panic!("dynamic plugin dependency was missed in {relative}"));
            assert!(reason.contains(relative));
            fs::remove_file(plugin_source).unwrap();
        }

        let app_source = root
            .path()
            .join("mobile/android/gradle/app/src/main/java/GpuiAudio.java");
        fs::create_dir_all(app_source.parent().unwrap()).unwrap();
        fs::write(
            &app_source,
            "String message = \"API 23+\" + Build.VERSION.SDK_INT;\n",
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_dynamic_dependency_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn local_android_gradle_build_logic_disables_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        let build_src = root
            .path()
            .join("mobile/android/gradle/buildSrc/src/main/kotlin");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&build_src).unwrap();
        fs::write(app.join("build.gradle.kts"), "plugins {}\n").unwrap();
        fs::write(
            build_src.join("ConventionPlugin.kt"),
            "class ConventionPlugin { fun apply() = readEnvironment() }\n",
        )
        .unwrap();

        let native = NativeInputs::scan(root.path()).unwrap();
        assert_eq!(
            android_gradle_local_build_logic_cache_disabled_reason(&native).as_deref(),
            Some(ANDROID_GRADLE_LOCAL_BUILD_LOGIC_CACHE_DISABLED_REASON)
        );

        fs::remove_dir_all(root.path().join("mobile/android/gradle/buildSrc")).unwrap();
        let ordinary_app_source = root
            .path()
            .join("mobile/android/gradle/app/src/main/kotlin/Ordinary.kt");
        fs::create_dir_all(ordinary_app_source.parent().unwrap()).unwrap();
        fs::write(&ordinary_app_source, "class Ordinary\n").unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(android_gradle_local_build_logic_cache_disabled_reason(&native).is_none());
    }

    #[test]
    fn ordinary_android_gradle_app_script_io_disables_only_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        let script = app.join("build.gradle.kts");

        fs::write(
            &script,
            r#"
                val gpuiAbis = providers.gradleProperty("gpui.abis")
                val buildDir = providers.gradleProperty("gpui.buildDir")
                val jniDir = providers.gradleProperty("gpui.jniLibsDir")
                val ndkHome = providers.environmentVariable("ANDROID_NDK_HOME")
                val ndkDirectory = file(ndkHome.get())
                val properties = java.util.Properties().apply {
                    ndkDirectory.resolve("source.properties").inputStream().use { load(it) }
                }
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            r#"
                val gpuiJniLibsDir = providers.gradleProperty("gpui.jniLibsDir")
                    .orElse("src/main/jniLibs").get()
                android { sourceSets { getByName("main") { jniLibs.srcDirs(gpuiJniLibsDir) } } }
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            r#"
                val configuredBuildDir = providers.gradleProperty("gpui.buildDir").get()
                val managedBuildDir = file(configuredBuildDir)
                val ndkHome = providers.environmentVariable("ANDROID_NDK_HOME").get()
                val ndkDirectory = file(ndkHome)
                ndkDirectory.resolve("source.properties").inputStream()
                val unknown = file("config.json")
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some()
        );

        for source in [
            "val value = System.getenv(\"CUSTOM_INPUT\")",
            "val value = providers.gradleProperty(\"custom.input\")",
            "val value = file(\"config.json\")",
            "val value = files(\"config.json\")",
            "val value = file 'config.json'",
            "val value = files 'config.json'",
            "android { sourceSets { main { java.srcDirs files 'src/generated/java' } } }",
            "val value = File(\"config.json\")",
            "val value = java.io.File(\"config.json\")",
            "val value = java.nio.file.Paths.get(\"config.json\")",
            "val value = Paths.get(configPath)",
            "val value = FileSystems.getDefault().getPath(\"config.json\")",
            "val value = java.nio.file.FileSystems.getDefault().getPath(configPath)",
            "val value = java.nio.file.Path.of(\"config.json\")",
            "val value = Path.of(configPath)",
            "val value = File(\"config.json\").getCanonicalFile()",
            "val value = File(\"config.json\").getCanonicalPath()",
            "val value = File(\"config.json\").toPath()",
            "val value = File(\"config.json\").getAbsolutePath()",
            "val channel = AsynchronousFileChannel.open(path, StandardOpenOption.READ)",
            "val channel: java.nio.channels.AsynchronousFileChannel = AsynchronousFileChannel.open(path, options)",
            "val channel: SeekableByteChannel = customChannel",
            "val provider: java.nio.file.spi.FileSystemProvider = path.fileSystem.provider()",
            "val entries: DirectoryStream<Path> = provider.newDirectoryStream(path, filter)",
            "val secureEntries: SecureDirectoryStream<Path> = secureDirectoryStream",
            "Files.walkFileTree(root, FileVisitOption.FOLLOW_LINKS, visitor)",
            "val visitor = object : SimpleFileVisitor<Path>() { override fun visitFile(path: Path, attrs: BasicFileAttributes): FileVisitResult = FileVisitResult.CONTINUE }",
            "val visitor: FileVisitor<Path> = customVisitor",
            "val matcher: PathMatcher = path.fileSystem.getPathMatcher(\"glob:*.json\")",
            "val matcher: java.nio.file.PathMatcher = fileSystem.getPathMatcher(pattern)",
            "val value = File(\"config.json\").canRead()",
            "val value = path.toFile()",
            "val value = java.nio.file.Path.of(configPath).toFile()",
            "val watcher: java.nio.file.WatchService = fileSystem.newWatchService()",
            "val watcher: WatchService = FileSystems.getDefault().newWatchService()",
            "val key: WatchKey = directory.register(watcher, StandardWatchEventKinds.ENTRY_MODIFY)",
            "val changes: Iterable<WatchEvent<*>> = key.pollEvents()",
            "val value = File(\"config.json\").canWrite()",
            "val value = File(\"config.json\").canExecute()",
            "val value = File(\"config.json\").isHidden()",
            "val value = File(\"config.json\").length()",
            "val value = File(\"config.json\").lastModified()",
            "val value = File(\"config.json\").getFreeSpace()",
            "val value = File(\"config.json\").getTotalSpace()",
            "val value = File(\"config.json\").getUsableSpace()",
            "val value = File(\"config\").list()",
            "val value = File(\"config\").listFiles()",
            "File(\"config.json\").createNewFile()",
            "File(\"generated\").mkdir()",
            "File(\"generated\").mkdirs()",
            "File(\"config.json\").delete()",
            "File(\"config.json\").deleteOnExit()",
            "File(\"config.json\").renameTo(File(\"moved.json\"))",
            "File(\"config.json\").setLastModified(timestamp)",
            "File(\"config.json\").setReadOnly()",
            "File(\"config.json\").setWritable(true)",
            "File(\"config.json\").setReadable(true)",
            "File(\"config.json\").setExecutable(true)",
            "val value = Files.exists(path)",
            "val value = Files.isReadable(path)",
            "val value = java.nio.file.Files.isWritable(path)",
            "val value = Files.isExecutable(path)",
            "val value = Files.isHidden(path)",
            "val value = Files.isSameFile(path, otherPath)",
            "val value = Files.size(path)",
            "val value = Files.getLastModifiedTime(path)",
            "val value = Files.setLastModifiedTime(path, timestamp)",
            "val value = Files.getFileStore(path)",
            "val value = Files.getAttribute(path, \"basic:size\")",
            "val value = Files.getPosixFilePermissions(path)",
            "Files.setPosixFilePermissions(path, permissions)",
            "val value = Files.readSymbolicLink(path)",
            "Files.createSymbolicLink(link, target)",
            "Files.createLink(link, target)",
            "Files.createDirectory(path)",
            "Files.createDirectories(path)",
            "Files.createTempFile(path, \"prefix\", \"suffix\")",
            "Files.createTempDirectory(path, \"prefix\")",
            "Files.delete(path)",
            "Files.deleteIfExists(path)",
            "Files.move(source, target)",
            "val value = Files.newByteChannel(path)",
            "val value = Files.newDirectoryStream(path)",
            "val value = Files.readAllBytes(path)",
            "val value = Files.readAllLines(path)",
            "val value = Files.lines(path)",
            "Files.write(path, bytes)",
            "val value = Files.find(path, 1, matcher)",
            "val value = Files.probeContentType(path)",
            "val value = path.toRealPath()",
            "val value = FileSystems.getDefault().getFileStores()",
            "val value = FileSystems.getDefault().getRootDirectories()",
            "val value = fileStore.getAttribute(\"volume:vsn\")",
            "val value = ZipFile(\"config.zip\")",
            "val value = java.util.zip.ZipFile(archivePath)",
            "val value = JarFile(\"plugin.jar\")",
            "val value = java.util.jar.JarFile(jarPath)",
            "val value = ZipInputStream(input)",
            "val value = JarInputStream(input)",
            "val value = ZipOutputStream(output)",
            "val value = JarOutputStream(output)",
            "val value = ZipFileSystemProvider()",
            "val value = FileSystems.newFileSystem(archivePath, loader)",
            "val value = Scanner(File(configPath))",
            "val value = java.util.Scanner(Path.of(configPath))",
            "val value = Scanner(configPath)",
            "val value = PrintStream(File(outputPath))",
            "val value = java.io.PrintStream(outputPath)",
            "val value = PrintWriter(Path.of(outputPath))",
            "val value = java.io.PrintWriter(outputPath)",
            "val value = javaClass.getResource(\"/config.properties\")",
            "val value = javaClass.getResourceAsStream(\"/config.properties\")",
            "val value = ClassLoader.getSystemResource(\"config.properties\")",
            "val value = ServiceLoader.load(MyProvider::class.java)",
            "val value = Class.forName(providerClassName)",
            "android { sourceSets { getByName(\"main\") { java.srcDir(\"src/generated/java\") } } }",
            "android { sourceSets { getByName(\"main\") { java.srcDirs(\"src/generated/java\", \"src/shared/java\") } } }",
            "android { sourceSets { getByName(\"main\") { java.setSrcDirs(listOf(\"src/generated/java\")) } } }",
            "android { sourceSets { getByName(\"main\") { java.srcDirs = listOf(\"src/generated/java\") } } }",
            "android { sourceSets { main { java.srcDir 'src/generated/java' } } }",
            "android { sourceSets { main { resources.srcDirs += 'src/generated/resources' } } }",
            "val value = file(\"config.json\").readText()",
            "val value = providers.fileContents(\"config.properties\")",
            "val value = layout.projectDirectory.file(\"config.properties\")",
            "val value = projectDir.resolve(\"config.properties\")",
            "val value = rootDir.resolve(\"config.properties\")",
            "val value = gradleLocalProperties(rootDir)",
            "val value = resources.text.fromArchiveEntry(\"config.zip\", \"entry\")",
            "val value = layout.files.from(\"config.json\")",
            "val value = fileCollection.from(configPath)",
            "android { sourceSets { main { java.from(\"src/generated/java\") } } }",
            "val value = fileCollection.from 'config.json'",
            "android { sourceSets { main { java.from 'src/generated/java' } } }",
            "val value = provider.getAsFile()",
            "val value = providers.of(MyValueSource::class) {}",
            "val value = providers.provider { \"computed\" }",
            "abstract class Inputs : ValueSource<String, ValueSource.Parameters>",
            "val value = URL(\"https://example.test/config.json\")",
            "tasks.register(\"probe\") { exec { commandLine(\"tool\") } }",
            "repositories { maven { url = uri(\"https://example.test/maven\") } }",
        ] {
            fs::write(&script, source).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            let reason = android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .unwrap_or_else(|| panic!("unmodeled Gradle I/O was missed: {source}"));
            assert!(reason.contains(ANDROID_GRADLE_APP_SCRIPT_IO_CACHE_DISABLED_REASON));
            assert!(reason.contains("mobile/android/gradle/app/build.gradle.kts"));
        }

        fs::write(
            &script,
            r#"
                // srcDir("comment-only")
                /* sourceSets { main { java.setSrcDirs(listOf("comment-only")) } } */
                val example = "srcDirs = listOf(\"string-only\")"
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            r#"
                // System.getenv("CUSTOM_INPUT") and URL("https://example.test")
                // file 'comment-only'
                // File("comment-only") and Paths.get("comment-only")
                // File("comment-only").getCanonicalPath() and FileSystems.getDefault().getPath("comment-only")
                // path.toFile() and Path.of("comment-only").toFile()
                // FileSystem.newWatchService(), WatchService, WatchKey, WatchEvent, Watchable.register(), StandardWatchEventKinds.ENTRY_MODIFY
                // AsynchronousFileChannel.open(path, options) and SeekableByteChannel
                // FileSystemProvider, DirectoryStream, SecureDirectoryStream
                // Files.walkFileTree(root, visitor), SimpleFileVisitor, FileVisitor, FileVisitResult, FileVisitOption, BasicFileAttributes
                // PathMatcher and FileSystem.getPathMatcher("comment-only")
                // File("comment-only").listFiles() and File("comment-only").lastModified()
                // File("comment-only").delete() and File("comment-only").canRead()
                // Files.isReadable(path) and Files.getLastModifiedTime(path) and Files.newDirectoryStream(path)
                // Files.createDirectories(path) and Files.deleteIfExists(path) and path.toRealPath()
                // ZipFile(\"comment-only.zip\") and FileSystems.newFileSystem(path, loader)
                // Scanner(File(\"comment-only.txt\")) and PrintWriter(\"comment-only.txt\")
                // javaClass.getResource("comment-only") and ServiceLoader.load(Provider::class.java)
                // fileCollection.from("comment-only")
                val from = "ordinary-variable"
                val text = "file(\"config.json\").readText() file 'string-only' File(\"string-only\") Path.of(\"string-only\") Class.forName(\"string-only\") from(\"string-only\") File(\"string-only\").toPath() File(\"string-only\").listFiles() File(\"string-only\").delete() Files.isReadable(path) Files.newDirectoryStream(path) Files.move(source, target) path.toRealPath() path.toFile() FileSystem.newWatchService() WatchService WatchKey WatchEvent Watchable.register() StandardWatchEventKinds.ENTRY_MODIFY AsynchronousFileChannel.open(path, options) SeekableByteChannel FileSystemProvider DirectoryStream SecureDirectoryStream Files.walkFileTree(root, visitor) SimpleFileVisitor FileVisitor FileVisitResult FileVisitOption BasicFileAttributes PathMatcher FileSystem.getPathMatcher(\"string-only\") ZipFile(\"string-only.zip\") JarFile(\"string-only.jar\") Scanner(File(\"string-only.txt\")) PrintStream(\"string-only.txt\") PrintWriter(\"string-only.txt\")"
            "#,
        )
        .unwrap();
        let ordinary_source = root
            .path()
            .join("mobile/android/gradle/app/src/main/kotlin/Ordinary.kt");
        fs::create_dir_all(ordinary_source.parent().unwrap()).unwrap();
        fs::write(
            &ordinary_source,
            "class Ordinary { val value = System.getenv(\"CUSTOM_INPUT\"); val provider: FileSystemProvider? = null; val stream: DirectoryStream<*>? = null; val visitor: FileVisitor<*>? = null }\n",
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            r#"
                tasks.register("clean", Delete::class) {
                    delete(rootProject.layout.buildDirectory)
                }
                val plainRegistration = tasks.register("probe")
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            r#"
                val ndkHome = providers.environmentVariable("ANDROID_NDK_HOME")
                val keystoreProperties = java.util.Properties()
                keystoreProperties.load(FileInputStream("keystore.properties"))
                val ndkDirectory = file("ndk")
                ndkDirectory.resolve("source.properties").inputStream()
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );

        fs::write(
            &script,
            r#"
                tasks.register("clean", Delete::class) {
                    delete(rootProject.layout.buildDirectory)
                }
            "#,
        )
        .unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn managed_android_gradle_reads_do_not_allow_other_stream_inputs() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        let script = app.join("build.gradle.kts");
        let managed = r#"
            val ndkHome = providers.environmentVariable("ANDROID_NDK_HOME")
            val ndkDirectory = file(ndkHome.get())
            ndkDirectory.resolve("source.properties").inputStream().use { load(it) }
            val keystoreProperties = java.util.Properties()
            keystoreProperties.load(FileInputStream("keystore.properties"))
        "#;

        for extra in [
            r#"ndkDirectory.resolve("extra.properties").inputStream()"#,
            r#"otherDirectory.resolve("source.properties").inputStream()"#,
            r#"other.ndkDirectory.resolve("source.properties").inputStream()"#,
            r#"ndkDirectory.resolve("source.properties" + suffix).inputStream()"#,
            r#"ndkDirectory.resolve(configPath).inputStream()"#,
            r#"FileInputStream("extra.properties")"#,
            r#"java.io.FileInputStream(configPath)"#,
            r#"FileInputStream("keystore.properties" + suffix)"#,
            r#"val reader = otherDirectory::inputStream"#,
            "import java.io.FileInputStream as ExternalStream\nExternalStream(configPath)",
        ] {
            for source in [format!("{managed}\n{extra}"), format!("{extra}\n{managed}")] {
                fs::write(&script, &source).unwrap();
                let native = NativeInputs::scan(root.path()).unwrap();
                let reason =
                    android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                        .unwrap()
                        .unwrap_or_else(|| panic!("managed read allowed another input: {extra}"));
                assert!(reason.contains(ANDROID_GRADLE_APP_SCRIPT_IO_CACHE_DISABLED_REASON));
            }
        }

        for extra in [
            "",
            "import java.io.FileInputStream\n",
            r#"ndkDirectory /* receiver */ . resolve ( "source.properties" ) /* stream */ . inputStream ( )"#,
            r#"FileInputStream(/* source */ "keystore.properties" /* end */)"#,
            r#"java.io.FileInputStream('keystore.properties')"#,
            r#"// ndkDirectory.resolve("extra.properties").inputStream()
                /* FileInputStream("extra.properties") */
                val text = "FileInputStream(\"example\") otherDirectory.inputStream()""#,
        ] {
            fs::write(&script, format!("{extra}\n{managed}")).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            assert!(
                android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                    .unwrap()
                    .is_none(),
                "managed or inert stream input disabled cache reuse: {extra}"
            );
        }
    }

    #[test]
    fn android_gradle_interpolated_expressions_disable_only_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();

        for filename in ["build.gradle.kts", "build.gradle"] {
            let script = app.join(filename);
            for source in [
                r#"println("env=${System.getenv("GPUI_INTERPOLATED_INPUT")}")"#,
                r#"println("file=${File("config.json").readText()}")"#,
                r#"println("provider=${providers.environmentVariable("GPUI_INTERPOLATED_INPUT").get()}")"#,
                r#"println("""raw=${System.getenv("GPUI_INTERPOLATED_INPUT")}""")"#,
                r#"println("nested=${if (true) "${customInput()}" else ""}")"#,
                r#"println("computed=${customInput()}")"#,
                r#"println("backslash=\\${customInput()}")"#,
                r#"println("literal=\${example} active=${customInput()}")"#,
            ] {
                fs::write(&script, source).unwrap();
                let native = NativeInputs::scan(root.path()).unwrap();
                let reason =
                    android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                        .unwrap()
                        .unwrap_or_else(|| {
                            panic!("interpolation was missed in {filename}: {source}")
                        });
                assert!(reason.contains(ANDROID_GRADLE_APP_SCRIPT_IO_CACHE_DISABLED_REASON));
                assert!(reason.contains(filename));
                assert!(!reason.contains("GPUI_INTERPOLATED_INPUT"));
            }

            for source in [
                r#"println("plain System.getenv(\"EXAMPLE\")")"#,
                r#"println("literal=\${System.getenv(\"EXAMPLE\")}")"#,
                r#"println("ABI: $gpuiAbis")"#,
                r#"// println("${customInput()}")
                    /* outer /* println("${customInput()}") */ comment */
                    println("plain")"#,
            ] {
                fs::write(&script, source).unwrap();
                let native = NativeInputs::scan(root.path()).unwrap();
                assert!(
                    android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                        .unwrap()
                        .is_none(),
                    "literal text disabled cache reuse in {filename}: {source}"
                );
            }
            fs::remove_file(script).unwrap();
        }

        let script = app.join("build.gradle.kts");
        fs::write(&script, r#"println("""raw=\${customInput()}""")"#).unwrap();
        let native = NativeInputs::scan(root.path()).unwrap();
        assert!(
            android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                .unwrap()
                .is_some(),
            "backslashes do not escape interpolation in Kotlin raw strings"
        );
        fs::remove_file(script).unwrap();

        let script = app.join("build.gradle");
        for source in [
            r#"println('literal ${System.getenv("EXAMPLE")}')"#,
            r#"println('''literal ${System.getenv("EXAMPLE")}''')"#,
        ] {
            fs::write(&script, source).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            assert!(
                android_gradle_app_script_io_cache_disabled_reason(root.path(), &native)
                    .unwrap()
                    .is_none(),
                "Groovy single-quoted text disabled cache reuse: {source}"
            );
        }
    }

    #[test]
    fn unknown_android_gradle_plugins_disable_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let gradle = root.path().join("mobile/android/gradle");
        let app = gradle.join("app");
        fs::create_dir_all(&app).unwrap();
        let script = app.join("build.gradle.kts");

        for source in [
            "plugins { id(\"com.android.application\") }",
            "plugins { id(\"com.example.convention\") }",
            "plugins { alias(libs.plugins.android.application) }",
            "plugins { kotlin(\"android\") }",
            "apply(plugin = \"com.example.convention\")",
            "apply plugin: \"com.example.convention\"",
        ] {
            fs::write(&script, source).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            let reason =
                android_gradle_unknown_plugin_cache_disabled_reason(root.path(), &native).unwrap();
            if source.contains("com.android.application") {
                assert!(reason.is_none(), "known AGP plugin was rejected: {source}");
            } else {
                assert!(
                    reason
                        .as_deref()
                        .is_some_and(|value| value
                            .contains(ANDROID_GRADLE_UNKNOWN_PLUGIN_CACHE_DISABLED_REASON)),
                    "unknown plugin was not rejected: {source}"
                );
            }
        }
    }

    #[test]
    fn unknown_android_gradle_buildscript_classpath_disables_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        let script = app.join("build.gradle.kts");

        for (source, cache_reuse_is_allowed) in [
            (
                "buildscript { dependencies { classpath(\"com.android.tools.build:gradle:9.1.0\") } }",
                true,
            ),
            (
                "buildscript { dependencies { classpath(\"com.example.convention:plugin:1.0\") } }",
                false,
            ),
            (
                "buildscript { dependencies { classpath(libs.plugins.convention) } }",
                false,
            ),
            (
                "buildscript { dependencies { classpath(\"com.android.tools.build:gradle:$agpVersion\") } }",
                false,
            ),
            (
                "buildscript { dependencies { add(\"classpath\", \"com.android.tools.build:gradle:9.1.0\") } }",
                true,
            ),
            (
                "buildscript { dependencies { add(\"classpath\", \"com.example.convention:plugin:1.0\") } }",
                false,
            ),
            (
                "buildscript { dependencies { add(\"classpath\", libs.plugins.convention) } }",
                false,
            ),
        ] {
            fs::write(&script, source).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            let reason =
                android_gradle_unknown_plugin_cache_disabled_reason(root.path(), &native).unwrap();
            if cache_reuse_is_allowed {
                assert!(
                    reason.is_none(),
                    "known AGP classpath was rejected: {source}"
                );
            } else {
                assert!(
                    reason
                        .as_deref()
                        .is_some_and(|value| value
                            .contains(ANDROID_GRADLE_UNKNOWN_PLUGIN_CACHE_DISABLED_REASON)),
                    "unknown buildscript classpath was not rejected: {source}"
                );
            }
        }
    }

    #[test]
    fn custom_android_gradle_repositories_disable_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let gradle = root.path().join("mobile/android/gradle");
        let settings = gradle.join("settings.gradle.kts");
        fs::create_dir_all(&gradle).unwrap();

        for source in [
            "pluginManagement { repositories { google(); gradlePluginPortal(); mavenCentral() } }\ndependencyResolutionManagement { repositories { google(); mavenCentral() } }",
            "repositories { maven { url = uri(\"https://repo.example.test\") } }",
            "repositories { mavenLocal() }",
            "repositories { flatDir { dirs(\"libs\") } }",
            "repositories { exclusiveContent { forRepository { mavenCentral() } } }",
        ] {
            fs::write(&settings, source).unwrap();
            let native = NativeInputs::scan(root.path()).unwrap();
            let reason =
                android_gradle_custom_repository_cache_disabled_reason(root.path(), &native)
                    .unwrap();
            if source.starts_with("pluginManagement") {
                assert!(
                    reason.is_none(),
                    "standard repositories were rejected: {source}"
                );
            } else {
                assert!(
                    reason.as_deref().is_some_and(|value| value
                        .contains(ANDROID_GRADLE_CUSTOM_REPOSITORY_CACHE_DISABLED_REASON)),
                    "custom repository was not rejected: {source}"
                );
            }
        }
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
    fn android_cache_policy_allows_unsigned_release_and_bypasses_sensitive_signing_inputs() {
        let mut release_native = NativeInputs::default();
        let (release_reason, release_identity) =
            android_cache_hit_eligibility(&mut release_native, true, false, None);
        assert!(release_reason.is_none());
        assert!(release_identity.is_none());
        assert!(release_native.external_hashes.is_empty());

        let mut sensitive_native = NativeInputs::default();
        sensitive_native
            .excluded_sensitive_files
            .push("mobile/android/gradle/keystore.properties".into());
        let (sensitive_reason, sensitive_identity) =
            android_cache_hit_eligibility(&mut sensitive_native, true, false, None);
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
    fn android_release_without_signing_configuration_keeps_cache_eligible() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("build.gradle.kts"), "plugins {}\n").unwrap();

        let mut native = NativeInputs::scan(root.path()).unwrap();
        let policy = android_cache_signing_policy(root.path(), true, &mut native).unwrap();

        assert!(policy.disabled_reason.is_none());
        assert!(policy.signing_identity.is_none());
        assert!(policy.debug_keystore_identity.is_none());
    }

    #[test]
    fn android_release_preview_remains_cache_disabled() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("build.gradle.kts"), "plugins {}\n").unwrap();

        let policy = android_preview_cache_policy(root.path(), true, "arm64-v8a").unwrap();
        assert!(
            policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("release APK is not a live preview"))
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

        let policy = android_preview_cache_policy(root.path(), false, "arm64-v8a").unwrap();
        assert_eq!(
            policy.key,
            android_build_key(root.path(), false, &["arm64-v8a".into()]).unwrap()
        );
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
        write_android_verification_metadata(root.path(), STRICT_ANDROID_VERIFICATION_METADATA);

        let policy = android_preview_cache_policy(root.path(), false, "arm64-v8a").unwrap();
        assert!(
            !policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("custom/release signing cache reuse"))
        );
        assert!(policy.android_signing_fingerprint.is_some());
        assert!(policy.debug_keystore_hash.is_none());
        assert!(
            !policy
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("dependency verification metadata"))
        );
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
    fn gradle_plugin_signing_source_disables_android_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        let plugin = root
            .path()
            .join("mobile/android/gradle/buildSrc/src/main/kotlin");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            "plugins { id(\"com.android.application\") }\n",
        )
        .unwrap();
        fs::write(
            plugin.join("RemoteSigningPlugin.kt"),
            "variant.signingConfig = remoteSigningConfig()\n",
        )
        .unwrap();

        let mut native = NativeInputs::scan(root.path()).unwrap();
        assert!(has_custom_android_signing_config(root.path(), &native).unwrap());
        let policy = android_cache_signing_policy_with_debug_identity(
            root.path(),
            false,
            &mut native,
            Some(AndroidDebugKeystoreIdentity {
                path: root.path().join("debug.keystore"),
                sha256: "d".repeat(64),
            }),
        )
        .unwrap();

        assert!(policy.debug_keystore_identity.is_none());
        assert!(policy.signing_identity.is_none());
        assert!(policy.disabled_reason.as_deref().is_some_and(|reason| {
            reason.contains("not a supported local keystore.properties layout")
        }));
    }

    #[test]
    fn included_android_gradle_build_logic_disables_cache_reuse() {
        let root = tempfile::tempdir().unwrap();
        let gradle = root.path().join("mobile/android/gradle");
        let app = gradle.join("app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("build.gradle.kts"), "plugins {}\n").unwrap();
        fs::write(
            gradle.join("settings.gradle.kts"),
            "pluginManagement { includeBuild(\"../../build-logic\") }\n",
        )
        .unwrap();

        let mut native = NativeInputs::scan(root.path()).unwrap();
        let policy = android_cache_signing_policy_with_debug_identity(
            root.path(),
            false,
            &mut native,
            Some(AndroidDebugKeystoreIdentity {
                path: root.path().join("debug.keystore"),
                sha256: "d".repeat(64),
            }),
        )
        .unwrap();

        assert!(policy.debug_keystore_identity.is_some());
        assert!(policy.disabled_reason.as_deref().is_some_and(|reason| {
            reason.contains("included build logic is outside the signing input closure")
        }));
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
