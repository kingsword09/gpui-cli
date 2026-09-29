//! `gpui run --live`: watch the project, rebuild on change, relaunch.
//!
//! The loop is a single-flight pipeline: one build/install at a time, changes
//! arriving mid-build are coalesced into one pending rebuild. A failed build
//! keeps the previously launched app running (mobile) or the previous process
//! alive (desktop); the loop just keeps watching until the fix lands.

use anyhow::{Context, Result, bail};
use colored::Colorize;
use notify_debouncer_full::notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use std::collections::BTreeMap;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::error;
use super::run::{
    IosTarget, Project, android_abis, android_rust_target, bundle_id_of, bundle_id_of_android,
    check_android_libraries_at, ensure_tool, gradle_task, leased_device_step, resolve_ios_target,
    xcode_app_path, xcode_destination,
};
use crate::device::{self, DeviceFlags, android, inventory, ios};
use crate::devserver::control::ControlServer;
use crate::devserver::events::{Kind, Scope};
use crate::devserver::inputs::{AssetDelta, should_trigger};
use crate::devserver::output::{self, AppProcess};
use crate::devserver::protocol::{self, AssetManifestEntry, ServerMessage};
use crate::devserver::session::{Build, Session};
use crate::devserver::timing;
use crate::devserver::{AssetReconciliation, DevServer};
use crate::runner::build_cache::{BuildCacheLookup, BuildOutputLock, lookup_verified_at_path};
use crate::runner::build_coordinator::coordinate_preview_build_with_verifier;
use crate::runner::build_inputs::android_debug_keystore_hash;
use crate::runner::build_manifest::BuildArtifactManifest;
use crate::runner::lease::DeviceLeaseSession;
use crate::runner::output_layout::{
    BuildOutputLayout, BuildPlatform, PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE,
};
use serde_json::json;

/// Source files watch out for asset-only changes under this directory; they
/// can reload in the running app instead of triggering a rebuild.
const ASSETS_DIR: &str = "assets";
const ANDROID_PREVIEW_FIXTURE: &str = "gpui_preview_fixture.json";
const ANDROID_PREVIEW_DATA_DIR: &str = "gpui_preview_data";
/// How long the live loop waits for the app to hand over its snapshot.
const SNAPSHOT_WAIT: Duration = Duration::from_millis(1500);
/// Snapshots above this size are rejected (the frame cap is 1 MiB).
const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
/// Snapshots older than this are pruned on the next save.
const SNAPSHOT_TTL: Duration = Duration::from_secs(24 * 3600);
const PREVIEW_BUILD_FAILED: &str = "preview build failed";
const PREVIEW_BUILD_SUPERSEDED: &str = "preview build superseded";

/// Editors fire several events per save; this absorbs the burst.
const DEBOUNCE_MS: u64 = 400;

enum Event {
    Change,
    /// Exact asset-only changes derived from the input content manifest.
    Assets(AssetDelta),
    Force,
    Quit,
}

enum Iteration {
    Rebuilt,
    BuildFailed,
    Superseded,
}

/// The resolved launch plan; device selection happens once, up front.
enum Plan {
    Desktop,
    Ios {
        physical: bool,
        id: String,
        label: String,
    },
    Android {
        serial: String,
        label: String,
    },
}

/// Explicit scenario inputs handed to the generated preview runtime. The
/// preview path never carries a Live snapshot; every launch gets its own data
/// directory and starts at reset_generation 1.
#[derive(Clone, Debug)]
pub struct PreviewBuildOutputs {
    pub output_root: PathBuf,
    pub cargo_target_dir: PathBuf,
    pub build_key_hash: Option<String>,
    pub cache_hit_disabled_reason: Option<String>,
    pub android_debug_keystore_hash: Option<String>,
    pub jni_libs_dir: Option<PathBuf>,
    pub gradle_build_dir: Option<PathBuf>,
    pub ios_derived_data_dir: Option<PathBuf>,
}

impl PreviewBuildOutputs {
    pub fn from_environment() -> Option<Self> {
        let output_root = std::env::var_os("GPUI_PREVIEW_BUILD_OUTPUT_ROOT")?;
        let cargo_target_dir = std::env::var_os("GPUI_PREVIEW_CARGO_TARGET_DIR")?;
        Some(Self {
            output_root: PathBuf::from(output_root),
            cargo_target_dir: PathBuf::from(cargo_target_dir),
            build_key_hash: std::env::var("GPUI_PREVIEW_BUILD_KEY_HASH").ok(),
            cache_hit_disabled_reason: std::env::var("GPUI_PREVIEW_CACHE_HIT_DISABLED_REASON").ok(),
            android_debug_keystore_hash: std::env::var("GPUI_PREVIEW_ANDROID_DEBUG_KEYSTORE_HASH")
                .ok(),
            jni_libs_dir: std::env::var_os("GPUI_PREVIEW_JNI_LIBS_DIR").map(PathBuf::from),
            gradle_build_dir: std::env::var_os("GPUI_PREVIEW_GRADLE_BUILD_DIR").map(PathBuf::from),
            ios_derived_data_dir: std::env::var_os("GPUI_PREVIEW_IOS_DERIVED_DATA_DIR")
                .map(PathBuf::from),
        })
    }
}

#[derive(Clone, Debug)]
pub struct PreviewLaunch {
    pub scenario_id: String,
    pub component: String,
    pub fixture_path: PathBuf,
    pub fixture_hash: String,
    pub data_dir: PathBuf,
    pub project_root: PathBuf,
    pub theme: String,
    pub locale: String,
    pub clock: String,
    pub clock_at: Option<String>,
    pub random_seed: Option<i64>,
    pub uncontrolled_inputs: Vec<String>,
    pub build_outputs: Option<PreviewBuildOutputs>,
}

/// Dev-channel credentials handed to the app at every launch.
struct Channel {
    live: Arc<Session>,
    scope: Scope,
    overflow: Arc<AtomicBool>,
    project: String,
    port: u16,
    token: String,
    assets_dir: PathBuf,
    /// Snapshot session to restore in the next launch, if the previous process
    /// saved one.
    session: Option<String>,
    preview: Option<PreviewLaunch>,
}

impl Channel {
    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn sessions_dir(project: &Project) -> PathBuf {
        project.root.join(".gpui").join("sessions")
    }

    /// Environment variables the app reads (desktop directly, iOS simulator
    /// via the `SIMCTL_CHILD_` prefix added by `simctl launch`).
    fn env(&self, project: &Project) -> Vec<(String, String)> {
        let mut env = vec![
            ("GPUI_LIVE_PROJECT".to_string(), self.project.clone()),
            ("GPUI_LIVE_ADDR".to_string(), self.addr()),
            ("GPUI_LIVE_TOKEN".to_string(), self.token.clone()),
            (
                "GPUI_LIVE_BUILD_ID".to_string(),
                self.scope.build_id.clone().unwrap_or_default(),
            ),
            (
                "GPUI_LIVE_RUN_ID".to_string(),
                self.scope.run_id.clone().unwrap_or_default(),
            ),
            (
                "GPUI_LIVE_SOURCE_REVISION".to_string(),
                self.scope.revision.source_revision.to_string(),
            ),
            (
                "GPUI_LIVE_ASSET_REVISION".to_string(),
                self.scope.revision.asset_revision.to_string(),
            ),
            (
                "GPUI_LIVE_ASSETS".to_string(),
                self.assets_dir.to_string_lossy().into_owned(),
            ),
        ];
        if let Some(session) = &self.session {
            env.push(("GPUI_LIVE_SESSION".to_string(), session.clone()));
            env.push((
                "GPUI_LIVE_STATE_FILE".to_string(),
                Self::sessions_dir(project)
                    .join(format!("{session}.state"))
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        if let Some(preview) = &self.preview {
            env.extend([
                (
                    "GPUI_PREVIEW_SCENARIO_ID".to_string(),
                    preview.scenario_id.clone(),
                ),
                (
                    "GPUI_PREVIEW_COMPONENT".to_string(),
                    preview.component.clone(),
                ),
                (
                    "GPUI_PREVIEW_FIXTURE".to_string(),
                    preview.fixture_path.to_string_lossy().into_owned(),
                ),
                (
                    "GPUI_PREVIEW_FIXTURE_HASH".to_string(),
                    preview.fixture_hash.clone(),
                ),
                (
                    "GPUI_PREVIEW_DATA_DIR".to_string(),
                    preview.data_dir.to_string_lossy().into_owned(),
                ),
                (
                    "GPUI_PREVIEW_PROJECT_ROOT".to_string(),
                    preview.project_root.to_string_lossy().into_owned(),
                ),
                ("GPUI_PREVIEW_THEME".to_string(), preview.theme.clone()),
                ("GPUI_PREVIEW_LOCALE".to_string(), preview.locale.clone()),
                ("GPUI_PREVIEW_CLOCK".to_string(), preview.clock.clone()),
                (
                    "GPUI_PREVIEW_UNCONTROLLED_INPUTS".to_string(),
                    preview.uncontrolled_inputs.join(","),
                ),
            ]);
            if let Some(clock_at) = &preview.clock_at {
                env.push(("GPUI_PREVIEW_CLOCK_AT".to_string(), clock_at.clone()));
            }
            if let Some(random_seed) = preview.random_seed {
                env.push((
                    "GPUI_PREVIEW_RANDOM_SEED".to_string(),
                    random_seed.to_string(),
                ));
            }
        }
        env
    }

    /// File contents for platforms with no environment to inherit (Android).
    fn device_config(&self) -> String {
        let session = self.session.as_deref().unwrap_or_default();
        let mut config = format!(
            "project={}\naddr={}\ntoken={}\nsession={session}\nbuild_id={}\nrun_id={}\nsource_revision={}\nasset_revision={}\n",
            self.project,
            self.addr(),
            self.token,
            self.scope.build_id.as_deref().unwrap_or_default(),
            self.scope.run_id.as_deref().unwrap_or_default(),
            self.scope.revision.source_revision,
            self.scope.revision.asset_revision,
        );
        if let Some(preview) = &self.preview {
            config.push_str(&format!(
                "preview_scenario_id={}\npreview_component={}\npreview_fixture={}\npreview_fixture_hash={}\npreview_data_dir={}\npreview_theme={}\npreview_locale={}\npreview_clock={}\npreview_uncontrolled_inputs={}\n",
                preview.scenario_id,
                preview.component,
                ANDROID_PREVIEW_FIXTURE,
                preview.fixture_hash,
                ANDROID_PREVIEW_DATA_DIR,
                preview.theme,
                preview.locale,
                preview.clock,
                preview.uncontrolled_inputs.join(","),
            ));
            if let Some(clock_at) = &preview.clock_at {
                config.push_str(&format!("preview_clock_at={clock_at}\n"));
            }
            if let Some(random_seed) = preview.random_seed {
                config.push_str(&format!("preview_random_seed={random_seed}\n"));
            }
        }
        config
    }
}

/// Slash-relative path of an asset change under `<root>/assets/`, if it is one.
fn asset_rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    if rel.components().next()?.as_os_str() != ASSETS_DIR {
        return None;
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Path the app is launched from.
///
/// Windows locks a running image against replacement, so launching cargo's own
/// output makes the *next* build fail with "Access is denied" and the loop can
/// never install a successful rebuild. Each launch therefore runs from a sibling
/// copy; cargo's output stays replaceable while the app runs. The copy sits beside
/// it so DLLs and other resources the executable finds next to itself still
/// resolve. Other platforms launch the built binary directly — this is purely
/// about the Windows file lock.
fn launch_path(executable: &Path) -> Result<PathBuf> {
    if !cfg!(windows) {
        return Ok(executable.to_owned());
    }
    let stem = executable
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app");
    let extension = executable
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("exe");
    let path = executable.with_file_name(format!("{stem}-live.{extension}"));
    // The previous process has just been killed; Windows can take a moment to
    // release its image, and a scanner may hold the fresh copy briefly.
    let mut last = None;
    for _ in 0..20 {
        match fs::copy(executable, &path) {
            Ok(_) => return Ok(path),
            Err(error) => {
                last = Some(error);
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Err(last.expect("a failed copy records its error")).with_context(|| {
        format!(
            "copying {} to {} (is another instance still running?)",
            executable.display(),
            path.display()
        )
    })
}

fn preview_desktop_executable_from_manifest(
    manifest: &BuildArtifactManifest,
    root: &Path,
) -> Option<PathBuf> {
    let mut executables = manifest
        .files
        .iter()
        .filter(|file| file.executable && file.path.starts_with("cargo-target/"))
        .map(|file| root.join(&file.path));
    let executable = executables.next()?;
    executables.next().is_none().then_some(executable)
}

fn publish_preview_desktop_manifest(root: &Path, key_hash: &str, executable: &Path) -> Result<()> {
    let executable = fs::canonicalize(executable)
        .with_context(|| format!("resolving preview executable {}", executable.display()))?;
    let relative = executable.strip_prefix(root).with_context(|| {
        format!(
            "preview executable {} is outside output root {}",
            executable.display(),
            root.display()
        )
    })?;
    let manifest = BuildArtifactManifest::capture_at(
        root,
        BuildPlatform::Desktop,
        key_hash,
        &[relative.to_owned()],
    )
    .context("capturing preview desktop artifact manifest")?;
    manifest
        .write_atomic(&root.join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE))
        .context("publishing preview desktop artifact manifest")?;
    Ok(())
}

fn preview_desktop_output_layout(
    outputs: &PreviewBuildOutputs,
    key_hash: &str,
) -> BuildOutputLayout {
    BuildOutputLayout {
        platform: BuildPlatform::Desktop,
        key_hash: key_hash.to_owned(),
        root: outputs.output_root.clone(),
        cargo_target_dir: outputs.cargo_target_dir.clone(),
        native_staging_dir: outputs.output_root.join("native-staging"),
        android_jni_dir: None,
        android_gradle_build_dir: None,
        ios_derived_data_dir: None,
    }
}

fn verified_preview_desktop_executable(root: &Path, key_hash: &str) -> Result<PathBuf> {
    let manifest = match lookup_verified_at_path(
        &root.join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE),
        root,
        BuildPlatform::Desktop,
        key_hash,
    ) {
        BuildCacheLookup::Hit(manifest) => manifest,
        BuildCacheLookup::Miss(reason) => {
            bail!("preview artifact manifest is not verified: {reason}")
        }
    };
    preview_desktop_executable_from_manifest(&manifest, root)
        .context("preview artifact manifest does not identify one desktop executable")
}

fn preview_ios_output_layout(outputs: &PreviewBuildOutputs, key_hash: &str) -> BuildOutputLayout {
    BuildOutputLayout {
        platform: BuildPlatform::Ios,
        key_hash: key_hash.to_owned(),
        root: outputs.output_root.clone(),
        cargo_target_dir: outputs.cargo_target_dir.clone(),
        native_staging_dir: outputs.output_root.join("native-staging"),
        android_jni_dir: None,
        android_gradle_build_dir: None,
        ios_derived_data_dir: outputs.ios_derived_data_dir.clone(),
    }
}

fn verify_preview_ios_output(
    layout: &BuildOutputLayout,
    key_hash: &str,
    app_path: &Path,
) -> Result<()> {
    let manifest = match lookup_verified_at_path(
        &layout.preview_artifact_manifest_path(),
        &layout.root,
        BuildPlatform::Ios,
        key_hash,
    ) {
        BuildCacheLookup::Hit(manifest) => manifest,
        BuildCacheLookup::Miss(reason) => {
            bail!("preview iOS artifact manifest is not verified: {reason}")
        }
    };
    if !app_path.is_dir() || !preview_manifest_contains_root(&manifest, &layout.root, app_path) {
        bail!("preview iOS artifact manifest does not identify the expected app bundle");
    }
    Ok(())
}

fn verify_preview_desktop_output(layout: &BuildOutputLayout, key_hash: &str) -> Result<()> {
    verified_preview_desktop_executable(&layout.root, key_hash).map(|_| ())
}

fn preview_control_error(error: &anyhow::Error) -> Option<Iteration> {
    let message = error.to_string();
    if message.contains(PREVIEW_BUILD_FAILED) {
        Some(Iteration::BuildFailed)
    } else if message.contains(PREVIEW_BUILD_SUPERSEDED) {
        Some(Iteration::Superseded)
    } else {
        None
    }
}

fn coordinate_desktop_preview_build(
    project: &Project,
    outputs: &PreviewBuildOutputs,
    build: &Build,
) -> Result<PathBuf> {
    let key_hash = outputs
        .build_key_hash
        .as_deref()
        .context("desktop preview coordinator requires a BuildKey hash")?;
    let layout = preview_desktop_output_layout(outputs, key_hash);
    coordinate_preview_build_with_verifier(
        &layout,
        key_hash,
        || {
            match lookup_verified_at_path(
                &outputs
                    .output_root
                    .join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE),
                &outputs.output_root,
                BuildPlatform::Desktop,
                key_hash,
            ) {
                BuildCacheLookup::Hit(manifest) => {
                    if preview_desktop_executable_from_manifest(&manifest, &outputs.output_root)
                        .is_some()
                    {
                        println!("  {} preview BuildKey cache hit: {}", "✓".green(), key_hash);
                        return Ok(());
                    }
                    println!(
                        "  {} preview cache miss: manifest does not identify one desktop executable",
                        "→".blue()
                    );
                }
                BuildCacheLookup::Miss(reason) => {
                    println!("  {} preview cache miss: {reason}", "→".blue());
                }
            }

            let mut cmd = Command::new("cargo");
            cmd.current_dir(&project.root).args([
                "build",
                "-p",
                &project.desktop_crate(),
                "--features",
                "gpui-dev",
            ]);
            cmd.env("CARGO_TARGET_DIR", &outputs.cargo_target_dir);
            let outcome = error::run_cargo_json(&mut cmd, build, "cargo.build")?;
            if !outcome.success {
                bail!(PREVIEW_BUILD_FAILED);
            }
            let executable = outcome
                .executable
                .context("cargo succeeded but reported no binary path")?;
            if !build.is_current()? {
                bail!(PREVIEW_BUILD_SUPERSEDED);
            }
            publish_preview_desktop_manifest(&outputs.output_root, key_hash, &executable)?;
            Ok(())
        },
        verify_preview_desktop_output,
    )?;
    verified_preview_desktop_executable(&outputs.output_root, key_hash)
}

fn preview_manifest_contains_root(
    manifest: &BuildArtifactManifest,
    root: &Path,
    expected: &Path,
) -> bool {
    expected
        .strip_prefix(root)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .is_some_and(|relative| manifest.roots.iter().any(|root| root == &relative))
}

fn preview_manifest_contains_roots(
    manifest: &BuildArtifactManifest,
    root: &Path,
    expected: &[&Path],
) -> bool {
    let mut expected = expected
        .iter()
        .filter_map(|path| {
            path.strip_prefix(root)
                .ok()
                .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        })
        .collect::<Vec<_>>();
    expected.sort();
    expected == manifest.roots
}

fn publish_preview_ios_manifest(root: &Path, key_hash: &str, app_path: &Path) -> Result<()> {
    let app_relative = app_path.strip_prefix(root).with_context(|| {
        format!(
            "preview iOS app bundle {} is outside output root {}",
            app_path.display(),
            root.display()
        )
    })?;
    let manifest = BuildArtifactManifest::capture_at(
        root,
        BuildPlatform::Ios,
        key_hash,
        &[app_relative.to_owned()],
    )
    .context("capturing preview iOS artifact manifest")?;
    manifest
        .write_atomic(&root.join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE))
        .context("publishing preview iOS artifact manifest")?;
    Ok(())
}

fn publish_preview_android_manifest(
    root: &Path,
    key_hash: &str,
    jni_libs_dir: &Path,
    apk_path: &Path,
) -> Result<()> {
    let apk_output_dir = apk_path
        .parent()
        .context("Android preview APK has no output directory")?;
    let jni_relative = jni_libs_dir.strip_prefix(root).with_context(|| {
        format!(
            "preview Android JNI staging path is outside output root {}",
            jni_libs_dir.display()
        )
    })?;
    let apk_relative = apk_output_dir.strip_prefix(root).with_context(|| {
        format!(
            "preview Android APK output path is outside output root {}",
            apk_output_dir.display()
        )
    })?;
    let manifest = BuildArtifactManifest::capture_at(
        root,
        BuildPlatform::Android,
        key_hash,
        &[jni_relative.to_owned(), apk_relative.to_owned()],
    )
    .context("capturing preview Android artifact manifest")?;
    manifest
        .write_atomic(&root.join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE))
        .context("publishing preview Android artifact manifest")?;
    Ok(())
}

/// Runs one build + (re)launch cycle. `Ok(BuildFailed)` means a compile failure
/// was already rendered; infrastructure errors come back as `Err`.
fn run_iteration(
    project: &Project,
    plan: &Plan,
    channel: &mut Channel,
    server: &DevServer,
    child: &mut Option<AppProcess>,
    build: &Build,
    device_lease: Option<&DeviceLeaseSession>,
) -> Result<Iteration> {
    match plan {
        Plan::Desktop => {
            let outputs = channel
                .preview
                .as_ref()
                .and_then(|preview| preview.build_outputs.as_ref())
                .cloned();
            let executable = if let Some(outputs) = &outputs
                && outputs.build_key_hash.is_some()
                && outputs.cache_hit_disabled_reason.is_none()
            {
                match coordinate_desktop_preview_build(project, outputs, build) {
                    Ok(executable) => executable,
                    Err(error) => {
                        if let Some(control) = preview_control_error(&error) {
                            return Ok(control);
                        }
                        return Err(error);
                    }
                }
            } else {
                let _output_lock = outputs
                    .as_ref()
                    .map(|outputs| BuildOutputLock::acquire_at_root(&outputs.output_root))
                    .transpose()?;
                let executable = if let Some(outputs) = &outputs {
                    if let Some(key_hash) = outputs.build_key_hash.as_deref() {
                        match lookup_verified_at_path(
                            &outputs
                                .output_root
                                .join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE),
                            &outputs.output_root,
                            BuildPlatform::Desktop,
                            key_hash,
                        ) {
                            BuildCacheLookup::Hit(manifest) => {
                                preview_desktop_executable_from_manifest(
                                    &manifest,
                                    &outputs.output_root,
                                )
                            }
                            BuildCacheLookup::Miss(_) => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let executable = if let Some(executable) = executable {
                    executable
                } else {
                    let mut cmd = Command::new("cargo");
                    cmd.current_dir(&project.root).args([
                        "build",
                        "-p",
                        &project.desktop_crate(),
                        "--features",
                        "gpui-dev",
                    ]);
                    if let Some(outputs) = &outputs {
                        cmd.env("CARGO_TARGET_DIR", &outputs.cargo_target_dir);
                    }
                    let outcome = error::run_cargo_json(&mut cmd, build, "cargo.build")?;
                    if !outcome.success {
                        return Ok(Iteration::BuildFailed);
                    }
                    outcome
                        .executable
                        .context("cargo succeeded but reported no binary path")?
                };
                if !build.is_current()? {
                    return Ok(Iteration::Superseded);
                }
                if let Some(outputs) = &outputs
                    && let Some(key_hash) = outputs.build_key_hash.as_deref()
                {
                    publish_preview_desktop_manifest(&outputs.output_root, key_hash, &executable)?;
                }
                executable
            };
            if !build.is_current()? {
                return Ok(Iteration::Superseded);
            }
            prepare_restart(project, server, channel);
            if !build.is_current()? {
                return Ok(Iteration::Superseded);
            }
            drop(child.take());
            prepare_launch(channel, server, build)?;
            let executable = launch_path(&executable)?;
            let mut cmd = Command::new(&executable);
            cmd.current_dir(&project.root);
            for (key, value) in channel.env(project) {
                cmd.env(key, value);
            }
            *child = Some(AppProcess::spawn(
                &mut cmd,
                channel.live.clone(),
                channel.scope.clone(),
            )?);
            println!("{}", "✓ restarted".green());
        }
        Plan::Ios {
            physical,
            id,
            label,
        } => {
            let bundle_id = bundle_id_of(project);
            let app = match build_ios_app_live(
                project,
                *physical,
                id,
                build,
                channel
                    .preview
                    .as_ref()
                    .and_then(|preview| preview.build_outputs.as_ref()),
            ) {
                Ok(Some(app)) => app,
                Ok(None) => return Ok(Iteration::BuildFailed),
                Err(error) => {
                    if let Some(control) = preview_control_error(&error) {
                        return Ok(control);
                    }
                    return Err(error);
                }
            };
            if !build.is_current()? {
                return Ok(Iteration::Superseded);
            }
            if !physical {
                prepare_restart(project, server, channel);
            }
            if !build.is_current()? {
                return Ok(Iteration::Superseded);
            }
            prepare_launch(channel, server, build)?;
            if *physical {
                observed_device_step(build, device_lease, "ios.install", || {
                    ios::install_device(id, &app)
                })?;
                observed_device_step(build, device_lease, "ios.launch", || {
                    ios::launch_device(id, &bundle_id)
                })?;
            } else {
                observed_device_step(build, device_lease, "ios.install", || {
                    ios::install_simulator(id, &app)
                })?;
                let env = channel.env(project);
                observed_device_step(build, device_lease, "ios.launch", || {
                    ios::launch_simulator_with_env(id, &bundle_id, &env)
                })?;
            }
            channel.live.emit(
                Kind::AppStarted,
                &channel.scope,
                json!({"confirmed": false, "device": id}),
            );
            println!("{}", format!("✓ relaunched on {label}").green());
        }
        Plan::Android { serial, label } => {
            let bundle_id = bundle_id_of_android(project);
            let Some(apk) = build_android_apk_live(
                project,
                build,
                channel
                    .preview
                    .as_ref()
                    .and_then(|preview| preview.build_outputs.as_ref()),
            )?
            else {
                return Ok(Iteration::BuildFailed);
            };
            if !build.is_current()? {
                return Ok(Iteration::Superseded);
            }
            // Installation stops the old app, so snapshot it first.
            prepare_restart(project, server, channel);
            if !build.is_current()? {
                return Ok(Iteration::Superseded);
            }
            prepare_launch(channel, server, build)?;
            observed_device_step(build, device_lease, "android.install", || {
                android::install_apk(serial, &apk)
            })?;
            if let Some(preview) = channel.preview.as_ref() {
                let fixture = fs::read(&preview.fixture_path).with_context(|| {
                    format!(
                        "reading Android preview fixture {}",
                        preview.fixture_path.display()
                    )
                })?;
                observed_device_step(build, device_lease, "android.preview_fixture", || {
                    android::write_device_config(
                        serial,
                        &bundle_id,
                        ANDROID_PREVIEW_FIXTURE,
                        &fixture,
                    )
                })?;
            }
            push_android_assets(
                project,
                serial,
                &bundle_id,
                &all_asset_paths(project),
                device_lease,
            );
            if let Some(session) = &channel.session {
                let state = Channel::sessions_dir(project).join(format!("{session}.state"));
                match fs::read(&state)
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| {
                        leased_device_step(device_lease, "android.restore", || {
                            android::write_device_config(serial, &bundle_id, "gpui_state", &bytes)
                        })
                    }) {
                    Ok(()) => {}
                    Err(error) => {
                        channel.live.emit(Kind::AppLog, &channel.scope, json!({"level": "warn", "target": "restore", "message": error.to_string()}));
                        channel.session = None;
                    }
                }
            }
            observed_device_step(build, device_lease, "android.configure", || {
                android::write_device_config(
                    serial,
                    &bundle_id,
                    "gpui_live.txt",
                    channel.device_config().as_bytes(),
                )?;
                android::force_stop(serial, &bundle_id)?;
                android::reverse_port(serial, channel.port)
            })?;
            observed_device_step(build, device_lease, "android.launch", || {
                android::launch_app(serial, &bundle_id)
            })?;
            channel.live.emit(
                Kind::AppStarted,
                &channel.scope,
                json!({"confirmed": false, "device": serial}),
            );
            println!("{}", format!("✓ relaunched on {label}").green());
        }
    }
    Ok(Iteration::Rebuilt)
}

fn prepare_launch(channel: &mut Channel, server: &DevServer, build: &Build) -> Result<()> {
    channel.scope = channel.live.begin_run(build);
    channel.token = server.expect_run(channel.scope.clone())?;
    fs::write(channel.live.root.join(".gpui/dev-token"), &channel.token)?;
    Ok(())
}

fn observed_step<T>(build: &Build, stage: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if build.session.stopping.load(Ordering::SeqCst) {
        bail!("live session is stopping");
    }
    let span = build.session.start_span(
        timing::stage_name(stage),
        &build.scope,
        Some(build.span_id()),
        json!({"stage": stage}),
    );
    build
        .session
        .emit(Kind::StageStarted, &build.scope, json!({"stage": stage}));
    let result = f();
    build.session.emit(Kind::StageFinished, &build.scope,
        json!({"stage": stage, "success": result.is_ok(), "error": result.as_ref().err().map(|e| format!("{e:#}"))}));
    if let Err(error) = &result {
        let message = format!("{error:#}");
        span.finish("failed", Some(&message));
    } else {
        span.finish("ok", None);
    }
    result
}

fn observed_device_step<T>(
    build: &Build,
    lease: Option<&DeviceLeaseSession>,
    stage: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    observed_step(build, stage, || leased_device_step(lease, stage, operation))
}

/// Asks the running app to save its snapshot for the next session and stores
/// it under `.gpui/sessions/` (atomic temp-file + rename).
///
/// Without a connected app — or if the app cannot snapshot — the restart is a
/// plain cold start and says so; it never blocks or fails the cycle.
fn prepare_restart(project: &Project, server: &DevServer, channel: &mut Channel) {
    if !server.has_clients() {
        channel.session = None;
        return;
    }
    let session = format!(
        "s-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let data = server.save_state(&session, SNAPSHOT_WAIT);
    let Some(data) = data else {
        channel.session = None;
        println!(
            "{}",
            "[live] app did not save state (no snapshot support or busy) — restarting without state"
                .yellow()
        );
        return;
    };
    if data.len() > MAX_SNAPSHOT_BYTES {
        channel.session = None;
        println!(
            "{}",
            format!(
                "⚠ snapshot is {} bytes (limit {}) — restarting without state",
                data.len(),
                MAX_SNAPSHOT_BYTES
            )
            .yellow()
        );
        return;
    }
    let dir = Channel::sessions_dir(project);
    if fs::create_dir_all(&dir).is_err() {
        channel.session = None;
        return;
    }
    prune_sessions(&dir);
    let path = dir.join(format!("{session}.state"));
    let tmp = dir.join(format!("{session}.state.tmp"));
    match fs::write(&tmp, data.as_bytes()).and_then(|_| fs::rename(&tmp, &path)) {
        Ok(()) => {
            channel.session = Some(session);
            println!(
                "{}",
                "[live] state snapshot saved — restoring after restart".dimmed()
            );
        }
        Err(err) => {
            channel.session = None;
            println!(
                "{}",
                format!("⚠ failed to write the snapshot: {err} — restarting without state")
                    .yellow()
            );
        }
    }
}

/// Removes snapshots older than the TTL.
fn prune_sessions(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_sub(SNAPSHOT_TTL);
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok());
        if meta.is_file()
            && modified.map(|age| age < cutoff).unwrap_or(false)
            && fs::remove_file(entry.path()).is_err()
        {
            // Best effort; stale snapshots only waste a little disk.
        }
    }
}

/// Every file under `<project>/assets`, as slash-separated relative paths.
fn all_asset_paths(project: &Project) -> Vec<String> {
    let assets_dir = project.root.join(ASSETS_DIR);
    let mut out = Vec::new();
    let mut stack = vec![assets_dir.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(rel) = asset_rel_path(&project.root, &path) {
                out.push(rel);
            }
        }
    }
    out
}

/// Sends the given asset files to the connected iOS app over the dev
/// channel (the app sandbox cannot read the project directory, so the bytes
/// travel as base64 `asset_data` messages). Returns the paths that were
/// actually queued. Not connected: silently skipped (empty).
fn push_ios_assets(
    project: &Project,
    paths: Vec<String>,
    server: &DevServer,
    transfer_id: &str,
    asset_revision: u64,
    lease: Option<&DeviceLeaseSession>,
) -> Vec<String> {
    push_ios_assets_to(
        project,
        paths,
        server,
        None,
        transfer_id,
        asset_revision,
        lease,
    )
}

fn push_ios_assets_to(
    project: &Project,
    paths: Vec<String>,
    server: &DevServer,
    connection_id: Option<u64>,
    transfer_id: &str,
    asset_revision: u64,
    lease: Option<&DeviceLeaseSession>,
) -> Vec<String> {
    let mut pushed = Vec::new();
    for rel in paths {
        match fs::read(project.root.join(&rel)) {
            Ok(bytes) => {
                if !ios_frame_fits(bytes.len(), &rel) {
                    println!(
                        "{}",
                        format!(
                            "⚠ {rel} is {} KiB — over the dev-channel frame limit; it cannot be \
                             pushed or hot-reloaded on iOS",
                            bytes.len() / 1024
                        )
                        .yellow()
                    );
                    continue;
                }
                let message = ServerMessage::AssetData {
                    transfer_id: transfer_id.to_string(),
                    path: rel.clone(),
                    data: protocol::b64::encode(&bytes),
                    asset_revision,
                };
                let sent = leased_device_step(lease, "ios.asset.transfer", || {
                    Ok(if let Some(connection_id) = connection_id {
                        server.send_to(connection_id, &message)
                    } else {
                        server.broadcast(&message);
                        true
                    })
                })
                .unwrap_or_else(|error| {
                    println!("{}", format!("⚠ failed to sync {rel}: {error:#}").yellow());
                    false
                });
                if sent {
                    pushed.push(rel);
                }
            }
            Err(err) => println!("{}", format!("⚠ failed to read {rel}: {err}").yellow()),
        }
    }
    pushed
}

/// Whether an asset of this raw size still fits one dev-channel frame once
/// base64-encoded into an `asset_data` message for `path` (`MAX_FRAME_LEN`
/// bounds the whole JSON payload).
fn ios_frame_fits(raw_len: usize, path: &str) -> bool {
    let encoded = raw_len.div_ceil(3) * 4;
    // JSON shape and escaping headroom on top of path and data.
    encoded + path.len() + 64 <= protocol::MAX_FRAME_LEN as usize
}

/// Copies the given assets (relative to `<project>/assets`) into the app's
/// files dir on the device. Returns the paths that were actually staged;
/// failures are reported but never fatal.
fn push_android_assets(
    project: &Project,
    serial: &str,
    package: &str,
    paths: &[String],
    lease: Option<&DeviceLeaseSession>,
) -> Vec<String> {
    let mut pushed = Vec::new();
    for rel in paths {
        let source = project.root.join(rel);
        match fs::read(&source) {
            Ok(bytes) => {
                let result = leased_device_step(lease, "android.asset.write", || {
                    android::write_device_config(serial, package, rel, &bytes)
                });
                if let Err(err) = result {
                    println!(
                        "{}",
                        format!("⚠ failed to sync asset {rel}: {err:#}").yellow()
                    );
                } else {
                    pushed.push(rel.clone());
                }
            }
            Err(err) => {
                println!("{}", format!("⚠ failed to read {rel}: {err}").yellow());
            }
        }
    }
    pushed
}

/// Removes deleted assets from the app's private files dir. `rm -f` makes a
/// reconnect/retry idempotent when the file was already absent on the device.
fn remove_android_assets(
    serial: &str,
    package: &str,
    paths: &[String],
    lease: Option<&DeviceLeaseSession>,
) -> Vec<String> {
    let mut removed = Vec::new();
    for path in paths {
        match leased_device_step(lease, "android.asset.remove", || {
            android::remove_device_file(serial, package, path)
        }) {
            Ok(()) => removed.push(path.clone()),
            Err(err) => println!(
                "{}",
                format!("⚠ failed to remove asset {path} from the device: {err:#}").yellow()
            ),
        }
    }
    removed
}

/// iOS build with streaming Cargo diagnostics and Xcode output.
/// `Ok(None)` means the Rust build failed.
fn build_ios_app_live(
    project: &Project,
    physical: bool,
    udid: &str,
    build: &Build,
    outputs: Option<&PreviewBuildOutputs>,
) -> Result<Option<std::path::PathBuf>> {
    let outputs = outputs.cloned();
    if !physical
        && let Some(outputs) = &outputs
        && let Some(key_hash) = outputs.build_key_hash.as_deref()
        && outputs.cache_hit_disabled_reason.is_none()
    {
        let derived_dir = outputs
            .ios_derived_data_dir
            .as_deref()
            .context("iOS preview coordinator requires a DerivedData output directory")?;
        let app_path = xcode_app_path(derived_dir, &project.xcode_target(), false, false);
        let layout = preview_ios_output_layout(outputs, key_hash);
        coordinate_preview_build_with_verifier(
            &layout,
            key_hash,
            || {
                if build_ios_app_live_once(project, false, udid, build, Some(outputs), false)?
                    .is_none()
                {
                    bail!(PREVIEW_BUILD_FAILED);
                }
                if !build.is_current()? {
                    bail!(PREVIEW_BUILD_SUPERSEDED);
                }
                Ok(())
            },
            |layout, key_hash| verify_preview_ios_output(layout, key_hash, &app_path),
        )?;
        verify_preview_ios_output(&layout, key_hash, &app_path)?;
        return Ok(Some(app_path));
    }

    build_ios_app_live_once(project, physical, udid, build, outputs.as_ref(), true)
}

fn build_ios_app_live_once(
    project: &Project,
    physical: bool,
    udid: &str,
    build: &Build,
    outputs: Option<&PreviewBuildOutputs>,
    acquire_output_lock: bool,
) -> Result<Option<std::path::PathBuf>> {
    let _output_lock = if acquire_output_lock {
        outputs
            .map(|outputs| BuildOutputLock::acquire_at_root(&outputs.output_root))
            .transpose()?
    } else {
        None
    };
    let ios_dir = project.ios_dir();
    let scheme = project.xcode_target();
    let derived_dir = outputs
        .and_then(|outputs| outputs.ios_derived_data_dir.clone())
        .unwrap_or_else(|| ios_dir.join("build"));
    let app_path = xcode_app_path(&derived_dir, &scheme, physical, false);
    if !physical
        && let Some(outputs) = outputs
        && let Some(key_hash) = outputs.build_key_hash.as_deref()
        && outputs.cache_hit_disabled_reason.is_none()
    {
        match lookup_verified_at_path(
            &outputs
                .output_root
                .join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE),
            &outputs.output_root,
            BuildPlatform::Ios,
            key_hash,
        ) {
            BuildCacheLookup::Hit(manifest)
                if app_path.is_dir()
                    && preview_manifest_contains_root(
                        &manifest,
                        &outputs.output_root,
                        &app_path,
                    ) =>
            {
                println!(
                    "  {} iOS preview BuildKey cache hit: {}",
                    "✓".green(),
                    key_hash
                );
                return Ok(Some(app_path));
            }
            BuildCacheLookup::Hit(_) => println!(
                "  {} iOS preview cache miss: manifest app bundle root is not usable",
                "→".blue()
            ),
            BuildCacheLookup::Miss(reason) => {
                println!("  {} iOS preview cache miss: {reason}", "→".blue())
            }
        }
    }
    let rust_target = if physical {
        "aarch64-apple-ios"
    } else {
        "aarch64-apple-ios-sim"
    };
    output::step(
        "rustup.target",
        Command::new("rustup").args(["target", "add", rust_target]),
        build,
    )?;

    let mut cargo = Command::new("cargo");
    cargo.current_dir(&project.root).args([
        "build",
        "--lib",
        "-p",
        &project.app_crate(),
        "--target",
        rust_target,
        "--features",
        "gpui-dev",
    ]);
    if let Some(outputs) = outputs {
        cargo.env("CARGO_TARGET_DIR", &outputs.cargo_target_dir);
    }
    let outcome = error::run_cargo_json(&mut cargo, build, "cargo.build")?;
    if !outcome.success {
        println!(
            "{}",
            "✗ build failed — keeping the current app running".yellow()
        );
        return Ok(None);
    }

    ensure_tool("xcodegen", "Install it with `brew install xcodegen`.")?;
    run_tool(
        "xcodegen generate",
        Command::new("xcodegen")
            .current_dir(&ios_dir)
            .args(["generate", "--spec", "project.yml"]),
        build,
    )?;

    let mut xcodebuild = Command::new("xcodebuild");
    xcodebuild
        .current_dir(&ios_dir)
        .arg("-project")
        .arg(ios_dir.join(format!("{scheme}.xcodeproj")))
        .arg("-scheme")
        .arg(&scheme)
        .arg("-configuration")
        .arg("Debug")
        .arg("-destination")
        .arg(xcode_destination(physical, udid))
        .arg("-derivedDataPath")
        .arg(&derived_dir)
        .arg("-allowProvisioningUpdates")
        .arg("build");
    run_tool("xcodebuild (Debug)", &mut xcodebuild, build)?;

    if !app_path.exists() {
        bail!(
            "xcodebuild finished but no app bundle was found at '{}'.",
            app_path.display()
        );
    }
    if !build.is_current()? {
        bail!(PREVIEW_BUILD_SUPERSEDED);
    }
    if let Some(outputs) = outputs
        && let Some(key_hash) = outputs.build_key_hash.as_deref()
    {
        publish_preview_ios_manifest(&outputs.output_root, key_hash, &app_path)?;
    }
    println!("  {} {}", "✓".green(), app_path.display());
    Ok(Some(app_path))
}

/// Android build with Cargo JSON diagnostics and streaming Gradle output.
/// `Ok(None)` means the Rust build failed.
fn build_android_apk_live(
    project: &Project,
    build: &Build,
    outputs: Option<&PreviewBuildOutputs>,
) -> Result<Option<std::path::PathBuf>> {
    let outputs = outputs.cloned();
    let _output_lock = outputs
        .as_ref()
        .map(|outputs| BuildOutputLock::acquire_at_root(&outputs.output_root))
        .transpose()?;
    let abis = android_abis()?;
    let jni_libs_dir = outputs
        .as_ref()
        .and_then(|outputs| outputs.jni_libs_dir.clone())
        .unwrap_or_else(|| project.android_jni_libs_dir());
    let gradle_build_dir = outputs
        .as_ref()
        .and_then(|outputs| outputs.gradle_build_dir.clone());
    let cache_key = outputs
        .as_ref()
        .and_then(|outputs| outputs.build_key_hash.as_deref());
    let cache_hit_enabled = outputs.as_ref().is_some_and(|outputs| {
        outputs.cache_hit_disabled_reason.is_none()
            && outputs.android_debug_keystore_hash.is_some()
            && cache_key.is_some()
    });
    if let Some(outputs) = &outputs
        && let Some(reason) = &outputs.cache_hit_disabled_reason
    {
        println!("  {} Android preview cache miss: {reason}", "→".blue());
    }
    if cache_hit_enabled {
        let outputs = outputs
            .as_ref()
            .expect("cache hit requires preview outputs");
        let expected_keystore_hash = outputs
            .android_debug_keystore_hash
            .as_deref()
            .expect("cache hit requires the Android debug keystore hash");
        let current_keystore_hash = android_debug_keystore_hash()?
            .context("default Android debug keystore is unavailable for preview cache reuse")?;
        if current_keystore_hash != expected_keystore_hash {
            bail!(
                "Android debug keystore changed after the preview BuildKey was planned; restart the preview"
            );
        }

        match super::run::apk_path_at(project, false, gradle_build_dir.as_deref()) {
            Ok(apk) => {
                let apk_output_dir = apk
                    .parent()
                    .context("Android preview APK has no output directory")?;
                match lookup_verified_at_path(
                    &outputs
                        .output_root
                        .join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE),
                    &outputs.output_root,
                    BuildPlatform::Android,
                    cache_key.expect("cache hit requires a BuildKey hash"),
                ) {
                    BuildCacheLookup::Hit(manifest)
                        if preview_manifest_contains_roots(
                            &manifest,
                            &outputs.output_root,
                            &[&jni_libs_dir, apk_output_dir],
                        ) =>
                    {
                        println!(
                            "  {} Android preview BuildKey cache hit: {}",
                            "✓".green(),
                            cache_key.expect("cache hit requires a BuildKey hash")
                        );
                        return Ok(Some(apk));
                    }
                    BuildCacheLookup::Hit(_) => println!(
                        "  {} Android preview cache miss: manifest roots are not usable",
                        "→".blue()
                    ),
                    BuildCacheLookup::Miss(reason) => {
                        println!("  {} Android preview cache miss: {reason}", "→".blue())
                    }
                }
            }
            Err(error) => println!(
                "  {} Android preview cache miss: existing APK metadata is unusable ({error:#})",
                "→".blue()
            ),
        }
    }
    ensure_tool(
        "cargo-ndk",
        "Install it with `cargo install cargo-ndk`, then set ANDROID_NDK_HOME.",
    )?;
    for abi in &abis {
        output::step(
            "rustup.target",
            Command::new("rustup").args(["target", "add", android_rust_target(abi)?]),
            build,
        )?;
    }
    let mut ndk = Command::new("cargo");
    ndk.current_dir(&project.root).args(["ndk"]);
    for abi in &abis {
        ndk.args(["-t", abi]);
    }
    ndk.arg("-o").arg(&jni_libs_dir).args([
        "--platform",
        "31",
        "build",
        "-p",
        &project.app_crate(),
        "--features",
        "gpui-dev",
    ]);
    if let Some(outputs) = &outputs {
        ndk.env("CARGO_TARGET_DIR", &outputs.cargo_target_dir);
    }
    if !error::run_cargo_json(&mut ndk, build, "cargo.ndk")?.success {
        return Ok(None);
    }

    check_android_libraries_at(&jni_libs_dir, &project.app_lib_name(), &abis)?;

    run_tool(
        &format!("gradlew {}", gradle_task(false)),
        &mut super::run::gradle_command_with_outputs(
            project,
            false,
            &abis,
            Some(&jni_libs_dir),
            outputs
                .as_ref()
                .and_then(|outputs| outputs.gradle_build_dir.as_deref()),
        ),
        build,
    )?;

    let apk = super::run::apk_path_at(project, false, gradle_build_dir.as_deref())?;
    if let Some(outputs) = &outputs
        && let Some(key_hash) = outputs.build_key_hash.as_deref()
    {
        if let Some(expected_keystore_hash) = outputs.android_debug_keystore_hash.as_deref() {
            let current_keystore_hash = android_debug_keystore_hash()?
                .context("default Android debug keystore disappeared during preview build")?;
            if current_keystore_hash != expected_keystore_hash {
                bail!(
                    "Android debug keystore changed during the preview build; refusing to publish its manifest"
                );
            }
        }
        publish_preview_android_manifest(&outputs.output_root, key_hash, &jni_libs_dir, &apk)?;
    }
    println!("  {} {}", "✓".green(), apk.display());
    Ok(Some(apk))
}

/// Streams external tool output into the live event store and terminal.
fn run_tool(label: &str, cmd: &mut Command, build: &Build) -> Result<()> {
    println!("  {} {}", "→".blue(), label);
    output::step(label, cmd, build)
}

/// One build cycle plus coalescing of everything that arrived during it.
/// Returns `true` when the user asked to quit.
#[allow(clippy::too_many_arguments)]
fn run_cycles(
    project: &Project,
    plan: &Plan,
    channel: &mut Channel,
    server: &DevServer,
    child: &mut Option<AppProcess>,
    rx: &mpsc::Receiver<Event>,
    last_failed: &mut bool,
    device_lease: Option<&DeviceLeaseSession>,
) -> Result<bool> {
    loop {
        if channel.live.stopping.load(Ordering::SeqCst) {
            return Ok(true);
        }
        if let Some(lease) = device_lease {
            lease
                .assert_owned()
                .map_err(anyhow::Error::new)
                .context("mobile device lease is no longer owned")?;
        }
        let build = channel.live.begin_build()?;
        if !server.set_asset_manifest(
            build.scope.revision.asset_revision,
            channel.live.asset_manifest(),
        ) {
            build.finish(
                false,
                Some("asset manifest exceeds the dev-channel frame limit".to_string()),
            );
            bail!("asset manifest exceeds the dev-channel frame limit");
        }
        let mut again = false;
        let failed =
            match run_iteration(project, plan, channel, server, child, &build, device_lease) {
                Ok(Iteration::Rebuilt) => {
                    build.finish(true, None);
                    if *last_failed {
                        println!("{}", "✓ build recovered".green());
                    }
                    false
                }
                Ok(Iteration::BuildFailed) => {
                    build.finish(false, None);
                    println!(
                        "{}",
                        "✗ build failed — keeping the current app running".yellow()
                    );
                    true
                }
                Ok(Iteration::Superseded) => {
                    build.superseded();
                    again = true;
                    *last_failed
                }
                Err(error) => {
                    build.finish(false, Some(format!("{error:#}")));
                    if channel.scope.build_id == build.scope.build_id {
                        channel.live.emit(
                            Kind::AppLaunchFailed,
                            &channel.scope,
                            json!({"error": format!("{error:#}")}),
                        );
                    }
                    println!("{}", format!("✗ {error:#}").red());
                    true
                }
            };
        *last_failed = failed;
        let latest = channel.live.sync_inputs()?;
        again |= latest != build.scope.revision || channel.overflow.swap(false, Ordering::SeqCst);
        let mut quit = channel.live.stopping.load(Ordering::SeqCst);
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Quit => quit = true,
                Event::Force => again = true,
                Event::Change | Event::Assets(_) => {}
            }
        }
        again |= channel.live.take_build_request().is_some();
        if quit {
            return Ok(true);
        }
        if !again {
            return Ok(false);
        }
    }
}

/// Pushes + broadcasts an asset-only change to the running app. Returns false
/// when no capable client is attached or the complete delta could not be
/// staged; the caller must fall back to a full rebuild.
fn reload_assets(
    project: &Project,
    plan: &Plan,
    server: &DevServer,
    delta: &AssetDelta,
    transfer_id: &str,
    asset_revision: u64,
    device_lease: Option<&DeviceLeaseSession>,
) -> bool {
    if !(server.has_clients() && server.all_clients_support_asset_reload()) {
        return false;
    }

    let Some((_, _, manifest)) = server.current_asset_manifest() else {
        return false;
    };
    let entries = delta
        .changed
        .iter()
        .filter_map(|path| manifest.iter().find(|entry| entry.path == *path).cloned())
        .collect::<Vec<_>>();
    if entries.len() != delta.changed.len() {
        return false;
    }
    if !delta.changed.is_empty() || !delta.removed.is_empty() {
        let begin = ServerMessage::AssetsBegin {
            transfer_id: transfer_id.to_string(),
            asset_revision,
            entries,
            removed: delta.removed.clone(),
        };
        let Ok(payload) = protocol::encode(&begin) else {
            return false;
        };
        if payload.len() > protocol::MAX_FRAME_LEN as usize {
            return false;
        }
        server.broadcast(&begin);
    }

    // Only announce files the app can actually reload — announcing a push or
    // removal that failed would make the client evict a cache entry it cannot
    // refill or leave a stale device-side file behind.
    let (updated, removed) = match plan {
        Plan::Android { serial, .. } => {
            let package = bundle_id_of_android(project);
            (
                push_android_assets(project, serial, &package, &delta.changed, device_lease),
                remove_android_assets(serial, &package, &delta.removed, device_lease),
            )
        }
        Plan::Ios {
            physical: false, ..
        } => (
            push_ios_assets(
                project,
                delta.changed.clone(),
                server,
                transfer_id,
                asset_revision,
                device_lease,
            ),
            delta.removed.clone(),
        ),
        Plan::Ios { physical: true, .. } => return false,
        Plan::Desktop => (delta.changed.clone(), delta.removed.clone()),
    };

    let complete = updated.len() == delta.changed.len() && removed.len() == delta.removed.len();
    if !complete {
        println!(
            "{}",
            "[live] asset delta could not be staged completely — falling back to rebuild".yellow()
        );
        return false;
    }
    for path in &updated {
        server.broadcast(&ServerMessage::AssetChanged {
            transfer_id: transfer_id.to_string(),
            path: path.clone(),
            asset_revision,
        });
    }
    for path in &removed {
        server.broadcast(&ServerMessage::AssetRemoved {
            transfer_id: transfer_id.to_string(),
            path: path.clone(),
            asset_revision,
        });
    }
    if updated.is_empty() && removed.is_empty() {
        println!(
            "{}",
            "[live] no asset reached the app — the change applies on the next rebuild".yellow()
        );
        return true;
    }
    server.broadcast(&ServerMessage::AssetsCommit {
        transfer_id: transfer_id.to_string(),
        asset_revision,
    });
    if server.take_write_error() {
        println!(
            "{}",
            "[live] the dev channel dropped mid-reload — save again or press 'r' to rebuild"
                .yellow()
        );
    } else {
        println!(
            "{}",
            format!(
                "[live] sent asset update(s): {}{}",
                updated.join(", "),
                if removed.is_empty() {
                    String::new()
                } else {
                    format!("; removed: {}", removed.join(", "))
                }
            )
            .green()
        );
    }
    true
}

fn valid_asset_path(path: &str) -> bool {
    let path = Path::new(path);
    path.components()
        .next()
        .is_some_and(|component| component.as_os_str() == std::ffi::OsStr::new(ASSETS_DIR))
        && path.components().all(|component| {
            !matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
}

/// Applies a reconnect-time manifest reconciliation to one app connection.
/// Only paths declared by the current manifest are sent as content; removed
/// paths are validated as asset-relative names before a delete is forwarded.
fn apply_asset_reconciliation(
    project: &Project,
    plan: &Plan,
    server: &DevServer,
    reconciliation: &AssetReconciliation,
    manifest: &BTreeMap<String, String>,
    device_lease: Option<&DeviceLeaseSession>,
) -> bool {
    let mut changed = reconciliation.missing.clone();
    changed.extend(reconciliation.stale.iter().cloned());
    changed.sort();
    changed.dedup();
    if changed
        .iter()
        .any(|path| !valid_asset_path(path) || !manifest.contains_key(path))
        || reconciliation
            .removed
            .iter()
            .any(|path| !valid_asset_path(path))
    {
        return false;
    }

    let revision = reconciliation.asset_revision;
    let connection_id = reconciliation.connection_id;
    if changed.is_empty() && reconciliation.removed.is_empty() {
        return true;
    }
    let entries = changed
        .iter()
        .filter_map(|path| {
            manifest.get(path).map(|hash| AssetManifestEntry {
                path: path.clone(),
                hash: hash.clone(),
            })
        })
        .collect::<Vec<_>>();
    if entries.len() != changed.len()
        || !server.send_to(
            connection_id,
            &ServerMessage::AssetsBegin {
                transfer_id: reconciliation.transfer_id.clone(),
                asset_revision: revision,
                entries,
                removed: reconciliation.removed.clone(),
            },
        )
    {
        return false;
    }
    let updated = match plan {
        Plan::Desktop => changed
            .iter()
            .filter(|path| {
                server.send_to(
                    connection_id,
                    &ServerMessage::AssetChanged {
                        transfer_id: reconciliation.transfer_id.clone(),
                        path: (*path).clone(),
                        asset_revision: revision,
                    },
                )
            })
            .count(),
        Plan::Ios {
            physical: false, ..
        } => push_ios_assets_to(
            project,
            changed.clone(),
            server,
            Some(connection_id),
            &reconciliation.transfer_id,
            revision,
            device_lease,
        )
        .len(),
        Plan::Android { serial, .. } => {
            let package = bundle_id_of_android(project);
            let pushed = push_android_assets(project, serial, &package, &changed, device_lease);
            for path in &pushed {
                let _ = server.send_to(
                    connection_id,
                    &ServerMessage::AssetChanged {
                        transfer_id: reconciliation.transfer_id.clone(),
                        path: path.clone(),
                        asset_revision: revision,
                    },
                );
            }
            pushed.len()
        }
        Plan::Ios { physical: true, .. } => return false,
    };
    if updated != changed.len() {
        return false;
    }

    let removed = match plan {
        Plan::Android { serial, .. } => {
            let package = bundle_id_of_android(project);
            let removed =
                remove_android_assets(serial, &package, &reconciliation.removed, device_lease);
            removed
                .iter()
                .filter(|path| {
                    server.send_to(
                        connection_id,
                        &ServerMessage::AssetRemoved {
                            transfer_id: reconciliation.transfer_id.clone(),
                            path: (*path).clone(),
                            asset_revision: revision,
                        },
                    )
                })
                .count()
        }
        _ => reconciliation
            .removed
            .iter()
            .filter(|path| {
                server.send_to(
                    connection_id,
                    &ServerMessage::AssetRemoved {
                        transfer_id: reconciliation.transfer_id.clone(),
                        path: (*path).clone(),
                        asset_revision: revision,
                    },
                )
            })
            .count(),
    };
    removed == reconciliation.removed.len()
        && server.send_to(
            connection_id,
            &ServerMessage::AssetsCommit {
                transfer_id: reconciliation.transfer_id.clone(),
                asset_revision: revision,
            },
        )
}

fn drain_asset_reconciliations(
    project: &Project,
    plan: &Plan,
    server: &DevServer,
    session: &Arc<Session>,
    device_lease: Option<&DeviceLeaseSession>,
) -> bool {
    let reconciliations = server.take_asset_reconciliations();
    if reconciliations.is_empty() {
        return false;
    }
    let manifest: BTreeMap<String, String> = session
        .asset_manifest()
        .into_iter()
        .map(|entry| (entry.path, entry.hash))
        .collect();
    let mut rebuild = false;
    for reconciliation in reconciliations {
        if reconciliation.asset_revision != session.store.state().desired.asset_revision {
            continue;
        }
        let applied = apply_asset_reconciliation(
            project,
            plan,
            server,
            &reconciliation,
            &manifest,
            device_lease,
        );
        session.emit(
            Kind::AssetsSent,
            &reconciliation.scope,
            json!({
                "transfer_id": reconciliation.transfer_id,
                "changed": reconciliation.missing.iter().chain(reconciliation.stale.iter()).collect::<Vec<_>>(),
                "removed": reconciliation.removed,
                "present": reconciliation.present,
                "requested_asset_revision": reconciliation.asset_revision,
                "handled_without_build": applied,
                "render_confirmed": false,
                "reconciled": true,
                "connection_id": reconciliation.connection_id,
            }),
        );
        if !applied {
            rebuild = true;
            println!(
                "{}",
                "[live] asset reconciliation could not be completed — rebuilding".yellow()
            );
        }
    }
    rebuild
}

/// Sets up the launch plan for the chosen target (device selection up front,
/// reused across iterations).
fn resolve_plan(project: &Project, target: &str, flags: &DeviceFlags) -> Result<Plan> {
    match target.to_ascii_lowercase().as_str() {
        "desktop" | "macos" | "windows" | "linux" => {
            if !project.has_desktop() {
                bail!("This project has no desktop target. Add one with `gpui init --add`.");
            }
            Ok(Plan::Desktop)
        }
        "ios" => {
            if !project.ios_dir().exists() {
                bail!("This project has no iOS target. Add one with `gpui init --add`.");
            }
            let target = resolve_ios_target(project, flags)?;
            Ok(match target {
                IosTarget::Simulator(device) => {
                    let ready = inventory::ensure_running(device)?;
                    Plan::Ios {
                        physical: false,
                        id: ready.id.clone(),
                        label: ready.label(),
                    }
                }
                IosTarget::Physical(device) => Plan::Ios {
                    physical: true,
                    id: device.id.clone(),
                    label: device.label(),
                },
            })
        }
        "android" => {
            if !project.android_gradle_dir().exists() {
                bail!("This project has no Android target. Add one with `gpui init --add`.");
            }
            let chosen = inventory::resolve_device(
                device::Platform::Android,
                flags,
                &project.defaults,
                None,
            )?;
            let ready = inventory::ensure_running(chosen)?;
            let serial = ready.serial().context(
                "The selected Android device has no adb serial; re-run `gpui device list` to check it.",
            )?;
            Ok(Plan::Android {
                serial: serial.to_string(),
                label: ready.label(),
            })
        }
        other => bail!("Unknown target '{other}'. Valid targets: desktop, ios, android."),
    }
}

/// Runs one non-watching preview launch. Preview intentionally starts without
/// a Live snapshot and keeps the process attached until the user interrupts
/// it, so the runtime's `scenario_ready` event belongs to one isolated run.
pub fn handle_preview(
    project: &Project,
    target: &str,
    preview: PreviewLaunch,
    flags: &DeviceFlags,
) -> Result<()> {
    let plan = resolve_plan(project, target, flags)?;
    let preleased = std::env::var_os("GPUI_PREVIEW_PRELEASED").is_some();
    let device_lease = if preleased {
        None
    } else {
        match &plan {
            Plan::Desktop => None,
            Plan::Ios { id, .. } => Some(
                DeviceLeaseSession::acquire(&project.root, id)
                    .context("acquiring the iOS preview device lease")?,
            ),
            Plan::Android { serial, .. } => Some(
                DeviceLeaseSession::acquire(&project.root, serial)
                    .context("acquiring the Android preview device lease")?,
            ),
        }
    };
    let target_id = match &plan {
        Plan::Desktop => format!("desktop:{}", std::env::consts::OS),
        Plan::Ios { physical, id, .. } => format!(
            "ios-{}:{id}",
            if *physical { "device" } else { "simulator" }
        ),
        Plan::Android { serial, .. } => format!("android:{serial}"),
    };
    let target_id = preview_target_id(target_id);
    let session = Session::start(&project.root, &project.name, &target_id)?;
    struct EndSession(Arc<Session>);
    impl Drop for EndSession {
        fn drop(&mut self) {
            self.0.end();
        }
    }
    let _end_session = EndSession(session.clone());
    let _control = ControlServer::start(session.clone())?;
    let server = DevServer::start_observed(session.clone())?;
    if !server.set_asset_manifest(
        session.store.state().desired.asset_revision,
        session.asset_manifest(),
    ) {
        bail!("asset manifest exceeds the dev-channel frame limit");
    }
    let channel_dir = project.root.join(".gpui");
    fs::create_dir_all(&channel_dir)?;
    fs::write(channel_dir.join("dev-port"), server.port.to_string())?;
    let overflow = Arc::new(AtomicBool::new(false));
    let mut channel = Channel {
        live: session.clone(),
        scope: Scope::default(),
        overflow,
        project: project.name.clone(),
        port: server.port,
        token: server.token.clone(),
        assets_dir: project.root.join(ASSETS_DIR),
        session: None,
        preview: Some(preview),
    };
    let interrupt = Arc::downgrade(&session);
    ctrlc::set_handler(move || {
        if let Some(session) = interrupt.upgrade() {
            session.stopping.store(true, Ordering::SeqCst);
        }
    })
    .context("installing the preview shutdown handler")?;

    let mut event_seq = session.store.state().seq;
    let expected_scenario = channel
        .preview
        .as_ref()
        .map(|preview| preview.scenario_id.clone())
        .unwrap_or_default();
    let build = session.begin_build()?;
    let mut child = None;
    let iteration = run_iteration(
        project,
        &plan,
        &mut channel,
        &server,
        &mut child,
        &build,
        device_lease.as_ref(),
    );
    match iteration {
        Ok(Iteration::Rebuilt) => build.finish(true, None),
        Ok(Iteration::BuildFailed) => {
            build.finish(false, None);
            bail!("preview build failed")
        }
        Ok(Iteration::Superseded) => {
            build.superseded();
            bail!("preview inputs changed while building; rerun the preview")
        }
        Err(error) => {
            build.finish(false, Some(format!("{error:#}")));
            return Err(error);
        }
    }

    let ready_deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    while !ready && !session.stopping.load(Ordering::SeqCst) {
        let page = session.store.events(event_seq, Duration::from_millis(100));
        event_seq = page.next_seq;
        for event in page.events {
            match event.kind {
                Kind::ScenarioReady
                    if event.data["scenario_id"].as_str() == Some(expected_scenario.as_str()) =>
                {
                    ready = true;
                    println!(
                        "Preview ready for `{}` (reset_generation={}). Press Ctrl-C to stop.",
                        expected_scenario,
                        event.data["reset_generation"].as_u64().unwrap_or(0)
                    );
                }
                Kind::AppExited | Kind::AppLaunchFailed => {
                    bail!("preview app exited before scenario_ready")
                }
                _ => {}
            }
        }
        if Instant::now() >= ready_deadline {
            bail!("preview runtime did not emit scenario_ready within 30 seconds")
        }
    }
    while !session.stopping.load(Ordering::SeqCst) {
        session.advance_observe_requests();
        if session
            .store
            .state()
            .running
            .as_ref()
            .is_some_and(|run| matches!(run.process.as_str(), "exited" | "launch_failed"))
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    drop(child);
    Ok(())
}

fn preview_target_id(target_id: String) -> String {
    let Some(key) = std::env::var_os("GPUI_PREVIEW_SESSION_KEY") else {
        return target_id;
    };
    let key = safe_preview_component(&key.to_string_lossy());
    format!("{target_id}::gpui-check:{key}")
}

fn safe_preview_component(value: &str) -> String {
    let mut result = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                byte as char
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() {
        result.push('_');
    }
    result.truncate(128);
    result
}

pub fn handle_live(project: &Project, target: &str, flags: &DeviceFlags) -> Result<()> {
    let plan = resolve_plan(project, target, flags)?;
    let device_lease = match &plan {
        Plan::Desktop => None,
        Plan::Ios { id, .. } => Some(
            DeviceLeaseSession::acquire(&project.root, id)
                .context("acquiring the iOS live device lease")?,
        ),
        Plan::Android { serial, .. } => Some(
            DeviceLeaseSession::acquire(&project.root, serial)
                .context("acquiring the Android live device lease")?,
        ),
    };
    let target_id = match &plan {
        Plan::Desktop => format!("desktop:{}", std::env::consts::OS),
        Plan::Ios { physical, id, .. } => format!(
            "ios-{}:{id}",
            if *physical { "device" } else { "simulator" }
        ),
        Plan::Android { serial, .. } => format!("android:{serial}"),
    };
    let session = Session::start(&project.root, &project.name, &target_id)?;
    struct EndSession(Arc<Session>);
    impl Drop for EndSession {
        fn drop(&mut self) {
            self.0.end();
        }
    }
    let _end_session = EndSession(session.clone());
    let _control = ControlServer::start(session.clone())?;
    let server = DevServer::start_observed(session.clone())?;
    if !server.set_asset_manifest(
        session.store.state().desired.asset_revision,
        session.asset_manifest(),
    ) {
        bail!("asset manifest exceeds the dev-channel frame limit");
    }
    let channel_dir = project.root.join(".gpui");
    fs::write(channel_dir.join("dev-port"), server.port.to_string())?;
    let overflow = Arc::new(AtomicBool::new(false));
    let mut channel = Channel {
        live: session.clone(),
        scope: Scope::default(),
        overflow: overflow.clone(),
        project: project.name.clone(),
        port: server.port,
        token: server.token.clone(),
        assets_dir: project.root.join(ASSETS_DIR),
        session: None,
        preview: None,
    };
    let (tx, rx) = mpsc::sync_channel::<Event>(64);
    let interrupt = Arc::downgrade(&session);
    let interrupt_tx = tx.clone();
    ctrlc::set_handler(move || {
        if let Some(session) = interrupt.upgrade() {
            session.stopping.store(true, Ordering::SeqCst);
        }
        let _ = interrupt_tx.try_send(Event::Quit);
    })
    .context("installing the live shutdown handler")?;

    let keys_tx = tx.clone();
    let keys_session = Arc::downgrade(&session);
    let keys_overflow = overflow.clone();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let event = match line.trim() {
                "r" | "R" => Event::Force,
                "q" | "Q" | "quit" | "exit" => {
                    if let Some(session) = keys_session.upgrade() {
                        session.stopping.store(true, Ordering::SeqCst);
                    }
                    let _ = keys_tx.try_send(Event::Quit);
                    break;
                }
                _ => continue,
            };
            if keys_tx.try_send(event).is_err() {
                keys_overflow.store(true, Ordering::SeqCst);
            }
        }
    });

    let watch_root = session.root.clone();
    let watch_session = session.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        None,
        move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    watch_session.emit(
                        Kind::WatchError,
                        &Scope::default(),
                        json!({"message": format!("{errors:?}")}),
                    );
                    return;
                }
            };
            let mut saw_asset_path = false;
            let mut code_change = false;
            for event in events {
                if matches!(event.kind, EventKind::Access(_)) {
                    continue;
                }
                for path in &event.paths {
                    let Ok(relative) = path.strip_prefix(&watch_root) else {
                        continue;
                    };
                    if !should_trigger(relative) {
                        continue;
                    }
                    if asset_rel_path(&watch_root, path).is_some() {
                        saw_asset_path = true;
                    } else {
                        code_change = true;
                    }
                }
            }
            if !code_change && !saw_asset_path {
                return;
            }
            let previous = watch_session.store.state().desired;
            let delta = match watch_session.sync_inputs_with_delta() {
                Ok((revision, _)) if revision == previous => return,
                Ok((_, delta)) => delta,
                Err(error) => {
                    watch_session.emit(
                        Kind::WatchError,
                        &Scope::default(),
                        json!({"message": format!("{error:#}")}),
                    );
                    return;
                }
            };
            let event = if code_change {
                Event::Change
            } else {
                Event::Assets(delta)
            };
            if tx.try_send(event).is_err() {
                overflow.store(true, Ordering::SeqCst);
            }
        },
    )?;
    debouncer
        .watch(&session.root, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", session.root.display()))?;
    println!(
        "\n[live] watching {} (debug builds)",
        session.root.display()
    );
    println!(
        "[live] session {} — query with `gpui dev status --json`",
        session.id
    );
    println!("[live] type 'r' + Enter to force rebuild, 'q' + Enter to quit\n");
    let mut child: Option<AppProcess> = None;
    let mut last_failed = false;
    let mut quit = run_cycles(
        project,
        &plan,
        &mut channel,
        &server,
        &mut child,
        &rx,
        &mut last_failed,
        device_lease.as_ref(),
    )?;
    while !quit && !session.stopping.load(Ordering::SeqCst) {
        if let Some(lease) = device_lease.as_ref() {
            lease
                .assert_owned()
                .map_err(anyhow::Error::new)
                .context("mobile device lease is no longer owned")?;
        }
        session.advance_observe_requests();
        if session.take_build_request().is_some() {
            quit = run_cycles(
                project,
                &plan,
                &mut channel,
                &server,
                &mut child,
                &rx,
                &mut last_failed,
                device_lease.as_ref(),
            )?;
            continue;
        }
        if drain_asset_reconciliations(project, &plan, &server, &session, device_lease.as_ref()) {
            quit = run_cycles(
                project,
                &plan,
                &mut channel,
                &server,
                &mut child,
                &rx,
                &mut last_failed,
                device_lease.as_ref(),
            )?;
            continue;
        }
        let event = rx.recv_timeout(Duration::from_millis(100));
        if channel.overflow.swap(false, Ordering::SeqCst) {
            quit = run_cycles(
                project,
                &plan,
                &mut channel,
                &server,
                &mut child,
                &rx,
                &mut last_failed,
                device_lease.as_ref(),
            )?;
            continue;
        }
        match event {
            Ok(Event::Quit) => break,
            Ok(Event::Change) | Ok(Event::Force) => {
                quit = run_cycles(
                    project,
                    &plan,
                    &mut channel,
                    &server,
                    &mut child,
                    &rx,
                    &mut last_failed,
                    device_lease.as_ref(),
                )?;
            }
            Ok(Event::Assets(delta)) => {
                if !server.set_asset_manifest(
                    session.store.state().desired.asset_revision,
                    session.asset_manifest(),
                ) {
                    quit = run_cycles(
                        project,
                        &plan,
                        &mut channel,
                        &server,
                        &mut child,
                        &rx,
                        &mut last_failed,
                        device_lease.as_ref(),
                    )?;
                    continue;
                }
                let Some((transfer_id, _, _)) = server.current_asset_manifest() else {
                    quit = run_cycles(
                        project,
                        &plan,
                        &mut channel,
                        &server,
                        &mut child,
                        &rx,
                        &mut last_failed,
                        device_lease.as_ref(),
                    )?;
                    continue;
                };
                let scope = session.current_run().unwrap_or_default();
                let asset_span =
                    session.start_span("assets.apply", &scope, None, json!({"delta": delta}));
                let sent = reload_assets(
                    project,
                    &plan,
                    &server,
                    &delta,
                    &transfer_id,
                    session.store.state().desired.asset_revision,
                    device_lease.as_ref(),
                );
                asset_span.finish(if sent { "sent" } else { "fallback" }, None);
                session.emit(
                    Kind::AssetsSent,
                    &scope,
                    json!({"transfer_id": transfer_id, "changed": delta.changed, "removed": delta.removed,
                    "requested_asset_revision": session.store.state().desired.asset_revision,
                    "handled_without_build": sent, "render_confirmed": false}),
                );
                if !sent {
                    quit = run_cycles(
                        project,
                        &plan,
                        &mut channel,
                        &server,
                        &mut child,
                        &rx,
                        &mut last_failed,
                        device_lease.as_ref(),
                    )?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(debouncer);
    drop(child);
    server.shutdown();
    session.end();
    if let Some(lease) = device_lease {
        lease
            .release()
            .map_err(anyhow::Error::new)
            .context("releasing the mobile live device lease")?;
    }
    println!("{}", "\n[live] stopped".dimmed());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::build_manifest::{
        BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION, BuildArtifactFile,
    };
    use crate::runner::output_layout::BuildPlatform;

    #[test]
    fn ignores_build_output_and_generated_dirs() {
        let root = std::path::Path::new("/proj");
        assert!(should_trigger(&root.join("crates/app/src/lib.rs")));
        assert!(should_trigger(&root.join("Cargo.toml")));
        assert!(should_trigger(&root.join("gpui.toml")));
        assert!(should_trigger(&root.join(
            "mobile/android/gradle/app/src/main/java/dev/gpui/mobile/GpuiActivity.kt"
        )));
        assert!(should_trigger(&root.join("mobile/ios/App.swift")));
        assert!(!should_trigger(&root.join("target/debug/app")));
        assert!(!should_trigger(
            &root.join("mobile/ios/build/Build/Products/app")
        ));
        assert!(!should_trigger(&root.join(
            "mobile/android/gradle/app/build/outputs/apk/app-debug.apk"
        )));
        assert!(!should_trigger(&root.join(
            "mobile/android/gradle/app/src/main/jniLibs/arm64-v8a/libapp.so"
        )));
        assert!(!should_trigger(
            &root.join("mobile/ios/App.xcodeproj/project.pbxproj")
        ));
        assert!(!should_trigger(&root.join(".git/index")));
    }

    #[test]
    fn oversized_assets_are_rejected_before_encoding() {
        // ~800 KiB raw base64s past the 1 MiB frame cap (the audit repro).
        assert!(!ios_frame_fits(800 * 1024, "assets/logo.png"));
        // Everyday images fit comfortably.
        assert!(ios_frame_fits(256 * 1024, "assets/logo.png"));
        assert!(ios_frame_fits(0, "assets/logo.png"));
    }

    #[test]
    fn classifies_asset_paths_under_the_assets_dir() {
        let root = std::path::Path::new("/proj");
        assert_eq!(
            asset_rel_path(root, &root.join("assets/logo.png")).as_deref(),
            Some("assets/logo.png")
        );
        assert_eq!(
            asset_rel_path(root, &root.join("assets/icons/x.svg")).as_deref(),
            Some("assets/icons/x.svg")
        );
        assert_eq!(
            asset_rel_path(root, &root.join("crates/app/src/lib.rs")),
            None
        );
        assert_eq!(asset_rel_path(root, &root.join("assets_mine/x.png")), None);
        assert_eq!(asset_rel_path(root, &root.join("other/assets/x.png")), None);
    }

    #[test]
    fn preview_cache_only_selects_one_verified_desktop_executable() {
        let root = tempfile::tempdir().unwrap();
        let manifest = BuildArtifactManifest {
            schema_version: BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            platform: BuildPlatform::Desktop,
            key_hash: "a".repeat(64),
            roots: vec!["cargo-target/debug/app".into()],
            files: vec![BuildArtifactFile {
                path: "cargo-target/debug/app".into(),
                size: 4,
                sha256: "0".repeat(64),
                executable: true,
            }],
        };
        assert_eq!(
            preview_desktop_executable_from_manifest(&manifest, root.path()),
            Some(root.path().join("cargo-target/debug/app"))
        );

        let mut ambiguous = manifest.clone();
        ambiguous.files.push(BuildArtifactFile {
            path: "cargo-target/debug/other".into(),
            size: 4,
            sha256: "0".repeat(64),
            executable: true,
        });
        assert_eq!(
            preview_desktop_executable_from_manifest(&ambiguous, root.path()),
            None
        );
    }

    #[test]
    fn preview_manifest_matches_the_expected_ios_bundle_root() {
        let root = tempfile::tempdir().unwrap();
        let app = root
            .path()
            .join("native-staging/ios/derived-data/Build/Products/Debug-iphonesimulator/Demo.app");
        let manifest = BuildArtifactManifest {
            schema_version: BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            platform: BuildPlatform::Ios,
            key_hash: "b".repeat(64),
            roots: vec![
                "native-staging/ios/derived-data/Build/Products/Debug-iphonesimulator/Demo.app"
                    .into(),
            ],
            files: Vec::new(),
        };
        assert!(preview_manifest_contains_root(&manifest, root.path(), &app));
        assert!(!preview_manifest_contains_root(
            &manifest,
            root.path(),
            &root.path().join("other.app")
        ));
    }

    #[test]
    fn ios_preview_coordinator_verifies_the_complete_expected_bundle() {
        let root = tempfile::tempdir().unwrap();
        let key_hash = "e".repeat(64);
        let app = root.path().join("derived-data/Demo.app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("Info.plist"), b"bundle metadata").unwrap();
        publish_preview_ios_manifest(root.path(), &key_hash, &app).unwrap();

        let layout = BuildOutputLayout {
            platform: BuildPlatform::Ios,
            key_hash: key_hash.clone(),
            root: root.path().to_path_buf(),
            cargo_target_dir: root.path().join("cargo-target"),
            native_staging_dir: root.path().join("native-staging"),
            android_jni_dir: None,
            android_gradle_build_dir: None,
            ios_derived_data_dir: Some(root.path().join("derived-data")),
        };
        verify_preview_ios_output(&layout, &key_hash, &app).unwrap();
        assert!(
            verify_preview_ios_output(&layout, &key_hash, &root.path().join("other.app")).is_err()
        );

        fs::write(app.join("Info.plist"), b"tampered metadata").unwrap();
        assert!(verify_preview_ios_output(&layout, &key_hash, &app).is_err());
    }

    #[test]
    fn preview_manifest_matches_the_expected_android_output_roots() {
        let root = tempfile::tempdir().unwrap();
        let jni = root
            .path()
            .join("native-staging/android/jni-libs/arm64-v8a");
        let apk_output = root.path().join("gradle-build/outputs/apk/debug");
        let mut roots = vec![
            "native-staging/android/jni-libs/arm64-v8a".to_string(),
            "gradle-build/outputs/apk/debug".to_string(),
        ];
        roots.sort();
        let manifest = BuildArtifactManifest {
            schema_version: BUILD_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            platform: BuildPlatform::Android,
            key_hash: "c".repeat(64),
            roots,
            files: Vec::new(),
        };
        assert!(preview_manifest_contains_roots(
            &manifest,
            root.path(),
            &[jni.as_path(), apk_output.as_path()]
        ));
        let other_apk_output = root.path().join("other-apk");
        assert!(!preview_manifest_contains_roots(
            &manifest,
            root.path(),
            &[jni.as_path(), other_apk_output.as_path()]
        ));
    }

    #[test]
    fn publish_preview_android_manifest_captures_jni_and_apk_outputs() {
        let root = tempfile::tempdir().unwrap();
        let jni = root
            .path()
            .join("native-staging/android/jni-libs/arm64-v8a");
        fs::create_dir_all(&jni).unwrap();
        fs::write(jni.join("libdemo.so"), b"jni").unwrap();
        let apk_output = root.path().join("gradle-build/outputs/apk/debug");
        fs::create_dir_all(&apk_output).unwrap();
        let apk = apk_output.join("app-debug.apk");
        fs::write(&apk, b"apk").unwrap();
        fs::write(
            apk_output.join("output-metadata.json"),
            br#"{"elements":[{"outputFile":"app-debug.apk"}]}"#,
        )
        .unwrap();

        publish_preview_android_manifest(root.path(), &"d".repeat(64), &jni, &apk).unwrap();
        let manifest =
            BuildArtifactManifest::read(&root.path().join(PREVIEW_BUILD_ARTIFACT_MANIFEST_FILE))
                .unwrap();
        assert_eq!(manifest.platform, BuildPlatform::Android);
        assert_eq!(manifest.files.len(), 3);
        manifest.verify(root.path()).unwrap();
    }
}
