//! Verify stale-plan rejection through separate CLI processes.

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

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
fn cli_apply_rejects_an_edited_plan_before_creating_transaction_state() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("stale-probe");
    let project_string = project.to_str().unwrap();
    let init = run(
        dir.path(),
        &[
            "init",
            "stale-probe",
            "--path",
            project_string,
            "--targets",
            "macos",
            "--bundle-id",
            "com.example.staleprobe",
        ],
    );
    assert_success(&init, "init");

    let plan = run(
        &project,
        ["upgrade", "plan", "--to", "agent-native-v1-draft", "--json"].as_slice(),
    );
    assert_success(&plan, "upgrade plan");
    let plan_json: Value = serde_json::from_slice(&plan.stdout).unwrap();
    let plan_id = plan_json["plan_id"].as_str().unwrap();

    let managed = project.join("crates/app/src/lib.rs");
    let original = fs::read(&managed).unwrap();
    fs::write(
        &managed,
        [original.as_slice(), b"\n// user edit after plan\n"].concat(),
    )
    .unwrap();

    let apply = run(
        &project,
        ["upgrade", "apply", "--plan", plan_id, "--json"].as_slice(),
    );
    assert!(!apply.status.success());
    let stderr = String::from_utf8_lossy(&apply.stderr);
    assert!(stderr.contains("stale_plan"), "stderr={stderr}");
    assert_eq!(
        fs::read(&managed).unwrap(),
        [original.as_slice(), b"\n// user edit after plan\n"].concat()
    );
    assert!(!project.join(".gpui/upgrade/upgrade.lock").exists());
    assert!(!project.join(".gpui/upgrade/transactions").exists());
}
