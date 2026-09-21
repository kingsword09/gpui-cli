use anyhow::{Context, Result, bail};
use colored::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::device::{self, DeviceFlags, Kind, Platform as DevicePlatform, android, inventory, ios};
use crate::template::Platform;

/// Resolved project layout, read from the current working directory.
pub struct Project {
    pub root: PathBuf,
    pub name: String,
    pub title: String,
    /// Targets recorded by `gpui init`; doctor uses these when no target is
    /// supplied explicitly.
    pub targets: Vec<String>,
    /// Per-project device defaults from the `[run]` section.
    pub defaults: inventory::Defaults,
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
        let manifest = crate::config::Manifest::parse(&manifest)?;
        let name = manifest.app.name;
        let title = manifest.app.title.unwrap_or_else(|| name.clone());
        let targets = manifest.app.targets;
        let defaults = manifest.run;
        Ok(Self {
            root,
            name,
            title,
            targets,
            defaults,
        })
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

pub(crate) fn ensure_tool(tool: &str, hint: &str) -> Result<()> {
    if which::which(tool).is_err() {
        bail!("`{tool}` was not found on PATH.\n  {hint}");
    }
    Ok(())
}

pub(crate) fn ensure_rust_target(target: &str) -> Result<()> {
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

/// Where an iOS build should be targeted.
pub enum IosTarget {
    Simulator(device::Device),
    Physical(device::Device),
}

impl IosTarget {
    pub fn is_device(&self) -> bool {
        matches!(self, IosTarget::Physical(_))
    }

    pub fn label(&self) -> String {
        match self {
            IosTarget::Simulator(d) | IosTarget::Physical(d) => d.label(),
        }
    }
}

/// Resolves the iOS destination: a physical device (`--device-only`,
/// `GPUI_IOS_DEVICE_ID`, or a `--device` naming one), otherwise a simulator.
pub fn resolve_ios_target(project: &Project, flags: &DeviceFlags) -> Result<IosTarget> {
    let device = inventory::resolve_device(DevicePlatform::Ios, flags, &project.defaults, None)?;
    match device.kind {
        Kind::Physical => Ok(IosTarget::Physical(device)),
        Kind::Emulator => Ok(IosTarget::Simulator(device)),
    }
}

/// The `-destination` value `xcodebuild` needs. A concrete UDID avoids the
/// ambiguity of matching a simulator by name, which several runtimes share.
pub(crate) fn xcode_destination(physical: bool, id: &str) -> String {
    if physical {
        "generic/platform=iOS".to_string()
    } else {
        format!("platform=iOS Simulator,id={id}")
    }
}

/// Where xcodebuild drops the built bundle for this configuration.
pub(crate) fn xcode_app_path(
    derived_dir: &Path,
    scheme: &str,
    physical: bool,
    release: bool,
) -> PathBuf {
    let config = if release { "Release" } else { "Debug" };
    let sdk_dir = if physical {
        "iphoneos"
    } else {
        "iphonesimulator"
    };
    derived_dir.join(format!("Build/Products/{config}-{sdk_dir}/{scheme}.app"))
}

/// Builds the Rust staticlib, generates the Xcode project, then builds the app.
/// Returns the path of the produced `.app` bundle.
pub fn build_ios_app(project: &Project, target: &IosTarget, release: bool) -> Result<PathBuf> {
    if !project.ios_dir().exists() {
        bail!("This project has no iOS target. Add one with `gpui init --add`.");
    }
    ensure_tool("xcodegen", "Install it with `brew install xcodegen`.")?;

    let device = target.is_device();
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
    let udid = match target {
        IosTarget::Simulator(device) | IosTarget::Physical(device) => device.id.clone(),
    };
    let destination = xcode_destination(device, &udid);

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

    let app_path = xcode_app_path(&derived_dir, &scheme, device, release);
    if !app_path.exists() {
        bail!(
            "Xcode reported success but no app bundle was found at '{}'.",
            app_path.display()
        );
    }
    println!("  {} {}", "✓".green(), app_path.display());
    Ok(app_path)
}

pub fn run_ios(project: &Project, flags: &DeviceFlags, release: bool) -> Result<()> {
    let target = resolve_ios_target(project, flags)?;

    match target {
        IosTarget::Physical(device) => {
            println!("  {} targeting {}", "→".blue(), device.label());
            let app = build_ios_app(project, &IosTarget::Physical(device.clone()), release)?;
            ios::install_and_launch_device(&device.id, &app, &bundle_id_of(project))?;
            println!(
                "\n{}",
                format!("🚀 Launched on {}.", device.label()).green()
            );
            Ok(())
        }
        IosTarget::Simulator(device) => {
            // Boot before building: xcodebuild needs a booted destination to
            // install onto, and this keeps failures early.
            let ready = device::inventory::ensure_running(device)?;
            let app = build_ios_app(project, &IosTarget::Simulator(ready.clone()), release)?;
            println!("  {} installing on {}", "→".blue(), ready.label());
            ios::install_and_launch(&ready.id, &app, &bundle_id_of(project))?;
            println!("\n{}", format!("🚀 Launched on {}.", ready.label()).green());
            Ok(())
        }
    }
}

/// Reads the bundle id from `mobile/ios/project.yml`.
pub(crate) fn bundle_id_of(project: &Project) -> String {
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
pub(crate) fn android_abis() -> Result<Vec<String>> {
    parse_android_abis(&std::env::var("GPUI_ANDROID_ABIS").unwrap_or_else(|_| "arm64-v8a".into()))
}

fn parse_android_abis(value: &str) -> Result<Vec<String>> {
    let mut abis = Vec::new();
    for abi in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        android_rust_target(abi)?;
        if !abis.iter().any(|existing| existing == abi) {
            abis.push(abi.to_owned());
        }
    }
    if abis.is_empty() {
        bail!("GPUI_ANDROID_ABIS must contain at least one ABI (e.g. arm64-v8a or x86_64)");
    }
    Ok(abis)
}

pub(crate) fn android_rust_target(abi: &str) -> Result<&'static str> {
    match abi {
        "arm64-v8a" => Ok("aarch64-linux-android"),
        "armeabi-v7a" => Ok("armv7-linux-androideabi"),
        "x86" => Ok("i686-linux-android"),
        "x86_64" => Ok("x86_64-linux-android"),
        _ => bail!("Unknown Android ABI '{abi}'. Use arm64-v8a, armeabi-v7a, x86 or x86_64."),
    }
}

pub(crate) fn gradle_task(release: bool) -> &'static str {
    if release {
        "assembleRelease"
    } else {
        "assembleDebug"
    }
}

pub(crate) fn apk_path(project: &Project, release: bool) -> Result<PathBuf> {
    let variant = if release { "release" } else { "debug" };
    let dir = project
        .android_gradle_dir()
        .join(format!("app/build/outputs/apk/{variant}"));
    let metadata_path = dir.join("output-metadata.json");
    #[derive(serde::Deserialize)]
    struct Metadata {
        elements: Vec<Element>,
    }
    #[derive(serde::Deserialize)]
    struct Element {
        #[serde(rename = "outputFile")]
        output_file: String,
    }
    let metadata: Metadata =
        serde_json::from_slice(&fs::read(&metadata_path).with_context(|| {
            format!("reading Gradle APK metadata at {}", metadata_path.display())
        })?)
        .context("invalid Gradle APK metadata")?;
    let [element] = metadata.elements.as_slice() else {
        bail!(
            "Expected one APK in {}; split APK outputs are not supported",
            metadata_path.display()
        );
    };
    let name = Path::new(&element.output_file);
    if element.output_file.contains(['/', '\\'])
        || name.file_name() != Some(name.as_os_str())
        || name.extension().is_none_or(|ext| ext != "apk")
    {
        bail!(
            "Invalid APK filename in Gradle metadata: {}",
            element.output_file
        );
    }
    let apk = dir.join(name);
    if !apk.is_file() {
        bail!(
            "Gradle reported an APK but it is missing: {}",
            apk.display()
        );
    }
    Ok(apk)
}

fn ensure_installable_apk(apk: &Path) -> Result<()> {
    if apk
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with("-unsigned.apk"))
    {
        bail!(
            "{} is unsigned and cannot be installed. Configure a release signingConfig in mobile/android/gradle/app/build.gradle.kts, or use `gpui run android` for a debug build. `gpui build android --release` can produce an unsigned APK for signing separately.",
            apk.display()
        );
    }
    Ok(())
}

pub(crate) fn gradle_command(project: &Project, release: bool, abis: &[String]) -> Command {
    let wrapper = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };
    let mut cmd = Command::new(project.android_gradle_dir().join(wrapper));
    cmd.current_dir(project.android_gradle_dir())
        .arg(gradle_task(release))
        .arg(format!("-Pgpui.abis={}", abis.join(",")))
        .env("GPUI_ANDROID_ABIS", abis.join(","));
    cmd
}

pub(crate) fn check_android_libraries(project: &Project, abis: &[String]) -> Result<()> {
    for abi in abis {
        let expected = project
            .android_jni_libs_dir()
            .join(abi)
            .join(format!("lib{}.so", project.app_lib_name()));
        if !expected.is_file() {
            bail!(
                "cargo-ndk finished but '{}' is missing. Check the `[lib] name` in crates/app/Cargo.toml.",
                expected.display()
            );
        }
    }
    Ok(())
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
    let abis = android_abis()?;
    for abi in &abis {
        ensure_rust_target(android_rust_target(abi)?)?;
    }

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

    check_android_libraries(project, &abis)?;

    // 2. Gradle: package the APK.
    run_step(
        &format!("gradlew {}", gradle_task(release)),
        &mut gradle_command(project, release, &abis),
    )?;

    let apk = apk_path(project, release)?;
    println!("  {} {}", "✓".green(), apk.display());
    Ok(apk)
}

pub fn run_android(project: &Project, flags: &DeviceFlags, release: bool) -> Result<()> {
    let chosen =
        device::inventory::resolve_device(DevicePlatform::Android, flags, &project.defaults, None)?;

    let apk = build_android_apk(project, release)?;
    ensure_installable_apk(&apk)?;
    let target = device::inventory::ensure_running(chosen)?;
    let serial = target.serial().context(
        "The selected Android device has no adb serial; re-run `gpui device list` to check it.",
    )?;

    let bundle_id = bundle_id_of_android(project);

    println!("  {} installing on {}", "→".blue(), target.label());
    android::install_and_launch(serial, &apk, &bundle_id)?;
    println!(
        "\n{}",
        format!("🚀 Launched on {}.", target.label()).green()
    );
    Ok(())
}

/// Reads `applicationId` from the Gradle app module.
pub(crate) fn bundle_id_of_android(project: &Project) -> String {
    let gradle = project.android_gradle_dir().join("app/build.gradle.kts");
    if let Ok(contents) = fs::read_to_string(gradle) {
        for line in contents.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("applicationId")
                && let Some(rest) = rest.trim_start().strip_prefix('=')
            {
                return rest.trim().trim_matches('"').to_string();
            }
        }
    }
    format!("com.example.{}", project.name.replace('-', ""))
}

/// Dispatches `gpui run <target>`.
pub fn handle_run(
    target: Option<String>,
    release: bool,
    live: bool,
    flags: DeviceFlags,
) -> Result<()> {
    let project = Project::load(None)?;
    let target = target.unwrap_or_else(|| "desktop".to_string());

    if live && release {
        bail!("--live rebuilds on every save and only supports debug builds; drop --release.");
    }

    println!(
        "{}",
        format!(" Running '{}' for target: {}\n", project.title, target)
            .bold()
            .cyan()
    );

    if live {
        return super::live::handle_live(&project, &target, &flags);
    }

    match target.to_ascii_lowercase().as_str() {
        "desktop" | "macos" | "windows" | "linux" => run_desktop(&project, release),
        "ios" => run_ios(&project, &flags, release),
        "android" => run_android(&project, &flags, release),
        other => bail!(
            "Unknown target '{other}'. Valid targets: desktop, ios, android.{}",
            match Platform::parse(other) {
                Some(_) => " (use `desktop` for macOS/Windows/Linux)",
                None => "",
            }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &Path) -> Project {
        Project {
            root: root.into(),
            name: "probe".into(),
            title: "Probe".into(),
            targets: vec!["macos".into()],
            defaults: Default::default(),
        }
    }

    fn metadata(project: &Project, name: &str) -> PathBuf {
        let dir = project
            .android_gradle_dir()
            .join("app/build/outputs/apk/release");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("output-metadata.json"),
            serde_json::to_vec(&serde_json::json!({
                "elements": [{"outputFile": name}]
            }))
            .unwrap(),
        )
        .unwrap();
        dir.join(name)
    }

    #[test]
    fn release_apk_uses_gradle_metadata_and_requires_signing_to_run() {
        let dir = tempfile::tempdir().unwrap();
        let project = project(dir.path());
        let unsigned = metadata(&project, "app-release-unsigned.apk");
        fs::write(&unsigned, "apk fixture").unwrap();
        assert_eq!(apk_path(&project, true).unwrap(), unsigned);
        assert!(
            ensure_installable_apk(&unsigned)
                .unwrap_err()
                .to_string()
                .contains("signingConfig")
        );

        let signed = metadata(&project, "custom-release.apk");
        fs::write(&signed, "apk fixture").unwrap();
        assert_eq!(apk_path(&project, true).unwrap(), signed);
        assert!(ensure_installable_apk(&signed).is_ok());
    }

    #[test]
    fn apk_metadata_rejects_missing_files_and_escaping_paths() {
        let dir = tempfile::tempdir().unwrap();
        let project = project(dir.path());
        metadata(&project, "absent.apk");
        assert!(
            apk_path(&project, true)
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        for name in ["../outside.apk", "..\\outside.apk", "/outside.apk"] {
            metadata(&project, name);
            assert!(
                apk_path(&project, true)
                    .unwrap_err()
                    .to_string()
                    .contains("Invalid APK filename")
            );
        }
    }

    #[test]
    fn android_abis_are_validated_and_all_libraries_are_required() {
        let abis = parse_android_abis(" arm64-v8a, x86_64,arm64-v8a ").unwrap();
        assert_eq!(abis, ["arm64-v8a", "x86_64"]);
        assert_eq!(
            android_rust_target("x86_64").unwrap(),
            "x86_64-linux-android"
        );
        assert!(parse_android_abis(" , ").is_err());
        assert!(parse_android_abis("aarch64").is_err());
        let dir = tempfile::tempdir().unwrap();
        let project = project(dir.path());
        let lib = project
            .android_jni_libs_dir()
            .join("arm64-v8a/libprobe_app.so");
        fs::create_dir_all(lib.parent().unwrap()).unwrap();
        fs::write(lib, "library fixture").unwrap();
        assert!(
            check_android_libraries(&project, &abis)
                .unwrap_err()
                .to_string()
                .contains("x86_64")
        );
        let command = gradle_command(&project, false, &abis);
        assert!(
            command
                .get_args()
                .any(|arg| arg == "-Pgpui.abis=arm64-v8a,x86_64")
        );
    }
}
