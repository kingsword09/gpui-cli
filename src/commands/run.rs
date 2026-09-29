use anyhow::{Context, Result, bail};
use colored::*;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::device::{self, DeviceFlags, Kind, Platform as DevicePlatform, inventory, ios};
use crate::runner::android::AndroidRunner;
use crate::runner::build_cache::{BuildCacheLookup, BuildOutputLock, lookup_verified};
use crate::runner::build_inputs::{
    DesktopBuildPlan, android_build_plan, desktop_build_plan, ios_build_plan,
};
use crate::runner::build_manifest::BuildArtifactManifest;
use crate::runner::ios::IosSimulatorRunner;
use crate::runner::lease::DeviceLeaseSession;
use crate::runner::mobile::{MobileRunner, RunRequest};
use crate::runner::output_layout::BuildOutputLayout;
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoBinaryArtifact {
    name: String,
    executable: PathBuf,
}

fn parse_cargo_binary_artifact(line: &str, package_name: &str) -> Option<CargoBinaryArtifact> {
    let message: serde_json::Value = serde_json::from_str(line).ok()?;
    if message["reason"].as_str()? != "compiler-artifact" {
        return None;
    }
    let package_id = message["package_id"].as_str()?;
    let package_id_name = package_id
        .rsplit('#')
        .next()?
        .split('@')
        .next()
        .unwrap_or_default();
    if package_id_name != package_name {
        return None;
    }
    let target = &message["target"];
    let is_binary = target["kind"]
        .as_array()?
        .iter()
        .any(|kind| kind.as_str() == Some("bin"));
    if !is_binary {
        return None;
    }
    Some(CargoBinaryArtifact {
        name: target["name"].as_str()?.to_string(),
        executable: PathBuf::from(message["executable"].as_str()?),
    })
}

fn run_desktop_cargo_build(
    project: &Project,
    plan: &DesktopBuildPlan,
    release: bool,
) -> Result<Vec<CargoBinaryArtifact>> {
    let package = project.desktop_crate();
    let mut command = Command::new("cargo");
    command
        .current_dir(&plan.snapshot.root)
        .args([
            "build",
            "-p",
            &package,
            "--message-format=json-render-diagnostics",
        ])
        .env("CARGO_TARGET_DIR", &plan.layout.cargo_target_dir);
    if release {
        command.arg("--release");
    }
    println!("  {} cargo build -p {}", "→".blue(), package);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn: cargo build for desktop")?;
    let stdout = child
        .stdout
        .take()
        .context("capturing cargo build output for desktop")?;
    let mut artifacts = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading cargo build output for desktop")?;
        if let Some(artifact) = parse_cargo_binary_artifact(&line, &package) {
            artifacts.push(artifact);
            continue;
        }
        if serde_json::from_str::<serde_json::Value>(&line).is_err() {
            println!("{line}");
        }
    }
    let status = child.wait().context("waiting for desktop cargo build")?;
    if !status.success() {
        bail!("cargo build -p {package} failed");
    }
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));
    artifacts.dedup_by(|left, right| left.name == right.name);
    if artifacts.is_empty() {
        bail!("cargo build succeeded but reported no binary artifacts for {package}");
    }
    Ok(artifacts)
}

fn publish_desktop_build_manifest(
    layout: &BuildOutputLayout,
    artifacts: &[CargoBinaryArtifact],
) -> Result<BuildArtifactManifest> {
    if artifacts.is_empty() {
        bail!("desktop build produced no binary artifacts");
    }
    let entries = artifacts
        .iter()
        .map(|artifact| {
            artifact
                .executable
                .strip_prefix(&layout.root)
                .map(Path::to_owned)
                .with_context(|| {
                    format!(
                        "desktop executable is outside the BuildKey output root: {}",
                        artifact.executable.display()
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let manifest = BuildArtifactManifest::capture(layout, &entries)
        .context("capturing desktop BuildKey artifacts")?;
    manifest
        .write_atomic(&layout.artifact_manifest_path())
        .context("publishing desktop BuildKey artifact manifest")?;
    Ok(manifest)
}

fn build_desktop_artifacts(
    project: &Project,
    plan: &DesktopBuildPlan,
    release: bool,
) -> Result<()> {
    let _output_lock = BuildOutputLock::acquire(&plan.layout)?;
    let cache_lookup = if let Some(reason) = &plan.cache_hit_disabled_reason {
        BuildCacheLookup::Miss(reason.clone())
    } else {
        lookup_verified(&plan.layout, &plan.key)
    };
    match cache_lookup {
        BuildCacheLookup::Hit(manifest) => {
            println!(
                "  {} BuildKey cache hit: {} verified artifact(s)",
                "✓".green(),
                manifest.files.len()
            );
            return Ok(());
        }
        BuildCacheLookup::Miss(reason) => {
            println!("  {} desktop cache miss: {reason}", "→".blue());
        }
    }
    let artifacts = run_desktop_cargo_build(project, plan, release)?;
    let manifest = publish_desktop_build_manifest(&plan.layout, &artifacts)?;
    println!(
        "  {} artifact manifest: {}",
        "✓".green(),
        plan.layout.artifact_manifest_path().display()
    );
    for artifact in &artifacts {
        println!("  {} {}", "✓".green(), artifact.executable.display());
    }
    debug_assert_eq!(manifest.files.len(), artifacts.len());
    Ok(())
}

pub fn run_desktop(project: &Project, release: bool) -> Result<()> {
    if !project.has_desktop() {
        bail!("This project has no desktop target. Add one with `gpui init --add`.");
    }
    let plan = desktop_build_plan(&project.root, release)?;
    build_desktop_artifacts(project, &plan, release)?;
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&plan.snapshot.root)
        .args(["run", "-p", &project.desktop_crate()])
        .env("CARGO_TARGET_DIR", &plan.layout.cargo_target_dir);
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
    let plan = desktop_build_plan(&project.root, release)?;
    build_desktop_artifacts(project, &plan, release)?;
    Ok(())
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

/// Wraps one platform workload with a fencing check. Desktop callers do not
/// need a device lease, while iOS/Android install, launch, input and capture
/// callers pass their session through this helper.
pub(crate) fn leased_device_step<T>(
    lease: Option<&DeviceLeaseSession>,
    stage: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match lease {
        Some(lease) => lease.execute(stage, operation),
        None => operation(),
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

fn publish_ios_build_manifest(
    layout: &BuildOutputLayout,
    app_path: &Path,
) -> Result<BuildArtifactManifest> {
    if layout.ios_derived_data_dir.is_none() {
        bail!("iOS output layout did not provide a DerivedData path");
    }
    let app_relative = app_path.strip_prefix(&layout.root).with_context(|| {
        format!(
            "iOS app bundle is outside the BuildKey output root: {}",
            app_path.display()
        )
    })?;
    let manifest = BuildArtifactManifest::capture(layout, &[app_relative.to_owned()])
        .context("capturing iOS BuildKey app bundle")?;
    manifest
        .write_atomic(&layout.artifact_manifest_path())
        .context("publishing iOS BuildKey artifact manifest")?;
    Ok(manifest)
}

fn lookup_verified_ios_app(
    layout: &BuildOutputLayout,
    key: &crate::runner::build_key::BuildKey,
    app_path: &Path,
) -> BuildCacheLookup {
    let expected_root = match app_path.strip_prefix(&layout.root) {
        Ok(path) => path.to_string_lossy().replace('\\', "/"),
        Err(_) => {
            return BuildCacheLookup::Miss(
                "expected iOS app bundle is outside the BuildKey output root".into(),
            );
        }
    };

    match lookup_verified(layout, key) {
        BuildCacheLookup::Hit(manifest)
            if manifest.roots.iter().any(|root| root == &expected_root) =>
        {
            BuildCacheLookup::Hit(manifest)
        }
        BuildCacheLookup::Hit(_) => BuildCacheLookup::Miss(
            "verified artifact manifest does not declare the expected iOS app bundle".into(),
        ),
        BuildCacheLookup::Miss(reason) => BuildCacheLookup::Miss(reason),
    }
}

fn lookup_ios_app_for_target(
    layout: &BuildOutputLayout,
    key: &crate::runner::build_key::BuildKey,
    app_path: &Path,
    physical_device: bool,
) -> BuildCacheLookup {
    if physical_device {
        BuildCacheLookup::Miss(
            "physical-device cache reuse is disabled until signing inputs are represented in the BuildKey".into(),
        )
    } else {
        lookup_verified_ios_app(layout, key, app_path)
    }
}

/// Builds the Rust staticlib and app, or reuses a verified simulator bundle.
/// Returns the path of the produced `.app` bundle.
pub fn build_ios_app(project: &Project, target: &IosTarget, release: bool) -> Result<PathBuf> {
    if !project.ios_dir().exists() {
        bail!("This project has no iOS target. Add one with `gpui init --add`.");
    }

    let device = target.is_device();
    let rust_target = if device {
        "aarch64-apple-ios"
    } else {
        "aarch64-apple-ios-sim"
    };
    let plan = ios_build_plan(&project.root, release, rust_target)?;
    let _output_lock = BuildOutputLock::acquire(&plan.layout)?;
    let layout = &plan.layout;
    let snapshot_root = &plan.snapshot.root;
    let scheme = project.xcode_target();
    let derived_dir = layout
        .ios_derived_data_dir
        .as_deref()
        .context("iOS output layout did not provide a DerivedData path")?;
    let app_path = xcode_app_path(derived_dir, &scheme, device, release);

    let cache_lookup = if let Some(reason) = &plan.cache_hit_disabled_reason {
        BuildCacheLookup::Miss(reason.clone())
    } else {
        lookup_ios_app_for_target(layout, &plan.key, &app_path, device)
    };
    match cache_lookup {
        BuildCacheLookup::Hit(manifest) => {
            println!(
                "  {} iOS BuildKey cache hit: {} verified artifact(s)",
                "✓".green(),
                manifest.files.len()
            );
            return Ok(app_path);
        }
        BuildCacheLookup::Miss(reason) => {
            println!("  {} iOS cache miss: {reason}", "→".blue());
        }
    }

    ensure_tool("xcodegen", "Install it with `brew install xcodegen`.")?;
    ensure_rust_target(rust_target)?;

    // 1. Rust staticlib (Xcode's build phase also does this, but doing it here
    //    surfaces Rust errors with Rust-quality messages).
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(snapshot_root)
        .args([
            "build",
            "--lib",
            "-p",
            &project.app_crate(),
            "--target",
            rust_target,
        ])
        .env("CARGO_TARGET_DIR", &layout.cargo_target_dir);
    if release {
        cargo.arg("--release");
    }
    run_step(&format!("cargo build --target {rust_target}"), &mut cargo)?;

    // 2. XcodeGen: project.yml -> .xcodeproj
    let ios_dir = snapshot_root.join("mobile/ios");
    run_step(
        "xcodegen generate",
        Command::new("xcodegen")
            .current_dir(&ios_dir)
            .args(["generate", "--spec", "project.yml"]),
    )?;

    // 3. xcodebuild
    let xcode_project = ios_dir.join(format!("{scheme}.xcodeproj"));
    let config = if release { "Release" } else { "Debug" };

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
        .arg(derived_dir)
        .arg(format!(
            "GPUI_CARGO_TARGET_DIR={}",
            layout.cargo_target_dir.display()
        ))
        .env("CARGO_TARGET_DIR", &layout.cargo_target_dir)
        .arg("-allowProvisioningUpdates")
        .arg("build");
    run_step(&format!("xcodebuild ({config})"), &mut xcodebuild)?;

    if !app_path.is_dir() {
        bail!(
            "Xcode reported success but no app bundle directory was found at '{}'.",
            app_path.display()
        );
    }
    publish_ios_build_manifest(layout, &app_path)?;
    println!(
        "  {} artifact manifest: {}",
        "✓".green(),
        layout.artifact_manifest_path().display()
    );
    println!("  {} {}", "✓".green(), app_path.display());
    Ok(app_path)
}

pub fn run_ios(project: &Project, flags: &DeviceFlags, release: bool) -> Result<()> {
    let target = resolve_ios_target(project, flags)?;

    match target {
        IosTarget::Physical(device) => {
            println!("  {} targeting {}", "→".blue(), device.label());
            let app = build_ios_app(project, &IosTarget::Physical(device.clone()), release)?;
            let lease = DeviceLeaseSession::acquire(&project.root, &device.id)
                .context("acquiring the iOS device lease")?;
            let bundle_id = bundle_id_of(project);
            leased_device_step(Some(&lease), "ios.install", || {
                ios::install_device(&device.id, &app)
            })?;
            leased_device_step(Some(&lease), "ios.launch", || {
                ios::launch_device(&device.id, &bundle_id)
            })?;
            lease
                .release()
                .map_err(anyhow::Error::new)
                .context("releasing the iOS device lease")?;
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
            let lease = DeviceLeaseSession::acquire(&project.root, &ready.id)
                .context("acquiring the iOS simulator lease")?;
            let bundle_id = bundle_id_of(project);
            println!("  {} installing on {}", "→".blue(), ready.label());
            let mut runner = IosSimulatorRunner::new(
                ready.id.clone(),
                app,
                bundle_id.clone(),
                project.root.join(".gpui/runs"),
            );
            let request = RunRequest {
                run_id: mobile_run_id(&project.name, &ready.id),
                project_id: project.name.clone(),
                device_id: ready.id.clone(),
                bundle_id,
                artifact_root: project.root.join(".gpui/runs"),
                abi: None,
            };
            let prepared = runner.prepare(&request, &lease)?;
            runner.launch(&prepared, &lease)?;
            lease
                .release()
                .map_err(anyhow::Error::new)
                .context("releasing the iOS simulator lease")?;
            println!("\n{}", format!("🚀 Launched on {}.", ready.label()).green());
            Ok(())
        }
    }
}

fn mobile_run_id(project: &str, device_id: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let safe = device_id
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                byte as char
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("run-{}-{}-{}", project.replace('-', "_"), safe, now)
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

pub(crate) fn apk_path_at(
    project: &Project,
    release: bool,
    gradle_build_dir: Option<&Path>,
) -> Result<PathBuf> {
    let variant = if release { "release" } else { "debug" };
    let dir = match gradle_build_dir {
        Some(build_dir) => build_dir.join(format!("outputs/apk/{variant}")),
        None => project
            .android_gradle_dir()
            .join(format!("app/build/outputs/apk/{variant}")),
    };
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

#[cfg(test)]
pub(crate) fn apk_path(project: &Project, release: bool) -> Result<PathBuf> {
    apk_path_at(project, release, None)
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

pub(crate) fn gradle_command_with_outputs(
    project: &Project,
    release: bool,
    abis: &[String],
    jni_libs_dir: Option<&Path>,
    gradle_build_dir: Option<&Path>,
) -> Command {
    gradle_command_at(
        &project.android_gradle_dir(),
        release,
        abis,
        jni_libs_dir,
        gradle_build_dir,
    )
}

#[cfg(test)]
pub(crate) fn gradle_command(project: &Project, release: bool, abis: &[String]) -> Command {
    gradle_command_with_outputs(project, release, abis, None, None)
}

fn gradle_command_at(
    gradle_dir: &Path,
    release: bool,
    abis: &[String],
    jni_libs_dir: Option<&Path>,
    gradle_build_dir: Option<&Path>,
) -> Command {
    let wrapper = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };
    let mut cmd = Command::new(gradle_dir.join(wrapper));
    cmd.current_dir(gradle_dir)
        .arg(gradle_task(release))
        .arg(format!("-Pgpui.abis={}", abis.join(",")))
        .env("GPUI_ANDROID_ABIS", abis.join(","));
    if let Some(path) = jni_libs_dir {
        cmd.arg(format!("-Pgpui.jniLibsDir={}", path.display()));
    }
    if let Some(path) = gradle_build_dir {
        cmd.arg(format!("-Pgpui.buildDir={}", path.display()));
    }
    cmd
}

pub(crate) fn check_android_libraries_at(
    jni_libs_dir: &Path,
    app_lib_name: &str,
    abis: &[String],
) -> Result<()> {
    for abi in abis {
        let expected = jni_libs_dir.join(abi).join(format!("lib{app_lib_name}.so"));
        if !expected.is_file() {
            bail!(
                "cargo-ndk finished but '{}' is missing. Check the `[lib] name` in crates/app/Cargo.toml.",
                expected.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn check_android_libraries(project: &Project, abis: &[String]) -> Result<()> {
    check_android_libraries_at(
        &project.android_jni_libs_dir(),
        &project.app_lib_name(),
        abis,
    )
}

fn publish_android_build_manifest(
    layout: &BuildOutputLayout,
    apk: &Path,
) -> Result<BuildArtifactManifest> {
    let jni_libs_dir = layout
        .android_jni_dir
        .as_deref()
        .context("Android output layout did not provide a JNI staging path")?;
    let apk_output_dir = apk
        .parent()
        .context("Gradle APK output has no parent directory")?;
    let jni_relative = jni_libs_dir.strip_prefix(&layout.root).with_context(|| {
        format!(
            "Android JNI staging path is outside the BuildKey output root: {}",
            jni_libs_dir.display()
        )
    })?;
    let apk_relative = apk_output_dir.strip_prefix(&layout.root).with_context(|| {
        format!(
            "Gradle APK output is outside the BuildKey output root: {}",
            apk_output_dir.display()
        )
    })?;
    let manifest =
        BuildArtifactManifest::capture(layout, &[jni_relative.to_owned(), apk_relative.to_owned()])
            .context("capturing Android BuildKey artifacts")?;
    manifest
        .write_atomic(&layout.artifact_manifest_path())
        .context("publishing Android BuildKey artifact manifest")?;
    Ok(manifest)
}

enum AndroidBuildCacheLookup {
    Hit {
        manifest: BuildArtifactManifest,
        apk: PathBuf,
    },
    Miss(String),
}

fn lookup_verified_android_apk(
    project: &Project,
    layout: &BuildOutputLayout,
    key: &crate::runner::build_key::BuildKey,
    release: bool,
) -> AndroidBuildCacheLookup {
    let Some(jni_libs_dir) = layout.android_jni_dir.as_deref() else {
        return AndroidBuildCacheLookup::Miss(
            "Android output layout did not provide a JNI staging path".into(),
        );
    };
    let Some(gradle_build_dir) = layout.android_gradle_build_dir.as_deref() else {
        return AndroidBuildCacheLookup::Miss(
            "Android output layout did not provide a Gradle build path".into(),
        );
    };

    match lookup_verified(layout, key) {
        BuildCacheLookup::Miss(reason) => AndroidBuildCacheLookup::Miss(reason),
        BuildCacheLookup::Hit(manifest) => {
            let apk = match apk_path_at(project, release, Some(gradle_build_dir)) {
                Ok(apk) => apk,
                Err(error) => {
                    return AndroidBuildCacheLookup::Miss(format!(
                        "verified Android output has invalid APK metadata: {error:#}"
                    ));
                }
            };
            let Some(apk_output_dir) = apk.parent() else {
                return AndroidBuildCacheLookup::Miss(
                    "verified Android APK has no output directory".into(),
                );
            };
            let Some(jni_root) = jni_libs_dir
                .strip_prefix(&layout.root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
            else {
                return AndroidBuildCacheLookup::Miss(
                    "Android JNI staging path is outside the BuildKey output root".into(),
                );
            };
            let Some(apk_root) = apk_output_dir
                .strip_prefix(&layout.root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
            else {
                return AndroidBuildCacheLookup::Miss(
                    "Android APK output path is outside the BuildKey output root".into(),
                );
            };
            let mut expected_roots = vec![jni_root, apk_root];
            expected_roots.sort();
            if manifest.roots != expected_roots {
                return AndroidBuildCacheLookup::Miss(
                    "verified Android manifest does not declare the expected JNI and APK outputs"
                        .into(),
                );
            }
            AndroidBuildCacheLookup::Hit { manifest, apk }
        }
    }
}

/// Builds Android outputs or reuses a verified default-debug APK and JNI tree.
pub fn build_android_apk(project: &Project, release: bool) -> Result<PathBuf> {
    if !project.android_gradle_dir().exists() {
        bail!("This project has no Android target. Add one with `gpui init --add`.");
    }
    let abis = android_abis()?;
    let plan = android_build_plan(&project.root, release, &abis)?;
    let _output_lock = BuildOutputLock::acquire(&plan.layout)?;
    let layout = &plan.layout;

    let cache_lookup = if let Some(reason) = &plan.cache_hit_disabled_reason {
        AndroidBuildCacheLookup::Miss(reason.clone())
    } else {
        if let Some(identity) = &plan.debug_keystore_identity {
            identity.verify_unchanged()?;
        }
        lookup_verified_android_apk(project, layout, &plan.key, release)
    };
    match cache_lookup {
        AndroidBuildCacheLookup::Hit { manifest, apk } => {
            if let Some(identity) = &plan.debug_keystore_identity {
                identity.verify_unchanged()?;
            }
            println!(
                "  {} Android BuildKey cache hit: {} verified artifact(s)",
                "✓".green(),
                manifest.files.len()
            );
            return Ok(apk);
        }
        AndroidBuildCacheLookup::Miss(reason) => {
            println!("  {} Android cache miss: {reason}", "→".blue());
        }
    }

    ensure_tool(
        "cargo-ndk",
        "Install it with `cargo install cargo-ndk`, then set ANDROID_NDK_HOME.",
    )?;
    for abi in &abis {
        ensure_rust_target(android_rust_target(abi)?)?;
    }
    let snapshot_root = &plan.snapshot.root;
    let jni_libs_dir = layout
        .android_jni_dir
        .as_deref()
        .context("Android output layout did not provide a JNI staging path")?;
    let gradle_build_dir = layout
        .android_gradle_build_dir
        .as_deref()
        .context("Android output layout did not provide a Gradle build path")?;

    // 1. Rust shared library via cargo-ndk.
    let mut ndk = Command::new("cargo");
    ndk.current_dir(snapshot_root)
        .args(["ndk"])
        .env("CARGO_TARGET_DIR", &layout.cargo_target_dir);
    for abi in &abis {
        ndk.args(["-t", abi]);
    }
    ndk.arg("-o")
        .arg(jni_libs_dir)
        .args(["--platform", "31", "build", "-p", &project.app_crate()]);
    if release {
        ndk.arg("--release");
    }
    run_step(&format!("cargo ndk ({})", abis.join(", ")), &mut ndk)?;

    check_android_libraries_at(jni_libs_dir, &project.app_lib_name(), &abis)?;

    // 2. Gradle: package the APK.
    run_step(
        &format!("gradlew {}", gradle_task(release)),
        &mut gradle_command_at(
            &snapshot_root.join("mobile/android/gradle"),
            release,
            &abis,
            Some(jni_libs_dir),
            Some(gradle_build_dir),
        ),
    )?;

    let apk = apk_path_at(project, release, Some(gradle_build_dir))?;
    if let Some(identity) = &plan.debug_keystore_identity {
        identity.verify_unchanged()?;
    }
    publish_android_build_manifest(layout, &apk)?;
    println!(
        "  {} artifact manifest: {}",
        "✓".green(),
        layout.artifact_manifest_path().display()
    );
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
    let lease = DeviceLeaseSession::acquire(&project.root, serial)
        .context("acquiring the Android device lease")?;

    let bundle_id = bundle_id_of_android(project);

    println!("  {} installing on {}", "→".blue(), target.label());
    let mut runner = AndroidRunner::new(
        serial,
        apk,
        bundle_id.clone(),
        project.root.join(".gpui/runs"),
    );
    let request = RunRequest {
        run_id: mobile_run_id(&project.name, serial),
        project_id: project.name.clone(),
        device_id: serial.to_owned(),
        bundle_id,
        artifact_root: project.root.join(".gpui/runs"),
        abi: None,
    };
    let prepared = runner.prepare(&request, &lease)?;
    runner.launch(&prepared, &lease)?;
    lease
        .release()
        .map_err(anyhow::Error::new)
        .context("releasing the Android device lease")?;
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

        let isolated_jni = dir.path().join("isolated-jni");
        let isolated_gradle = dir.path().join("isolated-gradle");
        let isolated_command = gradle_command_with_outputs(
            &project,
            false,
            &abis,
            Some(&isolated_jni),
            Some(&isolated_gradle),
        );
        assert!(
            isolated_command.get_args().any(|arg| arg.to_string_lossy()
                == format!("-Pgpui.jniLibsDir={}", isolated_jni.display()))
        );
        assert!(
            isolated_command.get_args().any(|arg| arg.to_string_lossy()
                == format!("-Pgpui.buildDir={}", isolated_gradle.display()))
        );

        let snapshot_gradle = dir.path().join("snapshot/mobile/android/gradle");
        let snapshot_command = gradle_command_at(
            &snapshot_gradle,
            false,
            &abis,
            Some(&isolated_jni),
            Some(&isolated_gradle),
        );
        assert_eq!(
            snapshot_command.get_current_dir(),
            Some(snapshot_gradle.as_path())
        );
        assert_eq!(
            snapshot_command.get_program(),
            snapshot_gradle.join(if cfg!(windows) {
                "gradlew.bat"
            } else {
                "gradlew"
            })
        );
    }

    #[test]
    fn android_build_publishes_a_verified_manifest_for_jni_and_apk_outputs() {
        use crate::runner::build_key::{BuildKey, BuildKeyMaterial};
        use crate::runner::output_layout::BuildPlatform;

        let base = tempfile::tempdir().unwrap();
        let key = BuildKey::new(BuildKeyMaterial {
            source_manifest_hash: "source".into(),
            cargo_lock_hash: "lock".into(),
            target_triple: "aarch64-linux-android".into(),
            profile: "dev".into(),
            features: Vec::new(),
            abi: Some("arm64-v8a".into()),
            native_config_hash: "native".into(),
            toolchain_fingerprint: "toolchain".into(),
            relevant_env_hash: "env".into(),
            preview_registry_hash: "registry".into(),
        })
        .unwrap();
        let layout = BuildOutputLayout::for_key(base.path(), &key, BuildPlatform::Android).unwrap();
        layout.prepare().unwrap();
        let jni_libs_dir = layout.android_jni_dir.as_ref().unwrap();
        fs::create_dir_all(jni_libs_dir.join("arm64-v8a")).unwrap();
        fs::write(
            jni_libs_dir.join("arm64-v8a/libprobe_app.so"),
            b"jni library",
        )
        .unwrap();
        let gradle_build_dir = layout.android_gradle_build_dir.as_ref().unwrap();
        let apk_output_dir = gradle_build_dir.join("outputs/apk/debug");
        fs::create_dir_all(&apk_output_dir).unwrap();
        fs::write(apk_output_dir.join("app-debug.apk"), b"apk bytes").unwrap();
        fs::write(
            apk_output_dir.join("output-metadata.json"),
            br#"{"elements":[{"outputFile":"app-debug.apk"}]}"#,
        )
        .unwrap();

        let apk = apk_output_dir.join("app-debug.apk");
        let manifest = publish_android_build_manifest(&layout, &apk).unwrap();
        let project = project(base.path());
        let loaded = BuildArtifactManifest::read_verified(
            &layout.artifact_manifest_path(),
            &layout.root,
            BuildPlatform::Android,
            &key,
        )
        .unwrap();

        assert_eq!(manifest, loaded);
        assert_eq!(manifest.files.len(), 3);
        assert!(
            manifest
                .files
                .iter()
                .any(|file| file.path.ends_with("app-debug.apk"))
        );
        assert!(
            manifest
                .files
                .iter()
                .any(|file| file.path.ends_with("libprobe_app.so"))
        );
        assert!(matches!(
            lookup_verified_android_apk(&project, &layout, &key, false),
            AndroidBuildCacheLookup::Hit { .. }
        ));

        let unexpected = apk_output_dir.join("unexpected.apk");
        fs::write(&unexpected, b"extra output").unwrap();
        assert!(
            BuildArtifactManifest::read_verified(
                &layout.artifact_manifest_path(),
                &layout.root,
                BuildPlatform::Android,
                &key,
            )
            .is_err()
        );
        assert!(matches!(
            lookup_verified_android_apk(&project, &layout, &key, false),
            AndroidBuildCacheLookup::Miss(_)
        ));

        fs::remove_file(unexpected).unwrap();
        let decoy_output_dir = gradle_build_dir.join("outputs/apk/other");
        fs::create_dir_all(&decoy_output_dir).unwrap();
        fs::write(decoy_output_dir.join("decoy.apk"), b"not the expected apk").unwrap();
        let jni_root = jni_libs_dir.strip_prefix(&layout.root).unwrap().to_owned();
        let decoy_root = decoy_output_dir
            .strip_prefix(&layout.root)
            .unwrap()
            .to_owned();
        BuildArtifactManifest::capture(&layout, &[jni_root, decoy_root])
            .unwrap()
            .write_atomic(&layout.artifact_manifest_path())
            .unwrap();
        assert!(matches!(
            lookup_verified_android_apk(&project, &layout, &key, false),
            AndroidBuildCacheLookup::Miss(reason)
                if reason.contains("expected JNI and APK outputs")
        ));
    }

    #[test]
    fn ios_build_publishes_a_verified_manifest_for_the_app_bundle() {
        use crate::runner::build_key::{BuildKey, BuildKeyMaterial};
        use crate::runner::output_layout::BuildPlatform;

        let base = tempfile::tempdir().unwrap();
        let key = BuildKey::new(BuildKeyMaterial {
            source_manifest_hash: "source".into(),
            cargo_lock_hash: "lock".into(),
            target_triple: "aarch64-apple-ios-sim".into(),
            profile: "dev".into(),
            features: Vec::new(),
            abi: None,
            native_config_hash: "native".into(),
            toolchain_fingerprint: "toolchain".into(),
            relevant_env_hash: "env".into(),
            preview_registry_hash: "registry".into(),
        })
        .unwrap();
        let layout = BuildOutputLayout::for_key(base.path(), &key, BuildPlatform::Ios).unwrap();
        layout.prepare().unwrap();
        let derived_data = layout.ios_derived_data_dir.as_ref().unwrap();
        let app_path = derived_data.join("Build/Products/Debug-iphonesimulator/Probe.app");
        fs::create_dir_all(&app_path).unwrap();
        fs::write(app_path.join("Probe"), b"app executable").unwrap();
        fs::write(app_path.join("Info.plist"), b"plist fixture").unwrap();

        let manifest = publish_ios_build_manifest(&layout, &app_path).unwrap();
        let loaded = BuildArtifactManifest::read_verified(
            &layout.artifact_manifest_path(),
            &layout.root,
            BuildPlatform::Ios,
            &key,
        )
        .unwrap();

        assert_eq!(manifest, loaded);
        assert_eq!(manifest.files.len(), 2);
        assert!(
            manifest
                .files
                .iter()
                .any(|file| file.path.ends_with("Probe"))
        );
        assert!(
            manifest
                .files
                .iter()
                .any(|file| file.path.ends_with("Info.plist"))
        );
        assert!(matches!(
            lookup_verified_ios_app(&layout, &key, &app_path),
            BuildCacheLookup::Hit(_)
        ));
        assert!(matches!(
            lookup_ios_app_for_target(&layout, &key, &app_path, true),
            BuildCacheLookup::Miss(reason) if reason.contains("signing inputs")
        ));

        fs::write(app_path.join("unexpected-resource"), b"extra output").unwrap();
        assert!(
            BuildArtifactManifest::read_verified(
                &layout.artifact_manifest_path(),
                &layout.root,
                BuildPlatform::Ios,
                &key,
            )
            .is_err()
        );
        assert!(matches!(
            lookup_verified_ios_app(&layout, &key, &app_path),
            BuildCacheLookup::Miss(_)
        ));

        let decoy = layout.root.join("decoy-output");
        fs::write(&decoy, b"not an iOS app").unwrap();
        let decoy_root = decoy.strip_prefix(&layout.root).unwrap().to_owned();
        BuildArtifactManifest::capture(&layout, &[decoy_root])
            .unwrap()
            .write_atomic(&layout.artifact_manifest_path())
            .unwrap();
        assert!(matches!(
            lookup_verified_ios_app(&layout, &key, &app_path),
            BuildCacheLookup::Miss(reason) if reason.contains("expected iOS app bundle")
        ));
    }

    #[test]
    fn cargo_json_parser_selects_only_binary_artifacts_for_the_desktop_package() {
        let executable = "/tmp/builds/desktop/key/cargo-target/debug/probe-desktop";
        let desktop_bin = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "path+file:///tmp/project/crates/desktop#probe-desktop@0.1.0",
            "target": {"name": "probe-desktop", "kind": ["bin"]},
            "executable": executable
        })
        .to_string();
        let dependency_bin = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "registry+https://github.com/rust-lang/crates.io-index#other@1.0.0",
            "target": {"name": "other", "kind": ["bin"]},
            "executable": "/tmp/other"
        })
        .to_string();
        let desktop_library = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "path+file:///tmp/project/crates/desktop#probe-desktop@0.1.0",
            "target": {"name": "probe-desktop", "kind": ["lib"]},
            "executable": null
        })
        .to_string();

        let artifact = parse_cargo_binary_artifact(&desktop_bin, "probe-desktop").unwrap();
        assert_eq!(artifact.name, "probe-desktop");
        assert_eq!(artifact.executable, PathBuf::from(executable));
        assert!(parse_cargo_binary_artifact(&dependency_bin, "probe-desktop").is_none());
        assert!(parse_cargo_binary_artifact(&desktop_library, "probe-desktop").is_none());
        assert!(
            parse_cargo_binary_artifact(
                r#"{"reason":"compiler-message","message":{"rendered":"warning"}}"#,
                "probe-desktop"
            )
            .is_none()
        );
    }

    #[test]
    fn desktop_build_publishes_a_verified_manifest_for_cargo_binaries() {
        use crate::runner::build_key::{BuildKey, BuildKeyMaterial};
        use crate::runner::output_layout::BuildPlatform;

        let base = tempfile::tempdir().unwrap();
        let key = BuildKey::new(BuildKeyMaterial {
            source_manifest_hash: "source".into(),
            cargo_lock_hash: "lock".into(),
            target_triple: "x86_64-unknown-linux-gnu".into(),
            profile: "dev".into(),
            features: Vec::new(),
            abi: None,
            native_config_hash: "native".into(),
            toolchain_fingerprint: "toolchain".into(),
            relevant_env_hash: "env".into(),
            preview_registry_hash: "registry".into(),
        })
        .unwrap();
        let layout = BuildOutputLayout::for_key(base.path(), &key, BuildPlatform::Desktop).unwrap();
        layout.prepare().unwrap();
        let executable = layout.cargo_target_dir.join("debug/probe-desktop");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"desktop executable").unwrap();
        let artifacts = [CargoBinaryArtifact {
            name: "probe-desktop".into(),
            executable: executable.clone(),
        }];

        let manifest = publish_desktop_build_manifest(&layout, &artifacts).unwrap();
        let loaded = BuildArtifactManifest::read_verified(
            &layout.artifact_manifest_path(),
            &layout.root,
            BuildPlatform::Desktop,
            &key,
        )
        .unwrap();

        assert_eq!(manifest, loaded);
        assert_eq!(manifest.files.len(), 1);
        assert!(
            manifest.files[0]
                .path
                .ends_with("cargo-target/debug/probe-desktop")
        );

        fs::write(executable, b"changed executable bytes").unwrap();
        assert!(
            BuildArtifactManifest::read_verified(
                &layout.artifact_manifest_path(),
                &layout.root,
                BuildPlatform::Desktop,
                &key,
            )
            .is_err()
        );
    }
}
