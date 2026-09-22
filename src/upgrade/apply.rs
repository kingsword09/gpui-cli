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
    recover_transaction, safe_project_path,
};
use super::{FileAction, PlanStatus, hash_file, load_plan, load_project, plan_project};
use crate::commands::doctor::diagnose_project;
use crate::template::{Platform, scaffold_version};
use crate::template_manifest::{MANIFEST_RELATIVE_PATH, TemplateManifest};
use crate::toolchain::Target as ToolchainTarget;
use crate::toolchain::report::CheckStatus;

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
    apply_cached_plan_with_failure(root, plan_id, None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePoint {
    AfterJournal,
    AfterBackup,
    BeforeReplace,
    AfterReplace,
    BeforeManifest,
    AfterManifest,
    BeforeValidate,
    AfterValidate,
}

impl FailurePoint {
    fn crash_env_value(self) -> &'static str {
        match self {
            Self::AfterJournal => "after_journal",
            Self::AfterBackup => "after_backup",
            Self::BeforeReplace => "before_replace",
            Self::AfterReplace => "after_replace",
            Self::BeforeManifest => "before_manifest",
            Self::AfterManifest => "after_manifest",
            Self::BeforeValidate => "before_validate",
            Self::AfterValidate => "after_validate",
        }
    }
}

pub(crate) fn apply_cached_plan_with_failure(
    root: &Path,
    plan_id: &str,
    failure: Option<FailurePoint>,
) -> Result<ApplyReport> {
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
    scaffold_version(
        target_root.path(),
        &project.config,
        &stored.target.template_version,
    )?;
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
    let current_manifest_path = safe_project_path(root, MANIFEST_RELATIVE_PATH)?;
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
    maybe_crash(FailurePoint::AfterJournal);
    if let Err(error) = maybe_fail(failure, FailurePoint::AfterJournal) {
        return abort_transaction(root, &store, &mut journal, error);
    }
    if let Err(error) = backup_files(root, &store, &mut journal) {
        return abort_transaction(root, &store, &mut journal, error);
    }
    journal.state = TransactionState::Writing;
    store.write(&journal)?;
    maybe_crash(FailurePoint::AfterBackup);
    if let Err(error) = maybe_fail(failure, FailurePoint::AfterBackup) {
        return abort_transaction(root, &store, &mut journal, error);
    }

    let applied = match write_files(&store, &mut journal, &target_bytes, failure) {
        Ok(applied) => applied,
        Err(error) => return abort_transaction(root, &store, &mut journal, error),
    };

    journal.state = TransactionState::Validating;
    store.write(&journal)?;
    maybe_crash(FailurePoint::BeforeValidate);
    if let Err(error) = maybe_fail(failure, FailurePoint::BeforeValidate) {
        return abort_transaction(root, &store, &mut journal, error);
    }
    let validation = match validate_result(root, &target_manifest, &project.config.targets) {
        Ok(validation) => validation,
        Err(error) => return abort_transaction(root, &store, &mut journal, error),
    };
    if let Err(error) = maybe_fail(failure, FailurePoint::AfterValidate) {
        return abort_transaction(root, &store, &mut journal, error);
    }
    maybe_crash(FailurePoint::AfterValidate);

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
    store: &TransactionStore,
    journal: &mut TransactionJournal,
    target_bytes: &BTreeMap<String, Option<Vec<u8>>>,
    failure: Option<FailurePoint>,
) -> Result<usize> {
    let mut applied = 0;
    for index in 0..journal.files.len() {
        let path = journal.files[index].path.clone();
        let expected_old = journal.files[index].old_sha256.clone();
        let project_path = store.path(&path)?;
        if hash_file(&project_path)? != expected_old {
            bail!("concurrent_edit: '{}' changed before replacement", path);
        }
        if path == MANIFEST_RELATIVE_PATH {
            maybe_crash(FailurePoint::BeforeManifest);
            maybe_fail(failure, FailurePoint::BeforeManifest)?;
        }
        maybe_crash(FailurePoint::BeforeReplace);
        maybe_fail(failure, FailurePoint::BeforeReplace)?;
        journal.files[index].state = FileState::Replaced;
        store.write(journal)?;
        match target_bytes
            .get(&path)
            .context("journal file has no prepared target bytes")?
        {
            Some(bytes) => store.write_project_file(&path, bytes)?,
            None => store.remove_project_file(&path)?,
        }
        maybe_crash(FailurePoint::AfterReplace);
        maybe_fail(failure, FailurePoint::AfterReplace)?;
        if path == MANIFEST_RELATIVE_PATH {
            maybe_crash(FailurePoint::AfterManifest);
            maybe_fail(failure, FailurePoint::AfterManifest)?;
        }
        let expected_new = journal.files[index].new_sha256.clone();
        if hash_file(&project_path)? != expected_new {
            bail!("post_write_hash_mismatch: '{}'", path);
        }
        applied += 1;
    }
    Ok(applied)
}

fn maybe_fail(failure: Option<FailurePoint>, point: FailurePoint) -> Result<()> {
    if failure == Some(point) {
        bail!("injected_failure:{point:?}");
    }
    Ok(())
}

fn maybe_crash(point: FailurePoint) {
    if std::env::var("GPUI_UPGRADE_CRASH_POINT").ok().as_deref() == Some(point.crash_env_value()) {
        eprintln!(
            "injected_crash:{}; transaction remains for explicit recovery",
            point.crash_env_value()
        );
        std::process::exit(75);
    }
}

fn abort_transaction(
    root: &Path,
    store: &TransactionStore,
    journal: &mut TransactionJournal,
    error: anyhow::Error,
) -> Result<ApplyReport> {
    journal.state = TransactionState::RecoveryRequired;
    let journal_error = store.write(journal).err();
    let recovery = recover_transaction(root, &journal.transaction_id)?;
    bail!(
        "upgrade apply failed: {error:#}; recovery_state={:?}, \
         preserved_user_changes={:?}, recovery_errors={:?}, journal_error={:?}",
        recovery.state,
        recovery.preserved_user_changes,
        recovery.errors,
        journal_error.map(|error| error.to_string())
    );
}

fn validate_result(
    root: &Path,
    target: &TemplateManifest,
    platforms: &[Platform],
) -> Result<ValidationReport> {
    let current = TemplateManifest::read(root)?.context("manifest missing after apply")?;
    if current != *target {
        bail!("manifest validation failed after apply");
    }

    let mut commands = Vec::new();
    let mut notes = Vec::new();
    let mut validation_not_run = false;
    for target in validation_targets(platforms) {
        let report = diagnose_project(root, target)?;
        for check in &report.checks {
            if let Some(command) = &check.command {
                commands.push(command.argv());
            }
        }
        let failed = report
            .checks
            .iter()
            .filter(|check| check.required)
            .any(|check| check.status == CheckStatus::Fail);
        if failed {
            let failed_checks = report
                .checks
                .iter()
                .filter(|check| check.required && check.status == CheckStatus::Fail)
                .map(|check| check.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "toolchain validation failed for {}: {}",
                target.label(),
                failed_checks
            );
        }
        if !report.required_ok() {
            validation_not_run = true;
            notes.push(format!(
                "{} toolchain validation not_run: required tool unavailable or unknown",
                target.label()
            ));
        } else {
            notes.push(format!("{} toolchain checks passed", target.label()));
        }
    }
    Ok(ValidationReport {
        status: if validation_not_run {
            "not_run".to_owned()
        } else {
            "passed".to_owned()
        },
        commands,
        notes,
    })
}

fn validation_targets(platforms: &[Platform]) -> Vec<ToolchainTarget> {
    let mut targets = Vec::new();
    for platform in platforms {
        let target = if platform.is_desktop() {
            ToolchainTarget::Desktop
        } else if *platform == Platform::IOs {
            ToolchainTarget::Ios
        } else {
            ToolchainTarget::Android
        };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
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
        assert!(matches!(
            report.validation.status.as_str(),
            "passed" | "not_run"
        ));
        assert!(!report.validation.commands.is_empty());
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

    #[test]
    fn injected_failure_after_journal_recovers_and_releases_lock() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), &config()).unwrap();
        let plan = plan_project(dir.path(), crate::template::TEMPLATE_VERSION).unwrap();
        save_plan(dir.path(), &plan).unwrap();

        let error = apply_cached_plan_with_failure(
            dir.path(),
            &plan.plan_id,
            Some(FailurePoint::AfterJournal),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected_failure"));
        assert!(!dir.path().join(".gpui/upgrade/upgrade.lock").exists());
        let transactions = fs::read_dir(dir.path().join(".gpui/upgrade/transactions"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(transactions.len(), 1);
        let journal = fs::read_to_string(transactions[0].path().join("journal.json")).unwrap();
        assert!(journal.contains("\"state\": \"rolled_back\""));
    }

    #[test]
    fn injected_failure_before_validation_recovers_written_state() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), &config()).unwrap();
        let plan = plan_project(dir.path(), crate::template::TEMPLATE_VERSION).unwrap();
        save_plan(dir.path(), &plan).unwrap();

        let error = apply_cached_plan_with_failure(
            dir.path(),
            &plan.plan_id,
            Some(FailurePoint::BeforeValidate),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected_failure"));
        assert!(!dir.path().join(".gpui/upgrade/upgrade.lock").exists());
    }

    fn journal_file(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> JournalFile {
        JournalFile {
            path: path.to_owned(),
            old_sha256: old.map(hash_bytes),
            new_sha256: new.map(hash_bytes),
            backup_path: old.map(|_| TransactionStore::backup_path_for(path)),
            state: FileState::BackedUp,
        }
    }

    #[test]
    fn prepared_writer_applies_replace_add_and_delete_entries() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("replace.txt"), b"old replace").unwrap();
        fs::write(dir.path().join("delete.txt"), b"old delete").unwrap();
        let files = vec![
            journal_file("replace.txt", Some(b"old replace"), Some(b"new replace")),
            journal_file("add.txt", None, Some(b"new add")),
            journal_file("delete.txt", Some(b"old delete"), None),
        ];
        let (store, mut journal) =
            TransactionStore::create(dir.path(), "tx-write-matrix", "sha256:plan", files).unwrap();
        store
            .write_backup(
                &journal.files[0].backup_path.clone().unwrap(),
                b"old replace",
            )
            .unwrap();
        store
            .write_backup(
                &journal.files[2].backup_path.clone().unwrap(),
                b"old delete",
            )
            .unwrap();
        journal.state = TransactionState::Writing;
        store.write(&journal).unwrap();

        let targets = BTreeMap::from([
            ("replace.txt".to_owned(), Some(b"new replace".to_vec())),
            ("add.txt".to_owned(), Some(b"new add".to_vec())),
            ("delete.txt".to_owned(), None),
        ]);
        let applied = write_files(&store, &mut journal, &targets, None).unwrap();

        assert_eq!(applied, 3);
        assert_eq!(
            fs::read(dir.path().join("replace.txt")).unwrap(),
            b"new replace"
        );
        assert_eq!(fs::read(dir.path().join("add.txt")).unwrap(), b"new add");
        assert!(!dir.path().join("delete.txt").exists());
        assert!(
            journal
                .files
                .iter()
                .all(|file| file.state == FileState::Replaced)
        );
    }

    #[test]
    fn manifest_failure_before_and_after_write_recovers_exactly() {
        for failure in [FailurePoint::BeforeManifest, FailurePoint::AfterManifest] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(MANIFEST_RELATIVE_PATH);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"old manifest").unwrap();
            let file = journal_file(
                MANIFEST_RELATIVE_PATH,
                Some(b"old manifest"),
                Some(b"new manifest"),
            );
            let (store, mut journal) = TransactionStore::create(
                dir.path(),
                format!("tx-manifest-{failure:?}"),
                "sha256:plan",
                vec![file],
            )
            .unwrap();
            store
                .write_backup(
                    &journal.files[0].backup_path.clone().unwrap(),
                    b"old manifest",
                )
                .unwrap();
            journal.state = TransactionState::Writing;
            store.write(&journal).unwrap();

            let targets = BTreeMap::from([(
                MANIFEST_RELATIVE_PATH.to_owned(),
                Some(b"new manifest".to_vec()),
            )]);
            let error = write_files(&store, &mut journal, &targets, Some(failure)).unwrap_err();
            let recovery_error = abort_transaction(dir.path(), &store, &mut journal, error)
                .unwrap_err()
                .to_string();

            assert!(recovery_error.contains("injected_failure"));
            assert_eq!(fs::read(&path).unwrap(), b"old manifest");
            assert_eq!(store.read().unwrap().state, TransactionState::RolledBack);
        }
    }

    #[test]
    fn user_edit_after_partial_write_is_preserved_by_recovery() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("first.txt"), b"old first").unwrap();
        fs::write(dir.path().join("second.txt"), b"old second").unwrap();
        let files = vec![
            journal_file("first.txt", Some(b"old first"), Some(b"new first")),
            journal_file("second.txt", Some(b"old second"), Some(b"new second")),
        ];
        let (store, mut journal) =
            TransactionStore::create(dir.path(), "tx-concurrent-edit", "sha256:plan", files)
                .unwrap();
        store
            .write_backup(&journal.files[0].backup_path.clone().unwrap(), b"old first")
            .unwrap();
        store
            .write_backup(
                &journal.files[1].backup_path.clone().unwrap(),
                b"old second",
            )
            .unwrap();
        journal.state = TransactionState::Writing;
        store.write(&journal).unwrap();

        let targets = BTreeMap::from([
            ("first.txt".to_owned(), Some(b"new first".to_vec())),
            ("second.txt".to_owned(), Some(b"new second".to_vec())),
        ]);
        let error = write_files(
            &store,
            &mut journal,
            &targets,
            Some(FailurePoint::AfterReplace),
        )
        .unwrap_err();
        fs::write(dir.path().join("first.txt"), b"user edit after write").unwrap();

        let recovery_error = abort_transaction(dir.path(), &store, &mut journal, error)
            .unwrap_err()
            .to_string();
        assert!(recovery_error.contains("preserved_user_changes"));
        assert!(recovery_error.contains("first.txt"));
        assert_eq!(
            fs::read(dir.path().join("first.txt")).unwrap(),
            b"user edit after write"
        );
        assert_eq!(
            fs::read(dir.path().join("second.txt")).unwrap(),
            b"old second"
        );
        assert_eq!(
            store.read().unwrap().state,
            TransactionState::RecoveryRequired
        );
    }

    #[test]
    fn apply_refuses_a_second_transaction_while_the_project_is_locked() {
        let dir = tempfile::tempdir().unwrap();
        scaffold(dir.path(), &config()).unwrap();
        let plan = plan_project(dir.path(), crate::template::TEMPLATE_VERSION).unwrap();
        save_plan(dir.path(), &plan).unwrap();
        let lock = UpgradeLock::acquire(dir.path(), "tx-existing").unwrap();

        let error = apply_cached_plan(dir.path(), &plan.plan_id).unwrap_err();
        assert!(error.to_string().contains("upgrade_busy"));
        assert!(!dir.path().join(".gpui/upgrade/transactions").exists());
        drop(lock);
    }

    #[test]
    fn validation_targets_deduplicate_desktop_and_preserve_mobile_targets() {
        assert_eq!(
            validation_targets(&[
                Platform::MacOs,
                Platform::Windows,
                Platform::IOs,
                Platform::Android,
            ]),
            vec![
                ToolchainTarget::Desktop,
                ToolchainTarget::Ios,
                ToolchainTarget::Android
            ]
        );
    }

    #[test]
    fn conflicting_backup_aborts_without_modifying_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.txt");
        fs::write(&path, b"old bytes").unwrap();
        let file = journal_file("managed.txt", Some(b"old bytes"), Some(b"new bytes"));
        let (store, mut journal) =
            TransactionStore::create(dir.path(), "tx-backup-conflict", "sha256:plan", vec![file])
                .unwrap();
        let backup_path = journal.files[0].backup_path.clone().unwrap();
        store
            .write_backup(&backup_path, b"different bytes")
            .unwrap();
        let lock = UpgradeLock::acquire(dir.path(), &journal.transaction_id).unwrap();

        let error = backup_files(dir.path(), &store, &mut journal).unwrap_err();
        let recovery_error = abort_transaction(dir.path(), &store, &mut journal, error)
            .unwrap_err()
            .to_string();
        drop(lock);

        assert!(recovery_error.contains("already contains different bytes"));
        assert_eq!(fs::read(&path).unwrap(), b"old bytes");
        assert_eq!(store.read().unwrap().state, TransactionState::RolledBack);
        assert!(!dir.path().join(".gpui/upgrade/upgrade.lock").exists());
    }
}
