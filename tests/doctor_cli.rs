//! Run the target-aware doctor through the shipped CLI against a generated project.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed: status={:?}, stdout={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn doctor(root: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert_success(&output, "doctor");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_desktop_report(report: &Value, source: &str, project_present: bool) {
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["target"]["id"], "desktop");
    assert_eq!(report["target"]["explicit"], source == "cli");
    assert_eq!(report["target"]["source"], source);
    assert!(matches!(
        report["overall"].as_str(),
        Some("pass") | Some("warning")
    ));
    assert_eq!(report["project"]["name"].is_null(), !project_present);

    let checks = report["checks"].as_array().unwrap();
    assert!(!checks.is_empty());
    assert!(
        checks
            .iter()
            .filter(|check| check["required"] == true)
            .all(|check| check["status"] == "pass")
    );
    assert!(checks.iter().all(|check| {
        let id = check["id"].as_str().unwrap_or_default();
        !id.starts_with("android.") && !id.starts_with("ios.")
    }));
}

#[test]
fn generated_desktop_doctor_reports_target_aware_schema() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("doctor-cli-probe");
    let project_arg = project.to_str().unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(dir.path())
        .args([
            "init",
            "doctor-cli-probe",
            "--path",
            project_arg,
            "--targets",
            "macos",
            "--bundle-id",
            "com.example.doctorcliprobe",
        ])
        .output()
        .unwrap();
    assert_success(&init, "init");

    let explicit = doctor(&project, &["doctor", "--json", "--target", "desktop"]);
    assert_desktop_report(&explicit, "cli", true);

    let project_default = doctor(&project, &["doctor", "--json"]);
    assert_desktop_report(&project_default, "project", true);
}

#[test]
fn doctor_without_a_project_reports_host_only_target() {
    let dir = tempfile::tempdir().unwrap();
    let report = doctor(dir.path(), &["doctor", "--json", "--target", "desktop"]);
    assert_desktop_report(&report, "cli", false);
}
