//! Run the target-aware doctor through the shipped CLI against a generated project.

use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, OnceLock};

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

fn compile_native_helper(root: &Path, name: &str, source: &str) -> PathBuf {
    static RUSTC_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = RUSTC_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let source_path = root.join(format!("{name}-shim.rs"));
    fs::write(&source_path, source).unwrap();
    let shim_dir = root.join("shims");
    let shim = shim_dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    });
    let rustc_status = Command::new("rustc")
        .arg("--edition=2021")
        .arg(source_path)
        .arg("-o")
        .arg(&shim)
        .status()
        .unwrap();
    assert!(rustc_status.success(), "failed to compile {name} shim");
    shim
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

    // `Command::new("cc")` does not invoke a shell on any platform.  A text
    // shim (`.cmd` on Windows or a shell script on Unix) would therefore not
    // exercise the probe consistently.  Compile a tiny native executable
    // with the same Rust toolchain that is running this test instead.
    compile_native_helper(dir.path(), "cc", "fn main() { std::process::exit(42); }\n");

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

#[test]
fn doctor_cli_rejects_an_unparseable_successful_version_probe() {
    let dir = tempfile::tempdir().unwrap();
    let shim_dir = dir.path().join("shims");
    fs::create_dir(&shim_dir).unwrap();

    // Keep the helper native for the same reason as the nonzero probe above:
    // `Command::new` does not invoke a platform shell.
    compile_native_helper(
        dir.path(),
        "rustc",
        "fn main() { print!(\"rustc definitely-not-a-version\\n\"); }\n",
    );

    let output = doctor_with_path(
        dir.path(),
        &["doctor", "--json", "--target", "desktop"],
        &shim_dir,
    );
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["overall"], "fail");
    let rustc_check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "rust.rustc")
        .unwrap();
    assert_eq!(rustc_check["status"], "fail");
    assert!(rustc_check["reason"].as_str().unwrap().contains("version"));
}
