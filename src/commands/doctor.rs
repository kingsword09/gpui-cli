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
    let mut report = probe_report(project.as_ref(), target, explicit, source)?;
    if let Some(device_check) = device_check(target, &args.device, project.as_ref()) {
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
    probe_report(Some(&project), target, false, "upgrade")
}

fn probe_report(
    project: Option<&Project>,
    target: Target,
    explicit: bool,
    source: &'static str,
) -> Result<DoctorReport> {
    let context = requirements_context(project);
    let requirements = requirements_for(&context, target)?;
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

fn extract_agp(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
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

fn device_check(
    target: Target,
    args: &DeviceArgs,
    project: Option<&Project>,
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
        Ok(device) => Some(CheckReport {
            id: id.into(),
            required,
            status: CheckStatus::Pass,
            expected: json!({"launchable": true}),
            actual: serde_json::to_value(device).unwrap_or_else(|_| json!({"available": true})),
            reason: "selected device is known and launchable".into(),
            remediation: Vec::new(),
            duration_ms: elapsed_ms(started),
            command: None,
        }),
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
}
