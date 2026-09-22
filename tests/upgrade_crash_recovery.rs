//! Exercise upgrade crash recovery through separate CLI processes.

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, args: &[&str], crash_point: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gpui"));
    command.current_dir(root).args(args);
    if let Some(point) = crash_point {
        command.env("GPUI_UPGRADE_CRASH_POINT", point);
    } else {
        command.env_remove("GPUI_UPGRADE_CRASH_POINT");
    }
    command.output().unwrap()
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

fn transaction_id_from_lock(project: &Path) -> String {
    let lock = fs::read_to_string(project.join(".gpui/upgrade/upgrade.lock")).unwrap();
    lock.lines()
        .find_map(|line| line.strip_prefix("transaction_id="))
        .expect("crashed apply must leave a transaction id in the lock")
        .to_owned()
}

#[test]
fn crashed_apply_leaves_a_transaction_that_recover_cli_can_finish() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("crash-probe");
    let project_string = project.to_str().unwrap();
    let init = run(
        dir.path(),
        &[
            "init",
            "crash-probe",
            "--path",
            project_string,
            "--targets",
            "macos",
            "--bundle-id",
            "com.example.crashprobe",
        ],
        None,
    );
    assert_success(&init, "init");

    let plan = run(
        &project,
        ["upgrade", "plan", "--to", "agent-native-v1-draft", "--json"].as_slice(),
        None,
    );
    assert_success(&plan, "upgrade plan");
    let plan_json: Value = serde_json::from_slice(&plan.stdout).unwrap();
    let plan_id = plan_json["plan_id"].as_str().unwrap().to_owned();

    for point in [
        "after_journal",
        "after_backup",
        "before_validate",
        "after_validate",
    ] {
        let apply = run(
            &project,
            ["upgrade", "apply", "--plan", plan_id.as_str(), "--json"].as_slice(),
            Some(point),
        );
        assert_eq!(
            apply.status.code(),
            Some(75),
            "crash point {point} did not terminate the apply process: stdout={}, stderr={}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );

        let transaction_id = transaction_id_from_lock(&project);
        let journal_path = project
            .join(".gpui/upgrade/transactions")
            .join(&transaction_id)
            .join("journal.json");
        let journal: Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        assert_ne!(journal["state"], "committed", "crash point {point}");
        assert_ne!(journal["state"], "rolled_back", "crash point {point}");

        let recover = run(
            &project,
            [
                "upgrade",
                "recover",
                "--transaction",
                transaction_id.as_str(),
                "--json",
            ]
            .as_slice(),
            None,
        );
        assert_success(&recover, "upgrade recover");
        let recovery_json: Value = serde_json::from_slice(&recover.stdout).unwrap();
        assert_eq!(recovery_json["state"], "rolled_back", "crash point {point}");
        assert!(!project.join(".gpui/upgrade/upgrade.lock").exists());
    }
}
