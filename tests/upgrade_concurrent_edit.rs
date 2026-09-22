//! Verify cross-process locking and preservation of edits made mid-transaction.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const LEGACY_TEMPLATE_VERSION: &str = "agent-native-v0-legacy";
const CURRENT_TEMPLATE_VERSION: &str = "agent-native-v1-draft";
const LEGACY_LIB_PREFIX: &str = "// Historical agent-native-v0 baseline.\n";
const LEGACY_RUNTIME_SOURCE: &str = "//! Runtime marker removed by the agent-native-v1 template.\n\n\
pub const LEGACY_RUNTIME_REVISION: &str = \"agent-native-v0-legacy\";\n";

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

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_paused_apply(project: &Path, plan_id: &str, ready: &Path, continue_file: &Path) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gpui"));
    command
        .current_dir(project)
        .args(["upgrade", "apply", "--plan", plan_id, "--json"])
        .env("GPUI_UPGRADE_PAUSE_AFTER_PATH", "crates/app/src/lib.rs")
        .env("GPUI_UPGRADE_PAUSE_READY_FILE", ready)
        .env("GPUI_UPGRADE_PAUSE_CONTINUE_FILE", continue_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.spawn().unwrap()
}

fn only_transaction(project: &Path) -> String {
    let entries = fs::read_dir(project.join(".gpui/upgrade/transactions"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "unexpected transactions: {entries:?}");
    entries.into_iter().next().unwrap()
}

#[test]
fn concurrent_cli_edit_is_preserved_and_second_apply_is_locked_out() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("concurrent-probe");
    let project_string = project.to_str().unwrap();
    let init = run(
        dir.path(),
        &[
            "init",
            "concurrent-probe",
            "--path",
            project_string,
            "--targets",
            "macos",
            "--bundle-id",
            "com.example.concurrentprobe",
        ],
    );
    assert_success(&init, "init");

    let legacy_plan = run(
        &project,
        ["upgrade", "plan", "--to", LEGACY_TEMPLATE_VERSION, "--json"].as_slice(),
    );
    assert_success(&legacy_plan, "legacy target plan");
    let legacy_json: Value = serde_json::from_slice(&legacy_plan.stdout).unwrap();
    convert_project_to_legacy(
        &project,
        legacy_json["target"]["content_id"].as_str().unwrap(),
    );

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
    );
    assert_success(&plan, "upgrade plan");
    let plan_json: Value = serde_json::from_slice(&plan.stdout).unwrap();
    let plan_id = plan_json["plan_id"].as_str().unwrap().to_owned();

    let ready = dir.path().join("apply-ready");
    let continue_file = dir.path().join("apply-continue");
    let first = spawn_paused_apply(&project, &plan_id, &ready, &continue_file);
    wait_for_file(&ready);
    assert!(
        !fs::read_to_string(project.join("crates/app/src/lib.rs"))
            .unwrap()
            .starts_with(LEGACY_LIB_PREFIX)
    );

    let second = run(
        &project,
        ["upgrade", "apply", "--plan", plan_id.as_str(), "--json"].as_slice(),
    );
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("upgrade_busy"),
        "stderr={}",
        String::from_utf8_lossy(&second.stderr)
    );

    let user_edit = b"// user edit during upgrade\n";
    let lib_path = project.join("crates/app/src/lib.rs");
    let current_lib = fs::read(&lib_path).unwrap();
    fs::write(&lib_path, [current_lib.as_slice(), user_edit].concat()).unwrap();
    fs::write(&continue_file, b"continue").unwrap();

    let first_output = first.wait_with_output().unwrap();
    assert!(!first_output.status.success());
    let first_stderr = String::from_utf8_lossy(&first_output.stderr);
    assert!(
        first_stderr.contains("concurrent_edit"),
        "stderr={first_stderr}"
    );
    assert!(
        first_stderr.contains("preserved_user_changes"),
        "stderr={first_stderr}"
    );
    assert!(first_stderr.contains("crates/app/src/lib.rs"));

    assert_eq!(
        fs::read(&lib_path).unwrap(),
        [current_lib.as_slice(), user_edit].concat()
    );
    assert!(!project.join("crates/app/src/agent_runtime.rs").exists());
    assert_eq!(
        fs::read(project.join("crates/app/src/legacy_runtime.rs")).unwrap(),
        LEGACY_RUNTIME_SOURCE.as_bytes()
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(project.join(".gpui/template-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["template_version"], LEGACY_TEMPLATE_VERSION);
    assert!(!project.join(".gpui/upgrade/upgrade.lock").exists());

    let transaction_id = only_transaction(&project);
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
    );
    assert!(!recover.status.success());
    let recovery_json: Value = serde_json::from_slice(&recover.stdout).unwrap();
    assert_eq!(recovery_json["state"], "recovery_required");
    assert!(
        recovery_json["preserved_user_changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "crates/app/src/lib.rs")
    );
    assert!(!project.join(".gpui/upgrade/upgrade.lock").exists());
}
