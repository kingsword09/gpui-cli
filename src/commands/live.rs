//! `gpui run --live`: watch the project, rebuild on change, relaunch.
//!
//! The loop is a single-flight pipeline: one build/install at a time, changes
//! arriving mid-build are coalesced into one pending rebuild. A failed build
//! keeps the previously launched app running (mobile) or the previous process
//! alive (desktop); the loop just keeps watching until the fix lands.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use notify_debouncer_full::notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::error;
use super::run::{
    apk_path, android_abis, bundle_id_of, bundle_id_of_android, ensure_rust_target, ensure_tool,
    gradle_task, resolve_ios_target, xcode_app_path, xcode_destination, IosTarget, Project,
};
use crate::device::{self, android, inventory, ios, DeviceFlags};
use crate::devserver::protocol::{ClientMessage, ServerMessage};
use crate::devserver::DevServer;

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
/// How many trailing lines of a failed external command to show.
const OUTPUT_TAIL_LINES: usize = 40;

/// Directory names that hold build outputs or VCS noise. Watching them would
/// re-trigger the build that just wrote them, so they are filtered out.
const IGNORED_DIRS: &[&str] = &[
    "target",
    ".git",
    ".gpui",
    ".gradle",
    "build",
    "jniLibs",
    "node_modules",
    "Pods",
];

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
            "project={}\naddr={}\ntoken={}\nsession={session}\n",
            self.project,
            self.addr(),
            self.token
        )
    }
}

/// Directory name components that would re-trigger the build that wrote them.
fn should_trigger(path: &Path) -> bool {
    path.components().all(|component| {
        let name = component.as_os_str().to_string_lossy();
        !IGNORED_DIRS.contains(&name.as_ref()) && !name.ends_with(".xcodeproj")
    })
}

/// Slash-relative path of an asset change under `<root>/assets/`, if it is one.
fn asset_rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    if rel.components().next()?.as_os_str() != ASSETS_DIR {
        return None;
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Runs one build + (re)launch cycle. `Ok(BuildFailed)` means a compile failure
/// was already rendered; infrastructure errors come back as `Err`.
fn run_iteration(
    project: &Project,
    plan: &Plan,
    channel: &mut Channel,
    server: &DevServer,
    child: &mut Option<Child>,
) -> Result<Iteration> {
    match plan {
        Plan::Desktop => {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(&project.root)
                .args(["build", "-p", &project.desktop_crate()]);
            let outcome = error::run_cargo_json(&mut cmd)?;
            if !outcome.success {
                error::render_errors(&outcome.errors);
                println!("{}", "✗ build failed — keeping the current app running".yellow());
                return Ok(Iteration::BuildFailed);
            }
            let executable = outcome
                .executable
                .context("cargo succeeded but reported no binary path")?;
            // Snapshot goes to the still-running app before it is replaced.
            prepare_restart(project, server, channel);
            // Killing an already-exited child is a harmless no-op.
            if let Some(mut old) = child.take() {
                let _ = old.kill();
                let _ = old.wait();
            }
            let mut spawn_cmd = Command::new(&executable);
            spawn_cmd.current_dir(&project.root).stdin(Stdio::null());
            for (key, value) in channel.env(project) {
                spawn_cmd.env(key, value);
            }
            let new_child = spawn_cmd
                .spawn()
                .with_context(|| format!("failed to launch {}", executable.display()))?;
            *child = Some(new_child);
            println!("{}", "✓ restarted".green());
            Ok(Iteration::Rebuilt)
        }
        Plan::Ios {
            physical,
            id,
            label,
        } => {
            let bundle_id = bundle_id_of(project);
            let app = match build_ios_app_live(project, *physical, id)? {
                Some(app) => app,
                None => return Ok(Iteration::BuildFailed),
            };
            println!("  {} installing on {}", "→".blue(), label);
            if *physical {
                // No channel on physical devices (see DESIGN-live-mode.md §7.2);
                // launch without credentials, the app just runs detached.
                ios::install_and_launch_device(id, &app, &bundle_id)?;
            } else {
                // Snapshot goes to the still-running app before it is replaced.
                prepare_restart(project, server, channel);
                ios::install_and_launch_with_env(id, &app, &bundle_id, &channel.env(project))?;
            }
            println!("{}", format!("✓ relaunched on {label}").green());
            Ok(Iteration::Rebuilt)
        }
        Plan::Android { serial, label } => {
            let bundle_id = bundle_id_of_android(project);
            let apk = match build_android_apk_live(project)? {
                Some(apk) => apk,
                None => return Ok(Iteration::BuildFailed),
            };
            println!("  {} installing on {}", "→".blue(), label);
            android::install_apk(serial, &apk)?;
            // Snapshot goes to the still-running app before it is replaced.
            prepare_restart(project, server, channel);
            // adb reverse works on emulators and USB devices alike, so the app
            // always reaches the dev server at 127.0.0.1:<port>.
            push_android_assets(project, serial, &bundle_id, &all_asset_paths(project));
            if let Some(session) = &channel.session {
                let state = Channel::sessions_dir(project).join(format!("{session}.state"));
                match fs::read(&state) {
                    Ok(bytes) => {
                        let _ = android::write_device_config(serial, &bundle_id, "gpui_state", &bytes);
                    }
                    Err(err) => {
                        println!("{}", format!("⚠ failed to read the snapshot: {err}").yellow());
                        channel.session = None;
                    }
                }
            }
            android::write_device_config(
                serial,
                &bundle_id,
                "gpui_live.txt",
                channel.device_config().as_bytes(),
            )?;
            android::reverse_port(serial, channel.port)?;
            android::launch_app(serial, &bundle_id)?;
            println!("{}", format!("✓ relaunched on {label}").green());
            Ok(Iteration::Rebuilt)
        }
    }
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
    server.broadcast(&ServerMessage::PrepareRestart {
        session: session.clone(),
    });
    let saved = server.wait_for(SNAPSHOT_WAIT, |message| {
        matches!(
            message,
            ClientMessage::StateSaved { session: s, .. } if s == &session
        )
    });
    let data = match saved {
        Some(ClientMessage::StateSaved { data, .. }) => Some(data),
        _ => None,
    };
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
                format!("⚠ failed to write the snapshot: {err} — restarting without state").yellow()
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

/// Copies the given assets (relative to `<project>/assets`) into the app's
/// files dir on the device; failures are reported but never fatal.
fn push_android_assets(project: &Project, serial: &str, package: &str, paths: &[String]) {
    for rel in paths {
        let source = project.root.join(rel);
        match fs::read(&source) {
            Ok(bytes) => {
                if let Err(err) = android::write_device_config(serial, package, rel, &bytes) {
                    println!(
                        "{}",
                        format!("⚠ failed to sync asset {rel}: {err:#}").yellow()
                    );
                }
            }
            Err(err) => {
                println!("{}", format!("⚠ failed to read {rel}: {err}").yellow());
            }
        }
    }
}

/// iOS build for the live loop: structured cargo diagnostics, captured output
/// for xcodegen/xcodebuild (a tail on failure). `Ok(None)` = build failed.
fn build_ios_app_live(project: &Project, physical: bool, udid: &str) -> Result<Option<std::path::PathBuf>> {
    let rust_target = if physical {
        "aarch64-apple-ios"
    } else {
        "aarch64-apple-ios-sim"
    };
    ensure_rust_target(rust_target)?;

    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(&project.root)
        .args(["build", "--lib", "-p", &project.app_crate(), "--target", rust_target]);
    let outcome = error::run_cargo_json(&mut cargo)?;
    if !outcome.success {
        error::render_errors(&outcome.errors);
        println!("{}", "✗ build failed — keeping the current app running".yellow());
        return Ok(None);
    }

    ensure_tool("xcodegen", "Install it with `brew install xcodegen`.")?;
    let ios_dir = project.ios_dir();
    run_quiet(
        "xcodegen generate",
        Command::new("xcodegen")
            .current_dir(&ios_dir)
            .args(["generate", "--spec", "project.yml"]),
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
    run_quiet("xcodebuild (Debug)", &mut xcodebuild)?;

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

/// Android build for the live loop: cargo-ndk and gradle output is captured and
/// only a tail is shown on failure. `Ok(None)` = build failed.
fn build_android_apk_live(project: &Project) -> Result<Option<std::path::PathBuf>> {
    ensure_tool(
        "cargo-ndk",
        "Install it with `cargo install cargo-ndk`, then set ANDROID_NDK_HOME.",
    )?;
    ensure_rust_target("aarch64-linux-android")?;

    let abis = android_abis();
    let mut ndk = Command::new("cargo");
    ndk.current_dir(&project.root).args(["ndk"]);
    for abi in &abis {
        ndk.args(["-t", abi]);
    }
    ndk.arg("-o")
        .arg(project.android_jni_libs_dir())
        .args(["--platform", "31", "build", "-p", &project.app_crate()]);
    run_quiet(&format!("cargo ndk ({})", abis.join(", ")), &mut ndk)?;

    let expected = project
        .android_jni_libs_dir()
        .join(&abis[0])
        .join(format!("lib{}.so", project.app_lib_name()));
    if !expected.exists() {
        bail!(
            "cargo-ndk finished but '{}' is missing. Check the `[lib] name` in crates/app/Cargo.toml.",
            expected.display()
        );
    }

    run_quiet(
        &format!("gradlew {}", gradle_task(false)),
        Command::new("./gradlew")
            .current_dir(project.android_gradle_dir())
            .arg(gradle_task(false)),
    )?;

    let apk = apk_path(project, false);
    if !apk.exists() {
        bail!("Gradle finished but no APK was found at '{}'.", apk.display());
    }
    println!("  {} {}", "✓".green(), apk.display());
    Ok(Some(apk))
}

/// Runs an external command with its output captured; on failure prints a tail
/// instead of streaming everything.
fn run_quiet(label: &str, cmd: &mut Command) -> Result<()> {
    println!("  {} {}", "→".blue(), label);
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn: {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(OUTPUT_TAIL_LINES);
    println!(
        "  {} {label} failed; last {} line(s):",
        "✗".red(),
        lines.len() - start
    );
    for line in &lines[start..] {
        println!("    {line}");
    }
    bail!("{label} failed");
}

/// One build cycle plus coalescing of everything that arrived during it.
/// Returns `true` when the user asked to quit.
fn run_cycles(
    project: &Project,
    plan: &Plan,
    channel: &mut Channel,
    server: &DevServer,
    child: &mut Option<Child>,
    rx: &mpsc::Receiver<Event>,
    last_failed: &mut bool,
) -> Result<bool> {
    loop {
        let failed = match run_iteration(project, plan, channel, server, child) {
            Ok(Iteration::Rebuilt) => {
                if *last_failed {
                    println!("{}", "✓ build recovered".green());
                }
                false
            }
            Ok(Iteration::BuildFailed) => true,
            Err(err) => {
                println!("{}", format!("✗ {err:#}").red());
                true
            }
        };
        *last_failed = failed;

        let mut again = false;
        let mut quit = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Quit => quit = true,
                Event::Change | Event::Force => again = true,
                Event::Assets(paths) => {
                    // The relaunch above already picked up fresh assets when a
                    // rebuild happened; only fall back to another rebuild when
                    // no capable client can hot-reload them.
                    if !reload_assets(project, plan, server, &paths) {
                        again = true;
                    }
                }
            }
        }
        if quit {
            return Ok(true);
        }
        if !again {
            return Ok(false);
        }
        // Changes landed while building: fall through and rebuild once more.
    }
}

/// Pushes + broadcasts an asset-only change to the running app. Returns false
/// when no capable client is attached and the caller must fall back to a full
/// rebuild (the app then reads the fresh files at startup).
fn reload_assets(project: &Project, plan: &Plan, server: &DevServer, paths: &[String]) -> bool {
    if !(server.has_clients() && server.all_clients_support_asset_reload()) {
        return false;
    }
    if let Plan::Android { serial, .. } = plan {
        let package = bundle_id_of_android(project);
        push_android_assets(project, serial, &package, paths);
    }
    for path in paths {
        server.broadcast(&ServerMessage::AssetChanged {
            path: path.clone(),
        });
    }
    println!(
        "{}",
        format!("[live] reloaded asset(s): {}", paths.join(", ")).green()
    );
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

    // Dev channel: a loopback server apps connect back to, for logs, panics
    // and (in later phases) asset reloads and snapshot hand-off.
    let server = DevServer::start().context("failed to start the live dev server")?;
    let channel_dir = project.root.join(".gpui");
    fs::create_dir_all(&channel_dir)?;
    fs::write(channel_dir.join("dev-port"), server.port.to_string())?;
    fs::write(channel_dir.join("dev-token"), &server.token)?;
    let mut channel = Channel {
        project: project.name.clone(),
        port: server.port,
        token: server.token.clone(),
        assets_dir: project.root.join(ASSETS_DIR),
        session: None,
    };

    let (tx, rx) = mpsc::channel::<Event>();

    // Key commands come in as lines: the terminal buffers input until Enter,
    // so 'r' and 'q' are documented as "press + Enter" rather than raw keys.
    let keys_tx = tx.clone();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match line.trim() {
                "r" | "R" => {
                    if keys_tx.send(Event::Force).is_err() {
                        break;
                    }
                }
                "q" | "Q" | "quit" | "exit" => {
                    let _ = keys_tx.send(Event::Quit);
                    break;
                }
                _ => {}
            }
        }
    });

    let watch_root = project.root.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        None,
        move |result: DebounceEventResult| {
            let Ok(events) = result else { return };
            let mut assets: Vec<String> = Vec::new();
            let mut code_change = false;
            for event in events {
                // Access events (reads, open/close) never change source code.
                if matches!(event.kind, EventKind::Access(_)) {
                    continue;
                }
                for path in &event.paths {
                    if !should_trigger(path) {
                        continue;
                    }
                    if let Some(rel) = asset_rel_path(&watch_root, path) {
                        assets.push(rel);
                    } else {
                        code_change = true;
                    }
                }
            }
            if code_change {
                let _ = tx.send(Event::Change);
            } else if !assets.is_empty() {
                let _ = tx.send(Event::Assets(assets));
            }
        },
    )
    .context("failed to create the file watcher")?;
    debouncer
        .watch(&project.root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch '{}'", project.root.display()))?;

    println!(
        "\n{}",
        format!("[live] watching {} (debug builds)", project.root.display()).bold()
    );
    println!(
        "{}",
        format!("[live] dev channel on {} (credentials in .gpui/)", channel.addr()).dimmed()
    );
    println!(
        "{}",
        "[live] type 'r' + Enter to force rebuild, 'q' + Enter to quit\n".dimmed()
    );

    let mut child: Option<Child> = None;
    let mut last_failed = false;

    // The first call builds and launches immediately, then the loop waits.
    let mut quit_requested = run_cycles(
        project,
        &plan,
        &mut channel,
        &server,
        &mut child,
        &rx,
        &mut last_failed,
    )?;
    while !quit_requested {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(Event::Quit) => break,
            Ok(Event::Change) | Ok(Event::Force) => {
                quit_requested = run_cycles(
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
                if !reload_assets(project, &plan, &server, &paths) {
                    quit_requested = run_cycles(
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
            Err(mpsc::RecvTimeoutError::Timeout) => report_exited_child(&mut child),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Live mode leaves nothing running behind the CLI.
    if let Some(mut child) = child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    println!("{}", "\n[live] stopped".dimmed());
    Ok(())
}

/// Reports a desktop app that exited on its own (crash or window close) without
/// waiting for the next rebuild to find out.
fn report_exited_child(child: &mut Option<Child>) {
    let Some(running) = child.as_mut() else {
        return;
    };
    match running.try_wait() {
        Ok(Some(status)) => {
            child.take();
            let detail = if status.success() {
                "exited normally".to_string()
            } else {
                format!("exited with {status}")
            };
            println!(
                "{}",
                format!("[live] app {detail} — save a file or press 'r' to relaunch").yellow()
            );
        }
        Ok(None) => {}
        Err(_) => {}
    }
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
        assert!(should_trigger(
            &root.join("mobile/android/gradle/app/src/main/java/dev/gpui/mobile/GpuiActivity.kt")
        ));
        assert!(should_trigger(&root.join("mobile/ios/App.swift")));
        assert!(!should_trigger(&root.join("target/debug/app")));
        assert!(!should_trigger(&root.join("mobile/ios/build/Build/Products/app")));
        assert!(!should_trigger(
            &root.join("mobile/android/gradle/app/build/outputs/apk/app-debug.apk")
        ));
        assert!(!should_trigger(
            &root.join("mobile/android/gradle/app/src/main/jniLibs/arm64-v8a/libapp.so")
        ));
        assert!(!should_trigger(&root.join("mobile/ios/App.xcodeproj/project.pbxproj")));
        assert!(!should_trigger(&root.join(".git/index")));
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
        assert_eq!(asset_rel_path(root, &root.join("crates/app/src/lib.rs")), None);
        assert_eq!(asset_rel_path(root, &root.join("assets_mine/x.png")), None);
        assert_eq!(asset_rel_path(root, &root.join("/other/assets/x.png")), None);
    }
}
