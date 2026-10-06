//! Target-aware, read-only toolchain diagnosis.

use crate::DeviceArgs;
use crate::commands::run::Project;
use crate::device::{self, DeviceFlags, Platform as DevicePlatform};
use crate::toolchain::Target;
use crate::toolchain::probe::{ProbeConfig, ProbeRunner};
use crate::toolchain::report::{
    CheckReport, CheckStatus, DoctorReport, ProjectSummary, TargetSummary,
};
use crate::toolchain::requirements::{Context, requirements_for};
use anyhow::{Context as AnyhowContext, Result};
use clap::Args;
use colored::Colorize;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Instant;

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Diagnose one target; otherwise use the project's recorded target.
    #[arg(long, value_name = "desktop|ios|android")]
    pub target: Option<Target>,
    /// Emit schema-v2 JSON instead of the human report.
    #[arg(long)]
    pub json: bool,
    /// Device selection used for an explicit iOS/Android device check.
    #[command(flatten)]
    pub device: DeviceArgs,
}

pub fn handle_doctor(args: DoctorArgs) -> Result<i32> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    let has_project_markers = cwd.join("gpui.toml").exists();
    let project = match Project::load(None) {
        Ok(project) => Some(project),
        Err(_) if !has_project_markers => None,
        Err(error) => return Err(error),
    };
    let (target, explicit, source) = select_target(args.target, project.as_ref())?;
    let context = requirements_context(project.as_ref());
    let mut report = probe_report(project.as_ref(), target, explicit, source, &context)?;
    if let Some(device_check) = device_check(
        target,
        &args.device,
        project.as_ref(),
        &context.android_abis,
    ) {
        report.checks.push(device_check);
        report = DoctorReport::new(report.project, report.target, report.checks);
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_human(&report);
    }
    Ok(report.exit_code())
}

/// Runs the non-device portion of doctor for an existing generated project.
/// Upgrade validation uses this shared path so a missing native toolchain is
/// reported with the same bounded probe semantics as `gpui doctor`.
pub(crate) fn diagnose_project(root: &Path, target: Target) -> Result<DoctorReport> {
    let project = Project::load(Some(root.to_path_buf()))?;
    let context = requirements_context(Some(&project));
    probe_report(Some(&project), target, false, "upgrade", &context)
}

fn probe_report(
    project: Option<&Project>,
    target: Target,
    explicit: bool,
    source: &'static str,
    context: &Context,
) -> Result<DoctorReport> {
    let requirements = requirements_for(context, target)?;
    let probe_cwd = project.map(|project| {
        if target == Target::Android {
            project.android_gradle_dir()
        } else {
            project.root.clone()
        }
    });
    let mut runner = ProbeRunner::new(ProbeConfig::default());
    let checks = runner.probe(&requirements, probe_cwd.as_deref());
    Ok(DoctorReport::new(
        project_summary(project),
        TargetSummary {
            id: target,
            explicit,
            source: source.into(),
        },
        checks,
    ))
}

fn select_target(
    explicit: Option<Target>,
    project: Option<&Project>,
) -> Result<(Target, bool, &'static str)> {
    if let Some(target) = explicit {
        return Ok((target, true, "cli"));
    }
    if let Some(project) = project {
        let mut targets = project
            .targets
            .iter()
            .filter_map(|target| Target::parse(target))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            if project.has_desktop() {
                targets.push(Target::Desktop);
            }
            if project.ios_dir().exists() {
                targets.push(Target::Ios);
            }
            if project.android_gradle_dir().exists() {
                targets.push(Target::Android);
            }
        }
        targets.dedup();
        if let Some(target) = targets.into_iter().next() {
            return Ok((target, false, "project"));
        }
    }
    Ok((Target::Desktop, false, "host"))
}

fn requirements_context(project: Option<&Project>) -> Context {
    let mut context = Context::for_host(host_os());
    context.project_targets = project
        .map(|project| {
            project
                .targets
                .iter()
                .filter_map(|target| Target::parse(target))
                .collect()
        })
        .unwrap_or_default();
    if let Some(project) = project {
        context.project_agp = project_agp(project);
        context.project_rust_version = project_rust_version(project);
        context.android_compile_sdk =
            project_android_value(project, &["compileSdkVersion", "compileSdk"]);
        context.android_build_tools = project_android_value(project, &["buildToolsVersion"]);
        context.android_gradle_version = project_gradle_version(project);
        context.android_project = project.android_gradle_dir().is_dir();
    }
    if let Ok(abis) = std::env::var("GPUI_ANDROID_ABIS") {
        context.android_abis = abis
            .split(',')
            .map(str::trim)
            .filter(|abi| !abi.is_empty())
            .map(str::to_owned)
            .collect();
    }
    context
}

fn project_summary(project: Option<&Project>) -> ProjectSummary {
    let Some(project) = project else {
        return ProjectSummary {
            root: None,
            name: None,
            targets: Vec::new(),
        };
    };
    ProjectSummary {
        root: Some(project.root.display().to_string()),
        name: Some(project.name.clone()),
        targets: project
            .targets
            .iter()
            .filter_map(|target| Target::parse(target))
            .collect(),
    }
}

fn project_agp(project: &Project) -> Option<String> {
    let candidates = [
        project.android_gradle_dir().join("settings.gradle.kts"),
        project.android_gradle_dir().join("build.gradle.kts"),
        project.android_gradle_dir().join("app/build.gradle.kts"),
    ];
    candidates.iter().find_map(|path| extract_agp(path))
}

fn project_rust_version(project: &Project) -> Option<String> {
    let manifests = [
        project.root.join("crates/app/Cargo.toml"),
        project.root.join("Cargo.toml"),
    ];
    let mut workspace_version = None;
    for path in manifests {
        let Some(manifest) = fs::read_to_string(path)
            .ok()
            .and_then(|manifest| toml::from_str::<toml::Value>(&manifest).ok())
        else {
            continue;
        };
        let package_version = manifest
            .get("package")
            .and_then(|package| package.get("rust-version"))
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        if package_version.is_some() {
            return package_version;
        }
        workspace_version = workspace_version.or_else(|| {
            manifest
                .get("workspace")
                .and_then(|workspace| workspace.get("package"))
                .and_then(|package| package.get("rust-version"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        });
    }
    workspace_version
}

fn project_android_value(project: &Project, keys: &[&str]) -> Option<String> {
    let candidates = [
        project.android_gradle_dir().join("app/build.gradle.kts"),
        project.android_gradle_dir().join("app/build.gradle"),
        project.android_gradle_dir().join("build.gradle.kts"),
        project.android_gradle_dir().join("build.gradle"),
    ];
    candidates.iter().find_map(|path| {
        let text = fs::read_to_string(path).ok()?;
        text.lines()
            .filter_map(|line| gradle_literal(line, keys))
            .next()
    })
}

fn gradle_literal(line: &str, keys: &[&str]) -> Option<String> {
    let line = line.split("//").next()?.trim();
    if line.is_empty() || line.starts_with("*") {
        return None;
    }
    for key in keys {
        let Some(index) = line.find(key) else {
            continue;
        };
        let mut value = line[index + key.len()..].trim_start();
        if let Some(rest) = value.strip_prefix('=') {
            value = rest.trim_start();
        } else if let Some(rest) = value.strip_prefix('(') {
            value = rest.trim_start();
        }
        if let Some(quote) = value
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"'))
        {
            let value = &value[quote.len_utf8()..];
            if let Some(end) = value.find(quote) {
                let literal = &value[..end];
                if !literal.is_empty()
                    && literal
                        .chars()
                        .all(|character| character.is_ascii_digit() || character == '.')
                {
                    return Some(literal.to_owned());
                }
            }
        } else {
            let literal = value
                .chars()
                .take_while(|character| character.is_ascii_digit() || *character == '.')
                .collect::<String>();
            if !literal.is_empty() && literal.chars().any(|character| character.is_ascii_digit()) {
                return Some(literal);
            }
        }
    }
    None
}

fn project_gradle_version(project: &Project) -> Option<String> {
    let path = project
        .android_gradle_dir()
        .join("gradle/wrapper/gradle-wrapper.properties");
    let properties = fs::read_to_string(path).ok()?;
    properties.lines().find_map(|line| {
        let url = line.trim().strip_prefix("distributionUrl=")?;
        let filename = url.rsplit('/').next()?;
        let version = filename
            .strip_prefix("gradle-")?
            .strip_suffix("-bin.zip")
            .or_else(|| filename.strip_prefix("gradle-")?.strip_suffix("-all.zip"))?;
        is_numeric_version(version).then(|| version.to_owned())
    })
}

fn is_numeric_version(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn extract_agp(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or_default();
        if let Some(version) = extract_agp_classpath(line) {
            return Some(version);
        }
        if !line.contains("com.android") || !line.contains("version") {
            continue;
        }
        let after = line.split("version").nth(1)?;
        let start = after.find(['"', '\''])? + 1;
        let tail = &after[start..];
        let end = tail.find(['"', '\''])?;
        return Some(tail[..end].to_string());
    }
    None
}

fn extract_agp_classpath(line: &str) -> Option<String> {
    let coordinate = "com.android.tools.build:gradle:";
    let after = line.split_once(coordinate)?.1;
    let version = after
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect::<String>();
    (is_numeric_version(&version) && version.matches('.').count() >= 2).then_some(version)
}

fn device_check(
    target: Target,
    args: &DeviceArgs,
    project: Option<&Project>,
    configured_android_abis: &[String],
) -> Option<CheckReport> {
    let selected =
        args.device.is_some() || args.sim.is_some() || args.avd.is_some() || args.device_only;
    if !selected {
        return None;
    }
    let started = Instant::now();
    let required = true;
    let (id, result) = match target {
        Target::Ios => (
            "ios.selected_device",
            device::inventory::resolve_device(
                DevicePlatform::Ios,
                &DeviceFlags::from(args.clone()),
                &project
                    .map(|project| project.defaults.clone())
                    .unwrap_or_default(),
                None,
            ),
        ),
        Target::Android => (
            "android.selected_device",
            device::inventory::resolve_device(
                DevicePlatform::Android,
                &DeviceFlags::from(args.clone()),
                &project
                    .map(|project| project.defaults.clone())
                    .unwrap_or_default(),
                None,
            ),
        ),
        Target::Desktop => {
            return Some(CheckReport {
                id: "desktop.device_selection".into(),
                required,
                status: CheckStatus::Fail,
                expected: json!({"selection": "not_applicable"}),
                actual: json!({"selection": "provided"}),
                reason: "device flags are only valid with --target ios or --target android".into(),
                remediation: Vec::new(),
                duration_ms: elapsed_ms(started),
                command: None,
            });
        }
    };
    match result {
        Ok(device) => {
            let (status, expected, reason) = if target == Target::Android {
                let build_abis = effective_android_abis(configured_android_abis);
                let (status, reason) =
                    android_device_abi_status(device.arch.as_deref(), &build_abis);
                (
                    status,
                    json!({"launchable": true, "build_abis": build_abis}),
                    reason,
                )
            } else {
                (
                    CheckStatus::Pass,
                    json!({"launchable": true}),
                    "selected device is known and launchable".into(),
                )
            };
            Some(CheckReport {
                id: id.into(),
                required,
                status,
                expected,
                actual: serde_json::to_value(device).unwrap_or_else(|_| json!({"available": true})),
                reason,
                remediation: Vec::new(),
                duration_ms: elapsed_ms(started),
                command: None,
            })
        }
        Err(error) => Some(CheckReport {
            id: id.into(),
            required,
            status: CheckStatus::Fail,
            expected: json!({"launchable": true}),
            actual: json!({"available": false}),
            reason: error.to_string(),
            remediation: Vec::new(),
            duration_ms: elapsed_ms(started),
            command: None,
        }),
    }
}

fn effective_android_abis(configured: &[String]) -> Vec<String> {
    if configured.is_empty() {
        vec!["arm64-v8a".into()]
    } else {
        configured.to_vec()
    }
}

fn android_device_abi_status(
    device_abi: Option<&str>,
    build_abis: &[String],
) -> (CheckStatus, String) {
    let Some(device_abi) = device_abi.map(str::trim).filter(|abi| !abi.is_empty()) else {
        return (
            CheckStatus::Unknown,
            "selected Android device ABI is unavailable; compatibility cannot be confirmed".into(),
        );
    };
    if build_abis.iter().any(|abi| abi == device_abi) {
        (
            CheckStatus::Pass,
            format!(
                "selected Android device ABI {device_abi} matches the configured build ABI set"
            ),
        )
    } else {
        (
            CheckStatus::Fail,
            format!(
                "selected Android device ABI {device_abi} is not in configured build ABIs [{}]",
                build_abis.join(", ")
            ),
        )
    }
}

fn render_human(report: &DoctorReport) {
    println!(
        "{}",
        format!(
            "
🩺 GPUI doctor · {}
",
            report.target.id.label()
        )
        .bold()
        .cyan()
    );
    if let Some(name) = &report.project.name {
        println!("Project: {}", name.bold());
    } else {
        println!("Project: {}", "not found (host-only report)".dimmed());
    }
    for check in &report.checks {
        let icon = match check.status {
            CheckStatus::Pass => "✓".green(),
            CheckStatus::Warning => "!".yellow(),
            CheckStatus::Fail => "✗".red(),
            CheckStatus::Unavailable | CheckStatus::Unknown => "?".yellow(),
        };
        let required = if check.required {
            "required"
        } else {
            "optional"
        };
        println!(
            "  {} {:<34} [{}] {}",
            icon, check.id, required, check.reason
        );
        for remediation in &check.remediation {
            println!(
                "      {} {:?} — {}",
                "suggest:".dimmed(),
                remediation.argv,
                remediation.description.dimmed()
            );
        }
    }
    let overall = match report.overall {
        CheckStatus::Pass => "PASS".green(),
        CheckStatus::Warning => "WARNING".yellow(),
        CheckStatus::Fail => "FAIL".red(),
        CheckStatus::Unavailable | CheckStatus::Unknown => "UNKNOWN".yellow(),
    };
    println!(
        "
Overall: {}",
        overall.bold()
    );
}

fn host_os() -> String {
    match std::env::consts::OS {
        "macos" => "macos".into(),
        "windows" => "windows".into(),
        "linux" => "linux".into(),
        other => other.into(),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::run::Project;

    #[test]
    fn target_selection_prefers_explicit_target() {
        let selected = select_target(Some(Target::Android), None).unwrap();
        assert_eq!(selected, (Target::Android, true, "cli"));
    }

    #[test]
    fn agp_extraction_does_not_require_a_gradle_parser() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build.gradle.kts");
        fs::write(
            &path,
            r#"plugins { id("com.android.application") version "8.9.0" apply false }
"#,
        )
        .unwrap();
        assert_eq!(extract_agp(&path).as_deref(), Some("8.9.0"));
    }

    #[test]
    fn agp_extraction_reads_fixed_buildscript_coordinates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build.gradle.kts");
        fs::write(
            &path,
            "dependencies { classpath(\"com.android.tools.build:gradle:9.1.0\") }\n",
        )
        .unwrap();

        assert_eq!(extract_agp(&path).as_deref(), Some("9.1.0"));
    }

    #[test]
    fn agp_extraction_keeps_dynamic_coordinates_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build.gradle.kts");
        fs::write(
            &path,
            "dependencies { classpath(\"com.android.tools.build:gradle:$agpVersion\") }\n",
        )
        .unwrap();

        assert_eq!(extract_agp(&path), None);
    }

    #[test]
    fn project_toolchain_requirements_use_literal_manifest_values() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().into(),
            name: "probe".into(),
            title: "Probe".into(),
            targets: vec!["android".into()],
            defaults: Default::default(),
        };
        let android = project.android_gradle_dir();
        fs::create_dir_all(android.join("app")).unwrap();
        fs::create_dir_all(android.join("gradle/wrapper")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace.package]\nrust-version = \"1.97\"\n",
        )
        .unwrap();
        fs::write(
            android.join("app/build.gradle.kts"),
            "android { compileSdk = 34; buildToolsVersion = \"35.0.0\" }\n",
        )
        .unwrap();
        fs::write(
            android.join("gradle/wrapper/gradle-wrapper.properties"),
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.4.1-bin.zip\n",
        )
        .unwrap();

        assert_eq!(project_rust_version(&project).as_deref(), Some("1.97"));
        assert_eq!(
            project_android_value(&project, &["compileSdkVersion", "compileSdk"]).as_deref(),
            Some("34")
        );
        assert_eq!(
            project_android_value(&project, &["buildToolsVersion"]).as_deref(),
            Some("35.0.0")
        );
        assert_eq!(project_gradle_version(&project).as_deref(), Some("9.4.1"));
    }

    #[test]
    fn project_toolchain_requirements_ignore_comments_and_dynamic_versions() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().into(),
            name: "probe".into(),
            title: "Probe".into(),
            targets: vec!["android".into()],
            defaults: Default::default(),
        };
        let app = project.android_gradle_dir().join("app");
        fs::create_dir_all(&app).unwrap();
        fs::write(
            app.join("build.gradle"),
            "// compileSdk 34\nandroid { compileSdk libs.versions.androidSdk.get().toInteger() }\n",
        )
        .unwrap();

        assert_eq!(
            project_android_value(&project, &["compileSdk"]).as_deref(),
            None
        );
    }

    #[test]
    fn android_selected_device_abi_must_match_a_configured_build_abi() {
        let default_abis = effective_android_abis(&[]);
        let (default_status, default_reason) =
            android_device_abi_status(Some("arm64-v8a"), &default_abis);
        assert_eq!(default_status, CheckStatus::Pass);
        assert!(default_reason.contains("matches"));

        let configured = vec!["arm64-v8a".to_string()];
        let (mismatch_status, mismatch_reason) =
            android_device_abi_status(Some("x86_64"), &configured);
        assert_eq!(mismatch_status, CheckStatus::Fail);
        assert!(mismatch_reason.contains("not in configured build ABIs"));

        let configured_multiple = vec!["arm64-v8a".to_string(), "x86_64".to_string()];
        let (multi_status, _) = android_device_abi_status(Some("x86_64"), &configured_multiple);
        assert_eq!(multi_status, CheckStatus::Pass);

        let (unknown_status, unknown_reason) = android_device_abi_status(None, &configured);
        assert_eq!(unknown_status, CheckStatus::Unknown);
        assert!(unknown_reason.contains("cannot be confirmed"));
    }
}
