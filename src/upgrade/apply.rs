//! Plan revalidation, project locking, and journal-backed apply.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::lock::UpgradeLock;
use super::transaction::{
    FileState, JournalFile, TransactionJournal, TransactionState, TransactionStore,
    recover_transaction,
};
use super::{FileAction, PlanStatus, hash_file, load_plan, load_project, plan_project};
use crate::template::scaffold;
use crate::template_manifest::{MANIFEST_RELATIVE_PATH, TemplateManifest};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationReport {
    pub status: String,
    pub commands: Vec<Vec<String>>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyReport {
    pub transaction_id: String,
    pub plan_id: String,
    pub state: TransactionState,
    pub files_applied: usize,
    pub validation: ValidationReport,
}

pub fn apply_cached_plan(root: &Path, plan_id: &str) -> Result<ApplyReport> {
    let stored = load_plan(root, plan_id)?;
    if stored.status != PlanStatus::Ready {
        bail!(
            "plan '{}' is not applicable: status={:?}",
            plan_id,
            stored.status
        );
    }
    let transaction_id = new_transaction_id()?;
    let _lock = UpgradeLock::acquire(root, &transaction_id)?;

    let current = plan_project(root, &stored.target.template_version)?;
    if current.plan_id != stored.plan_id {
        bail!(
            "stale_plan: current project no longer matches plan '{}'",
            stored.plan_id
        );
    }
    if current.status != PlanStatus::Ready {
        bail!(
            "plan '{}' became non-applicable: status={:?}",
            stored.plan_id,
            current.status
        );
    }

    let project = load_project(root)?;
    let target_root = tempfile::tempdir().context("preparing target template")?;
    scaffold(target_root.path(), &project.config)?;
    let target_manifest = TemplateManifest::read(target_root.path())?
        .context("target template did not produce a manifest")?;

    let mut target_bytes = BTreeMap::new();
    let mut journal_files = Vec::new();
    for file in &current.files {
        if !matches!(
            file.action,
            FileAction::Replace | FileAction::Add | FileAction::Delete
        ) {
            continue;
        }
        let target = match file.target_sha256.as_deref() {
            Some(expected) => {
                let bytes = fs::read(target_root.path().join(&file.path))
                    .with_context(|| format!("reading target file '{}'", file.path))?;
                if hash_bytes(&bytes) != expected {
                    bail!("target template hash mismatch for '{}'", file.path);
                }
                Some(bytes)
            }
            None => None,
        };
        target_bytes.insert(file.path.clone(), target);
        journal_files.push(JournalFile {
            path: file.path.clone(),
            old_sha256: file.local_sha256.clone(),
            new_sha256: file.target_sha256.clone(),
            backup_path: file
                .local_sha256
                .as_ref()
                .map(|_| TransactionStore::backup_path_for(&file.path)),
            state: FileState::Planned,
        });
    }

    let target_manifest_bytes = fs::read(TemplateManifest::path(target_root.path()))?;
    let current_manifest_path = root.join(MANIFEST_RELATIVE_PATH);
    let current_manifest_hash = hash_file(&current_manifest_path)?;
    let target_manifest_hash = hash_bytes(&target_manifest_bytes);
    if current_manifest_hash.as_deref() != Some(target_manifest_hash.as_str()) {
        target_bytes.insert(
            MANIFEST_RELATIVE_PATH.to_owned(),
            Some(target_manifest_bytes),
        );
        journal_files.push(JournalFile {
            path: MANIFEST_RELATIVE_PATH.to_owned(),
            old_sha256: current_manifest_hash,
            new_sha256: Some(target_manifest_hash),
            backup_path: hash_file(&current_manifest_path)?
                .map(|_| TransactionStore::backup_path_for(MANIFEST_RELATIVE_PATH)),
            state: FileState::Planned,
        });
    }

    let (store, mut journal) =
        TransactionStore::create(root, &transaction_id, &stored.plan_id, journal_files)?;
    backup_files(root, &store, &mut journal)?;
    journal.state = TransactionState::Writing;
    store.write(&journal)?;

    let applied = match write_files(root, &store, &mut journal, &target_bytes) {
        Ok(applied) => applied,
        Err(error) => {
            journal.state = TransactionState::RecoveryRequired;
            store.write(&journal)?;
            let recovery = recover_transaction(root, &transaction_id)?;
            bail!(
                "upgrade apply failed: {error:#}; recovery_state={:?}, \
                 preserved_user_changes={:?}, recovery_errors={:?}",
                recovery.state,
                recovery.preserved_user_changes,
                recovery.errors
            );
        }
    };

    journal.state = TransactionState::Validating;
    store.write(&journal)?;
    let validation = validate_result(root, &target_manifest)?;

    journal.state = TransactionState::Committed;
    store.write(&journal)?;
    Ok(ApplyReport {
        transaction_id,
        plan_id: stored.plan_id,
        state: journal.state,
        files_applied: applied,
        validation,
    })
}

fn backup_files(
    root: &Path,
    store: &TransactionStore,
    journal: &mut TransactionJournal,
) -> Result<()> {
    for index in 0..journal.files.len() {
        let file = &journal.files[index];
        let Some(old_hash) = &file.old_sha256 else {
            continue;
        };
        let path = store.path(&file.path)?;
        let bytes = fs::read(&path).with_context(|| format!("backing up '{}'", file.path))?;
        if hash_bytes(&bytes) != *old_hash {
            bail!(
                "stale_plan: '{}' changed before backup; no files were modified",
                file.path
            );
        }
        let backup = file
            .backup_path
            .as_deref()
            .context("journal file with old hash has no backup path")?;
        store.write_backup(backup, &bytes)?;
        journal.files[index].state = FileState::BackedUp;
        store.write(journal)?;
    }
    let _ = root;
    Ok(())
}

fn write_files(
    root: &Path,
    store: &TransactionStore,
    journal: &mut TransactionJournal,
    target_bytes: &BTreeMap<String, Option<Vec<u8>>>,
) -> Result<usize> {
    let mut applied = 0;
    for index in 0..journal.files.len() {
        let path = journal.files[index].path.clone();
        let expected_old = journal.files[index].old_sha256.clone();
        if hash_file(&root.join(&path))? != expected_old {
            bail!("concurrent_edit: '{}' changed before replacement", path);
        }
        journal.files[index].state = FileState::Replaced;
        store.write(journal)?;
        match target_bytes
            .get(&path)
            .context("journal file has no prepared target bytes")?
        {
            Some(bytes) => store.write_project_file(&path, bytes)?,
            None => store.remove_project_file(&path)?,
        }
        let expected_new = journal.files[index].new_sha256.clone();
        if hash_file(&root.join(&path))? != expected_new {
            bail!("post_write_hash_mismatch: '{}'", path);
        }
        applied += 1;
    }
    Ok(applied)
}

fn validate_result(root: &Path, target: &TemplateManifest) -> Result<ValidationReport> {
    let current = TemplateManifest::read(root)?.context("manifest missing after apply")?;
    if current != *target {
        bail!("manifest validation failed after apply");
    }
    Ok(ValidationReport {
        status: "not_run".to_owned(),
        commands: vec![],
        notes: vec!["native/toolchain validation is not run by this transaction core".to_owned()],
    })
}

fn new_transaction_id() -> Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).context("generating transaction id")?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(format!(
        "tx-{millis}-{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{Platform, ProjectConfig, UiFramework, scaffold};
    use crate::upgrade::save_plan;

    fn config() -> ProjectConfig {
        ProjectConfig {
            name: "apply-fixture".into(),
            title: "Apply Fixture".into(),
            bundle_id: "com.example.applyfixture".into(),
            ui_framework: UiFramework::GpuiKit,
            targets: vec![Platform::MacOs],
        }
    }

    #[test]
    fn clean_plan_commits_noop_transaction_and_releases_lock() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), &config()).unwrap();
        let plan = plan_project(dir.path(), crate::template::TEMPLATE_VERSION).unwrap();
        save_plan(dir.path(), &plan).unwrap();

        let report = apply_cached_plan(dir.path(), &plan.plan_id).unwrap();
        assert_eq!(report.state, TransactionState::Committed);
        assert_eq!(report.files_applied, 0);
        assert!(!dir.path().join(".gpui/upgrade/upgrade.lock").exists());
        assert!(
            dir.path()
                .join(".gpui/upgrade/transactions")
                .join(report.transaction_id)
                .join("journal.json")
                .is_file()
        );
    }

    #[test]
    fn stale_plan_is_rejected_before_a_transaction_is_created() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), &config()).unwrap();
        let plan = plan_project(dir.path(), crate::template::TEMPLATE_VERSION).unwrap();
        save_plan(dir.path(), &plan).unwrap();
        fs::write(
            dir.path().join("crates/app/src/lib.rs"),
            b"user changed after plan\n",
        )
        .unwrap();

        let error = apply_cached_plan(dir.path(), &plan.plan_id).unwrap_err();
        assert!(error.to_string().contains("stale_plan"));
        assert!(!dir.path().join(".gpui/upgrade/upgrade.lock").exists());
        assert!(!dir.path().join(".gpui/upgrade/transactions").exists());
    }
}
