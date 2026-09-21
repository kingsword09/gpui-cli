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
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::error;
use super::run::{
    IosTarget, Project, android_abis, android_rust_target, apk_path, bundle_id_of,
    bundle_id_of_android, check_android_libraries, ensure_tool, gradle_command, gradle_task,
    resolve_ios_target, xcode_app_path, xcode_destination,
};
use crate::device::{self, DeviceFlags, android, inventory, ios};
use crate::devserver::DevServer;
use crate::devserver::control::ControlServer;
use crate::devserver::events::{Kind, Scope};
use crate::devserver::inputs::should_trigger;
use crate::devserver::output::{self, AppProcess};
use crate::devserver::protocol::{self, ServerMessage};
use crate::devserver::session::{Build, Session};
use serde_json::json;

/// Source files watch out for asset-only changes under this directory; they
/// can reload in the running app instead of triggering a rebuild.
const ASSETS_DIR: &str = "assets";
/// How long the live loop waits for the app to hand over its snapshot.
const SNAPSHOT_WAIT: Duration = Duration::from_millis(1500);
/// Snapshots above this size are rejected (the frame cap is 1 MiB).
const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
/// Snapshots older than this are pruned on the next save.
const SNAPSHOT_TTL: Duration = Duration::from_secs(24 * 3600);

/// Editors fire several events per save; this absorbs the burst.
const DEBOUNCE_MS: u64 = 400;

enum Event {
    Change,
    /// Asset-only changes under `assets/`, as slash-separated paths relative
    /// to the project root.
    Assets(Vec<String>),
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
        env
    }

    /// File contents for platforms with no environment to inherit (Android).
    fn device_config(&self) -> String {
        let session = self.session.as_deref().unwrap_or_default();
        format!(
            "project={}\naddr={}\ntoken={}\nsession={session}\nbuild_id={}\nrun_id={}\n",
            self.project,
            self.addr(),
            self.token,
            self.scope.build_id.as_deref().unwrap_or_default(),
            self.scope.run_id.as_deref().unwrap_or_default(),
        )
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

/// Runs one build + (re)launch cycle. `Ok(BuildFailed)` means a compile failure
/// was already rendered; infrastructure errors come back as `Err`.
fn run_iteration(
    project: &Project,
    plan: &Plan,
    channel: &mut Channel,
    server: &DevServer,
    child: &mut Option<AppProcess>,
    build: &Build,
) -> Result<Iteration> {
    match plan {
        Plan::Desktop => {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(&project.root)
                .args(["build", "-p", &project.desktop_crate()]);
            let outcome = error::run_cargo_json(&mut cmd, build, "cargo.build")?;
            if !outcome.success {
                return Ok(Iteration::BuildFailed);
            }
            let executable = outcome
                .executable
                .context("cargo succeeded but reported no binary path")?;
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
            let Some(app) = build_ios_app_live(project, *physical, id, build)? else {
                return Ok(Iteration::BuildFailed);
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
            observed_step(build, "ios.install_launch", || {
                if *physical {
                    ios::install_and_launch_device(id, &app, &bundle_id)
                } else {
                    ios::install_and_launch_with_env(id, &app, &bundle_id, &channel.env(project))
                }
            })?;
            channel.live.emit(
                Kind::AppStarted,
                &channel.scope,
                json!({"confirmed": false, "device": id}),
            );
            if !physical && server.wait_for_client(Duration::from_secs(30)) {
                push_ios_assets(project, all_asset_paths(project), server);
            }
            println!("{}", format!("✓ relaunched on {label}").green());
        }
        Plan::Android { serial, label } => {
            let bundle_id = bundle_id_of_android(project);
            let Some(apk) = build_android_apk_live(project, build)? else {
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
            observed_step(build, "android.install", || {
                android::install_apk(serial, &apk)
            })?;
            push_android_assets(project, serial, &bundle_id, &all_asset_paths(project));
            if let Some(session) = &channel.session {
                let state = Channel::sessions_dir(project).join(format!("{session}.state"));
                match fs::read(&state)
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| {
                        android::write_device_config(serial, &bundle_id, "gpui_state", &bytes)
                    }) {
                    Ok(()) => {}
                    Err(error) => {
                        channel.live.emit(Kind::AppLog, &channel.scope, json!({"level": "warn", "target": "restore", "message": error.to_string()}));
                        channel.session = None;
                    }
                }
            }
            observed_step(build, "android.configure", || {
                android::write_device_config(
                    serial,
                    &bundle_id,
                    "gpui_live.txt",
                    channel.device_config().as_bytes(),
                )?;
                android::force_stop(serial, &bundle_id)?;
                android::reverse_port(serial, channel.port)
            })?;
            observed_step(build, "android.launch", || {
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
    build
        .session
        .emit(Kind::StageStarted, &build.scope, json!({"stage": stage}));
    let result = f();
    build.session.emit(Kind::StageFinished, &build.scope,
        json!({"stage": stage, "success": result.is_ok(), "error": result.as_ref().err().map(|e| format!("{e:#}"))}));
    result
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
fn push_ios_assets(project: &Project, paths: Vec<String>, server: &DevServer) -> Vec<String> {
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
                server.broadcast(&ServerMessage::AssetData {
                    path: rel.clone(),
                    data: protocol::b64::encode(&bytes),
                });
                pushed.push(rel);
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
) -> Vec<String> {
    let mut pushed = Vec::new();
    for rel in paths {
        let source = project.root.join(rel);
        match fs::read(&source) {
            Ok(bytes) => {
                if let Err(err) = android::write_device_config(serial, package, rel, &bytes) {
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

/// iOS build with streaming Cargo diagnostics and Xcode output.
/// `Ok(None)` means the Rust build failed.
fn build_ios_app_live(
    project: &Project,
    physical: bool,
    udid: &str,
    build: &Build,
) -> Result<Option<std::path::PathBuf>> {
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
    ]);
    let outcome = error::run_cargo_json(&mut cargo, build, "cargo.build")?;
    if !outcome.success {
        println!(
            "{}",
            "✗ build failed — keeping the current app running".yellow()
        );
        return Ok(None);
    }

    ensure_tool("xcodegen", "Install it with `brew install xcodegen`.")?;
    let ios_dir = project.ios_dir();
    run_tool(
        "xcodegen generate",
        Command::new("xcodegen")
            .current_dir(&ios_dir)
            .args(["generate", "--spec", "project.yml"]),
        build,
    )?;

    let scheme = project.xcode_target();
    let derived_dir = ios_dir.join("build");

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

    let app_path = xcode_app_path(&derived_dir, &scheme, physical, false);
    if !app_path.exists() {
        bail!(
            "xcodebuild finished but no app bundle was found at '{}'.",
            app_path.display()
        );
    }
    println!("  {} {}", "✓".green(), app_path.display());
    Ok(Some(app_path))
}

/// Android build with Cargo JSON diagnostics and streaming Gradle output.
/// `Ok(None)` means the Rust build failed.
fn build_android_apk_live(project: &Project, build: &Build) -> Result<Option<std::path::PathBuf>> {
    ensure_tool(
        "cargo-ndk",
        "Install it with `cargo install cargo-ndk`, then set ANDROID_NDK_HOME.",
    )?;
    let abis = android_abis()?;
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
    ndk.arg("-o").arg(project.android_jni_libs_dir()).args([
        "--platform",
        "31",
        "build",
        "-p",
        &project.app_crate(),
    ]);
    if !error::run_cargo_json(&mut ndk, build, "cargo.ndk")?.success {
        return Ok(None);
    }

    check_android_libraries(project, &abis)?;

    run_tool(
        &format!("gradlew {}", gradle_task(false)),
        &mut gradle_command(project, false, &abis),
        build,
    )?;

    let apk = apk_path(project, false)?;
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
fn run_cycles(
    project: &Project,
    plan: &Plan,
    channel: &mut Channel,
    server: &DevServer,
    child: &mut Option<AppProcess>,
    rx: &mpsc::Receiver<Event>,
    last_failed: &mut bool,
) -> Result<bool> {
    loop {
        if channel.live.stopping.load(Ordering::SeqCst) {
            return Ok(true);
        }
        let build = channel.live.begin_build()?;
        let mut again = false;
        let failed = match run_iteration(project, plan, channel, server, child, &build) {
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
        if quit {
            return Ok(true);
        }
        if !again {
            return Ok(false);
        }
    }
}

/// Pushes + broadcasts an asset-only change to the running app. Returns false
/// when no capable client is attached and the caller must fall back to a full
/// rebuild (the app then reads the fresh files at startup).
fn reload_assets(project: &Project, plan: &Plan, server: &DevServer, paths: &[String]) -> bool {
    if !(server.has_clients() && server.all_clients_support_asset_reload()) {
        return false;
    }
    // Only announce files the app can actually reload — announcing a push
    // that failed would make the client evict a cache entry it cannot refill.
    let pushed = match plan {
        Plan::Android { serial, .. } => {
            let package = bundle_id_of_android(project);
            push_android_assets(project, serial, &package, paths)
        }
        Plan::Ios {
            physical: false, ..
        } => push_ios_assets(project, paths.to_vec(), server),
        _ => paths.to_vec(),
    };
    for path in &pushed {
        server.broadcast(&ServerMessage::AssetChanged { path: path.clone() });
    }
    if pushed.is_empty() {
        println!(
            "{}",
            "[live] no asset reached the app — the change applies on the next rebuild".yellow()
        );
        return true;
    }
    if server.take_write_error() {
        println!(
            "{}",
            "[live] the dev channel dropped mid-reload — save again or press 'r' to rebuild"
                .yellow()
        );
    } else {
        println!(
            "{}",
            format!("[live] sent asset update(s): {}", pushed.join(", ")).green()
        );
    }
    true
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

pub fn handle_live(project: &Project, target: &str, flags: &DeviceFlags) -> Result<()> {
    let plan = resolve_plan(project, target, flags)?;
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
            let mut assets = Vec::new();
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
                    if let Some(rel) = asset_rel_path(&watch_root, path) {
                        assets.push(rel);
                    } else {
                        code_change = true;
                    }
                }
            }
            if !code_change && assets.is_empty() {
                return;
            }
            let previous = watch_session.store.state().desired;
            match watch_session.sync_inputs() {
                Ok(revision) if revision == previous => return,
                Ok(_) => {}
                Err(error) => {
                    watch_session.emit(
                        Kind::WatchError,
                        &Scope::default(),
                        json!({"message": format!("{error:#}")}),
                    );
                    return;
                }
            }
            assets.sort();
            assets.dedup();
            let event = if code_change {
                Event::Change
            } else {
                Event::Assets(assets)
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
    )?;
    while !quit && !session.stopping.load(Ordering::SeqCst) {
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
                )?;
            }
            Ok(Event::Assets(paths)) => {
                let sent = reload_assets(project, &plan, &server, &paths);
                let scope = session.current_run().unwrap_or_default();
                session.emit(
                    Kind::AssetsSent,
                    &scope,
                    json!({"paths": paths,
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
    println!("{}", "\n[live] stopped".dimmed());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
