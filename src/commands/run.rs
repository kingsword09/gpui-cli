use anyhow::{bail, Context, Result};
use colored::*;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::template::Platform;

/// Resolved project layout, read from the current working directory.
pub struct Project {
    pub root: PathBuf,
    pub name: String,
    pub title: String,
}

impl Project {
    /// Loads the project rooted at `dir` (or the CWD), requiring a Cargo
    /// workspace plus the `gpui.toml` written by `gpui init`.
    pub fn load(dir: Option<PathBuf>) -> Result<Self> {
        let root = match dir {
            Some(d) => d,
            None => std::env::current_dir().context("cannot determine current directory")?,
        };
        if !root.join("Cargo.toml").exists() {
            bail!(
                "No Cargo.toml in '{}'. Run this inside a project created by `gpui init`.",
                root.display()
            );
        }
        let manifest_path = root.join("gpui.toml");
        if !manifest_path.exists() {
            bail!(
                "No gpui.toml in '{}'. Run this inside a project created by `gpui init`.",
                root.display()
            );
        }
        let manifest = fs::read_to_string(&manifest_path)?;
        let name = read_string(&manifest, "name").context("gpui.toml has no `name`")?;
        let title = read_string(&manifest, "title").unwrap_or_else(|| name.clone());
        Ok(Self { root, name, title })
    }

    pub fn app_crate(&self) -> String {
        format!("{}-app", self.name)
    }

    pub fn app_lib_name(&self) -> String {
        format!("{}_app", self.name.replace('-', "_"))
    }

    pub fn desktop_crate(&self) -> String {
        format!("{}-desktop", self.name)
    }

    pub fn has_desktop(&self) -> bool {
        self.root.join("crates/desktop").exists()
    }

    /// PascalCase name of the generated Xcode target / scheme.
    pub fn xcode_target(&self) -> String {
        self.name
            .split(['-', '_', ' '])
            .filter(|s| !s.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect()
    }

    pub fn ios_dir(&self) -> PathBuf {
        self.root.join("mobile/ios")
    }

    pub fn android_gradle_dir(&self) -> PathBuf {
        self.root.join("mobile/android/gradle")
    }

    pub fn android_jni_libs_dir(&self) -> PathBuf {
        self.android_gradle_dir().join("app/src/main/jniLibs")
    }
}

fn read_string(contents: &str, key: &str) -> Option<String> {
    let section = contents.split("[app]").nth(1)?;
    let section = section.split("\n[").next()?;
    for line in section.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.trim_start().strip_prefix('=') {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn run_step(label: &str, cmd: &mut Command) -> Result<()> {
    println!("  {} {}", "→".blue(), label);
    let status = cmd
        .status()
        .with_context(|| format!("failed to spawn: {label}"))?;
    if !status.success() {
        bail!("{label} failed");
    }
    Ok(())
}

fn ensure_tool(tool: &str, hint: &str) -> Result<()> {
    if which::which(tool).is_err() {
        bail!("`{tool}` was not found on PATH.\n  {hint}");
    }
    Ok(())
}

fn ensure_rust_target(target: &str) -> Result<()> {
    println!("  {} ensuring Rust target {}", "→".blue(), target);
    let status = Command::new("rustup")
        .args(["target", "add", target])
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => bail!("could not install Rust target `{target}`; run: rustup target add {target}"),
    }
}

// ── Desktop ──────────────────────────────────────────────────────────────────

pub fn run_desktop(project: &Project, release: bool) -> Result<()> {
    if !project.has_desktop() {
        bail!("This project has no desktop target. Add one with `gpui init --add`.");
    }
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&project.root)
        .args(["run", "-p", &project.desktop_crate()]);
    if release {
        cmd.arg("--release");
    }
    run_step(
        &format!("cargo run -p {}", project.desktop_crate()),
        &mut cmd,
    )
}

pub fn build_desktop(project: &Project, release: bool) -> Result<()> {
    if !project.has_desktop() {
        bail!("This project has no desktop target. Add one with `gpui init --add`.");
    }
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&project.root)
        .args(["build", "-p", &project.desktop_crate()]);
    if release {
        cmd.arg("--release");
    }
    run_step(
        &format!("cargo build -p {}", project.desktop_crate()),
        &mut cmd,
    )
}

// ── iOS ──────────────────────────────────────────────────────────────────────

/// Simulator device name, overridable with `GPUI_IOS_DEVICE`.
fn simulator_name() -> String {
    std::env::var("GPUI_IOS_DEVICE").unwrap_or_else(|_| "iPhone 16 Pro".to_string())
}

/// Resolves a simulator UDID, falling back to the first available iPhone.
fn resolve_simulator(preferred: &str) -> Result<String> {
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "available"])
        .output()
        .context("failed to run `xcrun simctl list devices`")?;
    let listing = String::from_utf8_lossy(&output.stdout);

    // Prefer an exact name match on an available device.
    for line in listing.lines() {
        if line.contains(preferred) {
            if let Some(udid) = extract_udid(line) {
                return Ok(udid);
            }
        }
    }
    for line in listing.lines() {
        if line.contains("iPhone") {
            if let Some(udid) = extract_udid(line) {
                return Ok(udid);
            }
        }
    }
    bail!(
        "No available iOS simulator found. Open Simulator.app once, or set GPUI_IOS_DEVICE \
         to an installed device name."
    )
}

fn extract_udid(line: &str) -> Option<String> {
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let candidate = &line[open + 1..close];
    let looks_like_udid =
        candidate.len() >= 8 && candidate.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if looks_like_udid {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Builds the Rust staticlib, generates the Xcode project, then builds the app.
/// Returns the path of the produced `.app` bundle.
pub fn build_ios_app(project: &Project, device: bool, release: bool) -> Result<PathBuf> {
    if !project.ios_dir().exists() {
        bail!("This project has no iOS target. Add one with `gpui init --add`.");
    }
    ensure_tool("xcodegen", "Install it with `brew install xcodegen`.")?;

    let rust_target = if device {
        "aarch64-apple-ios"
    } else {
        "aarch64-apple-ios-sim"
    };
    ensure_rust_target(rust_target)?;

    // 1. Rust staticlib (Xcode's build phase also does this, but doing it here
    //    surfaces Rust errors with Rust-quality messages).
    let mut cargo = Command::new("cargo");
    cargo.current_dir(&project.root).args([
        "build",
        "--lib",
        "-p",
        &project.app_crate(),
        "--target",
        rust_target,
    ]);
    if release {
        cargo.arg("--release");
    }
    run_step(&format!("cargo build --target {rust_target}"), &mut cargo)?;

    // 2. XcodeGen: project.yml -> .xcodeproj
    let ios_dir = project.ios_dir();
    run_step(
        "xcodegen generate",
        Command::new("xcodegen")
            .current_dir(&ios_dir)
            .args(["generate", "--spec", "project.yml"]),
    )?;

    // 3. xcodebuild
    let scheme = project.xcode_target();
    let xcode_project = ios_dir.join(format!("{scheme}.xcodeproj"));
    let config = if release { "Release" } else { "Debug" };
    let derived_dir = ios_dir.join("build");
    fs::create_dir_all(&derived_dir)?;

    // Resolve to a concrete UDID: matching a simulator by name is ambiguous
    // once several runtimes are installed, and xcodebuild then refuses to pick.
    let destination = if device {
        "generic/platform=iOS".to_string()
    } else {
        format!(
            "platform=iOS Simulator,id={}",
            resolve_simulator(&simulator_name())?
        )
    };

    let mut xcodebuild = Command::new("xcodebuild");
    xcodebuild
        .current_dir(&ios_dir)
        .arg("-project")
        .arg(&xcode_project)
        .arg("-scheme")
        .arg(&scheme)
        .arg("-configuration")
        .arg(config)
        .arg("-destination")
        .arg(&destination)
        .arg("-derivedDataPath")
        .arg(&derived_dir)
        .arg("-allowProvisioningUpdates")
        .arg("build");
    run_step(&format!("xcodebuild ({config})"), &mut xcodebuild)?;

    let sdk_dir = if device {
        "iphoneos"
    } else {
        "iphonesimulator"
    };
    let app_path = derived_dir.join(format!("Build/Products/{config}-{sdk_dir}/{scheme}.app"));
    if !app_path.exists() {
        bail!(
            "Xcode reported success but no app bundle was found at '{}'.",
            app_path.display()
        );
    }
    println!("  {} {}", "✓".green(), app_path.display());
    Ok(app_path)
}

pub fn run_ios(project: &Project, release: bool) -> Result<()> {
    // `GPUI_IOS_DEVICE_ID` switches to a connected physical device.
    match std::env::var("GPUI_IOS_DEVICE_ID") {
        Ok(device_id) if !device_id.trim().is_empty() => {
            let app = build_ios_app(project, true, release)?;
            run_step(
                "devicectl install + launch",
                Command::new("xcrun")
                    .args(["devicectl", "device", "install", "app"])
                    .arg(&app)
                    .arg("--device")
                    .arg(device_id.trim()),
            )?;
            run_step(
                "devicectl launch",
                Command::new("xcrun")
                    .args(["devicectl", "device", "process", "launch"])
                    .arg("--device")
                    .arg(device_id.trim())
                    .arg(bundle_id_of(project)),
            )
        }
        _ => {
            let app = build_ios_app(project, false, release)?;
            let udid = resolve_simulator(&simulator_name())?;

            println!("  {} booting simulator {}", "→".blue(), udid);
            // Already-booted simulators make `boot` fail; that is fine.
            let _ = Command::new("xcrun")
                .args(["simctl", "boot", &udid])
                .status();
            let _ = Command::new("open").args(["-a", "Simulator"]).status();
            run_step(
                "waiting for simulator",
                Command::new("xcrun").args(["simctl", "bootstatus", &udid, "-b"]),
            )?;
            run_step(
                "simctl install",
                Command::new("xcrun")
                    .args(["simctl", "install", &udid])
                    .arg(&app),
            )?;
            let bundle_id = bundle_id_of(project);
            run_step(
                "simctl launch",
                Command::new("xcrun").args([
                    "simctl",
                    "launch",
                    "--terminate-running-process",
                    &udid,
                    &bundle_id,
                ]),
            )?;
            println!("\n{}", "🚀 Launched on the iOS Simulator.".green());
            Ok(())
        }
    }
}

/// Reads the bundle id from `mobile/ios/project.yml`.
fn bundle_id_of(project: &Project) -> String {
    if let Ok(yml) = fs::read_to_string(project.ios_dir().join("project.yml")) {
        for line in yml.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("PRODUCT_BUNDLE_IDENTIFIER:") {
                return rest.trim().to_string();
            }
        }
    }
    format!("com.example.{}", project.name.replace('-', ""))
}

// ── Android ──────────────────────────────────────────────────────────────────

/// ABIs to build, overridable with `GPUI_ANDROID_ABIS` (comma separated).
fn android_abis() -> Vec<String> {
    std::env::var("GPUI_ANDROID_ABIS")
        .unwrap_or_else(|_| "arm64-v8a".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn gradle_task(release: bool) -> &'static str {
    if release {
        "assembleRelease"
    } else {
        "assembleDebug"
    }
}

fn apk_path(project: &Project, release: bool) -> PathBuf {
    let variant = if release { "release" } else { "debug" };
    project
        .android_gradle_dir()
        .join(format!("app/build/outputs/apk/{variant}/app-{variant}.apk"))
}

/// Compiles the Rust `cdylib` into `jniLibs`, then assembles the APK.
pub fn build_android_apk(project: &Project, release: bool) -> Result<PathBuf> {
    if !project.android_gradle_dir().exists() {
        bail!("This project has no Android target. Add one with `gpui init --add`.");
    }
    ensure_tool(
        "cargo-ndk",
        "Install it with `cargo install cargo-ndk`, then set ANDROID_NDK_HOME.",
    )?;
    ensure_rust_target("aarch64-linux-android")?;

    let abis = android_abis();

    // 1. Rust shared library via cargo-ndk.
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
    if release {
        ndk.arg("--release");
    }
    run_step(&format!("cargo ndk ({})", abis.join(", ")), &mut ndk)?;

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

    // 2. Gradle: package the APK.
    run_step(
        &format!("gradlew {}", gradle_task(release)),
        Command::new("./gradlew")
            .current_dir(project.android_gradle_dir())
            .arg(gradle_task(release)),
    )?;

    let apk = apk_path(project, release);
    if !apk.exists() {
        bail!(
            "Gradle finished but no APK was found at '{}'.",
            apk.display()
        );
    }
    println!("  {} {}", "✓".green(), apk.display());
    Ok(apk)
}

pub fn run_android(project: &Project, release: bool) -> Result<()> {
    ensure_tool(
        "adb",
        "Install the Android platform tools and put `adb` on PATH.",
    )?;

    // Host rendering is the tested emulator configuration. SurfaceFlinger's
    // renderer is a useful hint, but does not prove which adapter GPUI uses or
    // whether the application's shaders can compile.
    if let Some(("emulator", _)) = adb_target_kind() {
        let cpu_only = emulator_reports_cpu_only_gpu();
        if cpu_only {
            println!(
                "  {} emulator reports software rendering; this configuration is not yet verified.\n\
                   {} use the tested host mode: emulator -avd <name> -gpu host",
                "⚠".yellow(),
                " ".repeat(9)
            );
        }
    }
    let apk = build_android_apk(project, release)?;
    let bundle_id = bundle_id_of_android(project);

    run_step(
        "adb install -r",
        Command::new("adb").args(["install", "-r"]).arg(&apk),
    )?;

    // Prefer an explicit launcher activity from the manifest; the generated
    // host uses the gpui-mobile activity.
    run_step(
        "adb shell am start",
        Command::new("adb").args([
            "shell",
            "am",
            "start",
            "-n",
            &format!("{bundle_id}/dev.gpui.mobile.GpuiActivity"),
        ]),
    )?;
    println!(
        "\n{}",
        "🚀 Launched on the Android device/emulator.".green()
    );
    Ok(())
}

/// Returns the connected target's kind and serial, if a device is attached.
fn adb_target_kind() -> Option<(&'static str, String)> {
    let output = Command::new("adb").args(["devices"]).output().ok()?;
    let listing = String::from_utf8_lossy(&output.stdout);
    for line in listing.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let serial = parts.next()?;
        let state = parts.next().unwrap_or("");
        if state != "device" {
            continue;
        }
        let kind = if serial.starts_with("emulator-") {
            "emulator"
        } else {
            "device"
        };
        return Some((kind, serial.to_string()));
    }
    None
}

/// Asks the emulator whether any non-CPU GPU adapter is present.
fn emulator_reports_cpu_only_gpu() -> bool {
    let output = Command::new("adb")
        .args(["shell", "dumpsys", "SurfaceFlinger"])
        .output();
    let Ok(output) = output else { return false };
    let text = String::from_utf8_lossy(&output.stdout);
    // SurfaceFlinger lists the GLES renderer it is using.
    let gl = text
        .lines()
        .find(|l| l.contains("GLES:") || l.contains("GLES renderer"))
        .unwrap_or("");
    gl.contains("SwiftShader")
        || gl.contains("llvmpipe")
        || gl.contains("ANGLE (Google, Vulkan 1.3.0 (SwiftShader")
}

/// Reads `applicationId` from the Gradle app module.
fn bundle_id_of_android(project: &Project) -> String {
    let gradle = project.android_gradle_dir().join("app/build.gradle.kts");
    if let Ok(contents) = fs::read_to_string(gradle) {
        for line in contents.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("applicationId") {
                if let Some(rest) = rest.trim_start().strip_prefix('=') {
                    return rest.trim().trim_matches('"').to_string();
                }
            }
        }
    }
    format!("com.example.{}", project.name.replace('-', ""))
}

/// Dispatches `gpui run <target>`.
pub fn handle_run(target: Option<String>, release: bool) -> Result<()> {
    let project = Project::load(None)?;
    let target = target.unwrap_or_else(|| "desktop".to_string());

    println!(
        "{}",
        format!("🚀 Running '{}' for target: {}\n", project.title, target)
            .bold()
            .cyan()
    );

    match target.to_ascii_lowercase().as_str() {
        "desktop" | "macos" | "windows" | "linux" => run_desktop(&project, release),
        "ios" => run_ios(&project, release),
        "android" => run_android(&project, release),
        other => bail!(
            "Unknown target '{other}'. Valid targets: desktop, ios, android.{}",
            match Platform::parse(other) {
                Some(_) => " (use `desktop` for macOS/Windows/Linux)",
                None => "",
            }
        ),
    }
}
