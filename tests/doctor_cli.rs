//! Run the target-aware doctor through the shipped CLI against a generated project.

use serde_json::Value;
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

    let doctor = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(&project)
        .args(["doctor", "--json", "--target", "desktop"])
        .output()
        .unwrap();
    assert_success(&doctor, "doctor --target desktop");

    let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["target"]["id"], "desktop");
    assert_eq!(report["target"]["explicit"], true);
    assert_eq!(report["target"]["source"], "cli");
    assert!(matches!(
        report["overall"].as_str(),
        Some("pass") | Some("warning")
    ));

    let checks = report["checks"].as_array().unwrap();
    assert!(!checks.is_empty());
    assert!(
        checks
            .iter()
            .filter(|check| check["required"] == true)
            .all(|check| { check["status"] == "pass" })
    );
    assert!(checks.iter().all(|check| {
        let id = check["id"].as_str().unwrap_or_default();
        !id.starts_with("android.") && !id.starts_with("ios.")
    }));
}
