//! Precise, recoverable file transaction journal for upgrade apply.
//!
//! This module is intentionally independent from the CLI command. T04's later
//! apply layer will create journal entries while holding the project lock; this
//! layer owns only durable metadata, exact backups, and hash-based recovery.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;
const TRANSACTION_ROOT: &str = ".gpui/upgrade/transactions";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    Prepared,
    Writing,
    Validating,
    Committed,
    RolledBack,
    RecoveryRequired,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Planned,
    BackedUp,
    Replaced,
    Restored,
    UserModified,
    RecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalFile {
    pub path: String,
    pub old_sha256: Option<String>,
    pub new_sha256: Option<String>,
    pub backup_path: Option<String>,
    pub state: FileState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionJournal {
    pub schema_version: u32,
    pub transaction_id: String,
    pub plan_id: String,
    pub state: TransactionState,
    pub files: Vec<JournalFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryReport {
    pub transaction_id: String,
    pub state: TransactionState,
    pub restored: Vec<String>,
    pub preserved_user_changes: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TransactionStore {
    root: PathBuf,
    directory: PathBuf,
    journal_path: PathBuf,
}

impl TransactionJournal {
    pub fn new(
        transaction_id: impl Into<String>,
        plan_id: impl Into<String>,
        files: Vec<JournalFile>,
    ) -> Result<Self> {
        let journal = Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            transaction_id: transaction_id.into(),
            plan_id: plan_id.into(),
            state: TransactionState::Prepared,
            files,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION {
            bail!(
                "unsupported transaction journal schema {}; expected {}",
                self.schema_version,
                JOURNAL_SCHEMA_VERSION
            );
        }
        validate_id(&self.transaction_id)?;
        if self.plan_id.trim().is_empty() {
            bail!("transaction journal plan_id cannot be empty");
        }
        let mut paths = BTreeSet::new();
        for file in &self.files {
            validate_relative_path(&file.path)?;
            if !paths.insert(file.path.as_str()) {
                bail!(
                    "transaction journal contains duplicate path '{}'",
                    file.path
                );
            }
            if let Some(hash) = &file.old_sha256 {
                validate_hash(hash)?;
            }
            if let Some(hash) = &file.new_sha256 {
                validate_hash(hash)?;
            }
            if let Some(backup) = &file.backup_path {
                validate_relative_path(backup)?;
                if !backup.starts_with("backups/") {
                    bail!(
                        "backup path '{}' is outside the transaction backup directory",
                        backup
                    );
                }
            }
        }
        Ok(())
    }
}

impl TransactionStore {
    pub fn create(
        root: &Path,
        transaction_id: impl Into<String>,
        plan_id: impl Into<String>,
        files: Vec<JournalFile>,
    ) -> Result<(Self, TransactionJournal)> {
        let transaction_id = transaction_id.into();
        validate_id(&transaction_id)?;
        let directory = root.join(TRANSACTION_ROOT).join(&transaction_id);
        if directory.exists() {
            bail!(
                "transaction '{}' already exists at {}",
                transaction_id,
                directory.display()
            );
        }
        fs::create_dir_all(directory.join("backups"))?;
        let journal = TransactionJournal::new(transaction_id.clone(), plan_id, files)?;
        let store = Self {
            root: root.to_path_buf(),
            journal_path: directory.join("journal.json"),
            directory,
        };
        store.write(&journal)?;
        Ok((store, journal))
    }

    pub fn open(root: &Path, transaction_id: &str) -> Result<Self> {
        validate_id(transaction_id)?;
        let directory = root.join(TRANSACTION_ROOT).join(transaction_id);
        let journal_path = directory.join("journal.json");
        if !journal_path.is_file() {
            bail!(
                "transaction journal '{}' does not exist",
                journal_path.display()
            );
        }
        Ok(Self {
            root: root.to_path_buf(),
            directory,
            journal_path,
        })
    }

    pub fn read(&self) -> Result<TransactionJournal> {
        let bytes = fs::read(&self.journal_path)
            .with_context(|| format!("reading {}", self.journal_path.display()))?;
        let journal: TransactionJournal = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "invalid transaction journal {}",
                self.journal_path.display()
            )
        })?;
        journal.validate()?;
        Ok(journal)
    }

    pub fn write(&self, journal: &TransactionJournal) -> Result<()> {
        journal.validate()?;
        atomic_json(&self.journal_path, journal)
    }

    pub fn backup_path_for(path: &str) -> String {
        let digest = Sha256::digest(path.as_bytes());
        format!("backups/{digest:x}.bin")
    }

    pub fn write_backup(&self, relative_backup: &str, bytes: &[u8]) -> Result<()> {
        validate_relative_path(relative_backup)?;
        if !relative_backup.starts_with("backups/") {
            bail!(
                "backup path '{}' is outside the transaction backup directory",
                relative_backup
            );
        }
        let path = self.directory.join(relative_backup);
        if path.exists() {
            let existing = fs::read(&path)?;
            if existing != bytes {
                bail!(
                    "backup '{}' already contains different bytes",
                    relative_backup
                );
            }
            return Ok(());
        }
        atomic_bytes(&path, bytes)
    }

    pub fn read_backup(&self, relative_backup: &str) -> Result<Vec<u8>> {
        validate_relative_path(relative_backup)?;
        if !relative_backup.starts_with("backups/") {
            bail!(
                "backup path '{}' is outside the transaction backup directory",
                relative_backup
            );
        }
        fs::read(self.directory.join(relative_backup))
            .with_context(|| format!("reading backup '{}'", relative_backup))
    }

    pub fn path(&self, relative: &str) -> Result<PathBuf> {
        validate_relative_path(relative)?;
        let path = self.root.join(relative);
        ensure_no_symlink_parent(&self.root, relative)?;
        Ok(path)
    }
}

pub fn recover_transaction(root: &Path, transaction_id: &str) -> Result<RecoveryReport> {
    let store = TransactionStore::open(root, transaction_id)?;
    let mut journal = store.read()?;
    if journal.state == TransactionState::Committed || journal.state == TransactionState::RolledBack
    {
        return Ok(RecoveryReport {
            transaction_id: journal.transaction_id,
            state: journal.state,
            restored: vec![],
            preserved_user_changes: vec![],
            errors: vec![],
        });
    }

    let mut restored = Vec::new();
    let mut preserved_user_changes = Vec::new();
    let mut errors = Vec::new();

    for file in &mut journal.files {
        if !matches!(
            file.state,
            FileState::Replaced | FileState::RecoveryRequired
        ) {
            continue;
        }
        let path = match store.path(&file.path) {
            Ok(path) => path,
            Err(error) => {
                file.state = FileState::RecoveryRequired;
                errors.push(format!("{}: {error:#}", file.path));
                continue;
            }
        };
        let current = hash_file(&path)?;
        if current.as_deref() == file.old_sha256.as_deref() {
            file.state = FileState::Restored;
            restored.push(file.path.clone());
            continue;
        }
        if current.as_deref() != file.new_sha256.as_deref() {
            file.state = FileState::UserModified;
            preserved_user_changes.push(file.path.clone());
            continue;
        }

        match &file.old_sha256 {
            Some(old_hash) => {
                let Some(backup_path) = &file.backup_path else {
                    file.state = FileState::RecoveryRequired;
                    errors.push(format!("{}: missing backup path", file.path));
                    continue;
                };
                let bytes = match store.read_backup(backup_path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        file.state = FileState::RecoveryRequired;
                        errors.push(format!("{}: {error:#}", file.path));
                        continue;
                    }
                };
                if hash_bytes(&bytes) != *old_hash {
                    file.state = FileState::RecoveryRequired;
                    errors.push(format!(
                        "{}: backup hash does not match old hash",
                        file.path
                    ));
                    continue;
                }
                if let Err(error) = atomic_bytes(&path, &bytes) {
                    file.state = FileState::RecoveryRequired;
                    errors.push(format!("{}: restore failed: {error:#}", file.path));
                    continue;
                }
            }
            None => {
                if let Err(error) = remove_owned_file(&path) {
                    file.state = FileState::RecoveryRequired;
                    errors.push(format!("{}: remove failed: {error:#}", file.path));
                    continue;
                }
            }
        }
        file.state = FileState::Restored;
        restored.push(file.path.clone());
    }

    journal.state = if errors.is_empty() && preserved_user_changes.is_empty() {
        TransactionState::RolledBack
    } else {
        TransactionState::RecoveryRequired
    };
    let state = journal.state;
    store.write(&journal)?;
    Ok(RecoveryReport {
        transaction_id: journal.transaction_id,
        state,
        restored,
        preserved_user_changes,
        errors,
    })
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = {
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        bytes
    };
    atomic_bytes(path, &bytes)
}

fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path '{}' has no parent", path.display()))?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temp.path(), path)?;
    Ok(())
}

fn remove_owned_file(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(hash_bytes(&fs::read(path)?)))
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn validate_id(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid transaction id");
    }
    Ok(())
}

fn validate_hash(value: &str) -> Result<()> {
    let valid = value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if !valid {
        bail!("invalid sha256 '{value}'");
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("unsafe transaction path '{value}'");
    }
    Ok(())
}

fn ensure_no_symlink_parent(root: &Path, relative: &str) -> Result<()> {
    let mut current = root.to_path_buf();
    let path = Path::new(relative);
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        current.push(component.as_os_str());
        if components.peek().is_some() && current.is_symlink() {
            bail!("transaction path '{}' crosses a symbolic link", relative);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(value: &[u8]) -> String {
        hash_bytes(value)
    }

    fn journal_file(path: &str, old: Option<&[u8]>, new: &[u8]) -> JournalFile {
        JournalFile {
            path: path.into(),
            old_sha256: old.map(hash),
            new_sha256: Some(hash(new)),
            backup_path: old.map(|_| TransactionStore::backup_path_for(path)),
            state: FileState::Replaced,
        }
    }

    #[test]
    fn recovery_restores_exact_backup_and_marks_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("src/main.rs");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"new").unwrap();
        let file = journal_file("src/main.rs", Some(b"old"), b"new");
        let (store, mut journal) =
            TransactionStore::create(dir.path(), "tx-restore", "sha256:plan", vec![file]).unwrap();
        let backup = journal.files[0].backup_path.clone().unwrap();
        store.write_backup(&backup, b"old").unwrap();
        journal.state = TransactionState::Writing;
        store.write(&journal).unwrap();

        let report = recover_transaction(dir.path(), "tx-restore").unwrap();
        assert_eq!(report.state, TransactionState::RolledBack);
        assert_eq!(report.restored, vec!["src/main.rs"]);
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(store.read().unwrap().files[0].state, FileState::Restored);
    }

    #[test]
    fn recovery_preserves_user_edit_and_requires_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, b"user-after-failure").unwrap();
        let file = journal_file("settings.toml", Some(b"old"), b"new");
        let (store, journal) =
            TransactionStore::create(dir.path(), "tx-user-edit", "sha256:plan", vec![file])
                .unwrap();
        store.write(&journal).unwrap();

        let report = recover_transaction(dir.path(), "tx-user-edit").unwrap();
        assert_eq!(report.state, TransactionState::RecoveryRequired);
        assert_eq!(report.preserved_user_changes, vec!["settings.toml"]);
        assert_eq!(fs::read(&path).unwrap(), b"user-after-failure");
    }

    #[test]
    fn recovery_removes_owned_new_file_but_not_a_user_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        fs::write(&path, b"new").unwrap();
        let file = journal_file("new.txt", None, b"new");
        let (store, mut journal) =
            TransactionStore::create(dir.path(), "tx-new-file", "sha256:plan", vec![file]).unwrap();
        journal.state = TransactionState::Writing;
        store.write(&journal).unwrap();

        let report = recover_transaction(dir.path(), "tx-new-file").unwrap();
        assert_eq!(report.state, TransactionState::RolledBack);
        assert!(!path.exists());

        fs::write(&path, b"user-file").unwrap();
        let (store, mut journal) = TransactionStore::create(
            dir.path(),
            "tx-new-user",
            "sha256:plan",
            vec![journal_file("new.txt", None, b"new")],
        )
        .unwrap();
        journal.state = TransactionState::Writing;
        store.write(&journal).unwrap();
        let report = recover_transaction(dir.path(), "tx-new-user").unwrap();
        assert_eq!(report.state, TransactionState::RecoveryRequired);
        assert_eq!(fs::read(&path).unwrap(), b"user-file");
    }

    #[test]
    fn missing_backup_is_recovery_required_not_silent_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-backup.txt");
        fs::write(&path, b"new").unwrap();
        let mut file = journal_file("missing-backup.txt", Some(b"old"), b"new");
        file.backup_path = Some("backups/does-not-exist.bin".into());
        let (store, mut journal) =
            TransactionStore::create(dir.path(), "tx-missing-backup", "sha256:plan", vec![file])
                .unwrap();
        journal.state = TransactionState::Writing;
        store.write(&journal).unwrap();

        let report = recover_transaction(dir.path(), "tx-missing-backup").unwrap();
        assert_eq!(report.state, TransactionState::RecoveryRequired);
        assert!(!report.errors.is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn journal_rejects_path_traversal_and_duplicate_entries() {
        let mut file = journal_file("../escape", Some(b"old"), b"new");
        assert!(TransactionJournal::new("tx-path", "sha256:plan", vec![file.clone()]).is_err());
        file.path = "safe.txt".into();
        assert!(
            TransactionJournal::new("tx-duplicate", "sha256:plan", vec![file.clone(), file])
                .is_err()
        );
    }
}
