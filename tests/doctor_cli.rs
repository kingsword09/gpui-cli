//! Run the target-aware doctor through the shipped CLI against a generated project.

use serde_json::Value;
use std::env;
use std::fs;
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

fn doctor_with_path(root: &Path, args: &[&str], path: &Path) -> Output {
    let current_path = env::var_os("PATH").unwrap_or_default();
    let mut path_value = path.as_os_str().to_owned();
    path_value.push(if cfg!(windows) { ";" } else { ":" });
    path_value.push(current_path);
    Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(root)
        .args(args)
        .env("PATH", path_value)
        .output()
        .unwrap()
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

#[test]
fn doctor_cli_reports_a_nonzero_required_probe() {
    let dir = tempfile::tempdir().unwrap();
    let shim_dir = dir.path().join("shims");
    fs::create_dir(&shim_dir).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let shim = shim_dir.join("cc");
        fs::write(&shim, "#!/bin/sh\nexit 42\n").unwrap();
        let mut permissions = fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&shim, permissions).unwrap();
    }
    #[cfg(windows)]
    fs::write(shim_dir.join("cc.cmd"), "@echo off\r\nexit /b 42\r\n").unwrap();

    let output = doctor_with_path(
        dir.path(),
        &["doctor", "--json", "--target", "desktop"],
        &shim_dir,
    );
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["overall"], "fail");
    assert_eq!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["id"] == "desktop.c_compiler")
            .unwrap()["status"],
        "fail"
    );
}
