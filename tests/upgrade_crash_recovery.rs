//! Exercise multi-file upgrade crash recovery through separate CLI processes.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const LEGACY_TEMPLATE_VERSION: &str = "agent-native-v0-legacy";
const CURRENT_TEMPLATE_VERSION: &str = "agent-native-v1-draft";
const LEGACY_LIB_PREFIX: &str = "// Historical agent-native-v0 baseline.\n";
const LEGACY_RUNTIME_SOURCE: &str = "//! Runtime marker removed by the agent-native-v1 template.\n\n\
pub const LEGACY_RUNTIME_REVISION: &str = \"agent-native-v0-legacy\";\n";

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

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn convert_project_to_legacy(root: &Path, content_id: &str) {
    let lib_path = root.join("crates/app/src/lib.rs");
    let current_lib = fs::read(&lib_path).unwrap();
    let legacy_lib = [LEGACY_LIB_PREFIX.as_bytes(), current_lib.as_slice()].concat();
    fs::write(&lib_path, &legacy_lib).unwrap();

    fs::remove_file(root.join("crates/app/src/agent_runtime.rs")).unwrap();
    fs::write(
        root.join("crates/app/src/legacy_runtime.rs"),
        LEGACY_RUNTIME_SOURCE,
    )
    .unwrap();

    let manifest_path = root.join(".gpui/template-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["template_version"] = json!(LEGACY_TEMPLATE_VERSION);
    manifest["baseline"]["template_version"] = json!(LEGACY_TEMPLATE_VERSION);
    manifest["baseline"]["content_id"] = json!(content_id);

    let files = manifest["files"].as_array_mut().unwrap();
    let mut migrated = Vec::with_capacity(files.len() + 1);
    for mut entry in files.drain(..) {
        let path = entry["path"].as_str().unwrap().to_owned();
        if path == "crates/app/src/agent_runtime.rs" {
            continue;
        }
        if path == "crates/app/src/lib.rs" {
            entry["base_sha256"] = json!(hash(&legacy_lib));
        }
        migrated.push(entry);
    }
    migrated.push(json!({
        "path": "crates/app/src/legacy_runtime.rs",
        "group": "app-runtime",
        "base_sha256": hash(LEGACY_RUNTIME_SOURCE.as_bytes()),
        "template_path": "legacy/app/src/legacy_runtime.rs"
    }));
    migrated.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap()
            .cmp(right["path"].as_str().unwrap())
    });
    manifest["files"] = Value::Array(migrated);

    let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    bytes.push(b'\n');
    fs::write(manifest_path, bytes).unwrap();
}

fn snapshot(root: &Path) -> Vec<(&'static str, Option<Vec<u8>>)> {
    [
        "crates/app/src/lib.rs",
        "crates/app/src/agent_runtime.rs",
        "crates/app/src/legacy_runtime.rs",
        ".gpui/template-manifest.json",
    ]
    .into_iter()
    .map(|relative| {
        let path = root.join(relative);
        (relative, fs::read(path).ok())
    })
    .collect()
}

fn transaction_id_from_lock(project: &Path) -> String {
    let lock = fs::read_to_string(project.join(".gpui/upgrade/upgrade.lock")).unwrap();
    lock.lines()
        .find_map(|line| line.strip_prefix("transaction_id="))
        .expect("crashed apply must leave a transaction id in the lock")
        .to_owned()
}

#[test]
fn crashed_multifile_apply_recovers_every_transaction_boundary() {
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

    let legacy_plan = run(
        &project,
        ["upgrade", "plan", "--to", LEGACY_TEMPLATE_VERSION, "--json"].as_slice(),
        None,
    );
    assert_success(&legacy_plan, "legacy target plan");
    let legacy_plan_json: Value = serde_json::from_slice(&legacy_plan.stdout).unwrap();
    let legacy_content_id = legacy_plan_json["target"]["content_id"]
        .as_str()
        .unwrap()
        .to_owned();
    convert_project_to_legacy(&project, &legacy_content_id);
    let baseline = snapshot(&project);

    let plan = run(
        &project,
        [
            "upgrade",
            "plan",
            "--to",
            CURRENT_TEMPLATE_VERSION,
            "--json",
        ]
        .as_slice(),
        None,
    );
    assert_success(&plan, "upgrade plan");
    let plan_json: Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan_json["status"], "ready");
    assert_eq!(
        plan_json["base"]["template_version"],
        LEGACY_TEMPLATE_VERSION
    );
    assert_eq!(
        plan_json["target"]["template_version"],
        CURRENT_TEMPLATE_VERSION
    );
    let plan_id = plan_json["plan_id"].as_str().unwrap().to_owned();

    for point in [
        "after_journal",
        "after_backup",
        "before_replace",
        "after_replace",
        "before_manifest",
        "after_manifest",
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
        assert_eq!(snapshot(&project), baseline, "crash point {point}");
        assert!(!project.join(".gpui/upgrade/upgrade.lock").exists());
    }
}
