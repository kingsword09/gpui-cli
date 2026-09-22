//! Verify a real CLI migration between two embedded template revisions.

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

fn action_count(plan: &Value, action: &str) -> usize {
    plan["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|file| file["action"].as_str() == Some(action))
        .count()
}

#[test]
fn cli_migrates_legacy_revision_with_replace_add_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("revision-probe");
    let project_string = project.to_str().unwrap();

    let init = run(
        dir.path(),
        &[
            "init",
            "revision-probe",
            "--path",
            project_string,
            "--targets",
            "macos",
            "--bundle-id",
            "com.example.revisionprobe",
        ],
    );
    assert_success(&init, "init");

    let legacy_plan = run(
        &project,
        ["upgrade", "plan", "--to", LEGACY_TEMPLATE_VERSION, "--json"].as_slice(),
    );
    assert_success(&legacy_plan, "legacy target plan");
    let legacy_plan_json: Value = serde_json::from_slice(&legacy_plan.stdout).unwrap();
    let legacy_content_id = legacy_plan_json["target"]["content_id"]
        .as_str()
        .unwrap()
        .to_owned();
    convert_project_to_legacy(&project, &legacy_content_id);

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
    assert_success(&plan, "current target plan");
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
    assert_eq!(action_count(&plan_json, "replace"), 1);
    assert_eq!(action_count(&plan_json, "add"), 1);
    assert_eq!(action_count(&plan_json, "delete"), 1);
    let plan_id = plan_json["plan_id"].as_str().unwrap();

    let apply = run(
        &project,
        ["upgrade", "apply", "--plan", plan_id, "--json"].as_slice(),
    );
    assert_success(&apply, "apply revision migration");
    let apply_json: Value = serde_json::from_slice(&apply.stdout).unwrap();
    assert!(apply_json["files_applied"].as_u64().unwrap() >= 3);
    assert!(matches!(
        apply_json["validation"]["status"].as_str(),
        Some("passed") | Some("not_run")
    ));

    assert!(!project.join("crates/app/src/legacy_runtime.rs").exists());
    assert!(project.join("crates/app/src/agent_runtime.rs").is_file());
    let lib = fs::read(project.join("crates/app/src/lib.rs")).unwrap();
    assert!(!lib.starts_with(LEGACY_LIB_PREFIX.as_bytes()));

    let manifest: Value =
        serde_json::from_slice(&fs::read(project.join(".gpui/template-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["template_version"], CURRENT_TEMPLATE_VERSION);
    assert_eq!(
        manifest["baseline"]["template_version"],
        CURRENT_TEMPLATE_VERSION
    );
    assert!(!project.join(".gpui/upgrade/upgrade.lock").exists());
}
