use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct UpgradeLock {
    path: PathBuf,
    file: Option<File>,
}

impl UpgradeLock {
    pub fn acquire(root: &Path, transaction_id: &str) -> Result<Self> {
        let directory = root.join(".gpui/upgrade");
        fs::create_dir_all(&directory)?;
        let path = directory.join("upgrade.lock");
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!("upgrade_busy: {}", path.display())
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating upgrade lock {}", path.display()));
            }
        };
        writeln!(file, "transaction_id={transaction_id}")?;
        writeln!(file, "pid={}", std::process::id())?;
        file.sync_all()?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove a lock left by a transaction that is being explicitly recovered.
    ///
    /// Recovery is the one command that must be able to clean up after a
    /// process died while holding the lock.  It only removes the lock when
    /// its durable owner record names the requested transaction; a lock owned
    /// by another transaction is never silently removed.
    pub fn release_owned(root: &Path, transaction_id: &str) -> Result<()> {
        let path = root.join(".gpui/upgrade/upgrade.lock");
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading upgrade lock {}", path.display()));
            }
        };
        let owner = contents
            .lines()
            .find_map(|line| line.strip_prefix("transaction_id="));
        if owner != Some(transaction_id) {
            bail!(
                "upgrade_lock_owner_mismatch: {} is owned by {:?}",
                path.display(),
                owner
            );
        }
        fs::remove_file(&path)
            .with_context(|| format!("removing recovered upgrade lock {}", path.display()))
    }

    pub fn release(mut self) -> Result<()> {
        self.file.take();
        fs::remove_file(&self.path)
            .with_context(|| format!("removing upgrade lock {}", self.path.display()))
    }
}

impl Drop for UpgradeLock {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_exclusive_and_released() {
        let dir = tempfile::tempdir().unwrap();
        let first = UpgradeLock::acquire(dir.path(), "tx-a").unwrap();
        assert!(UpgradeLock::acquire(dir.path(), "tx-b").is_err());
        first.release().unwrap();
        let second = UpgradeLock::acquire(dir.path(), "tx-c").unwrap();
        assert!(second.path().exists());
    }

    #[test]
    fn recovery_only_releases_the_named_transaction_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join(".gpui/upgrade/upgrade.lock");
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        fs::write(&lock_path, "transaction_id=tx-a\npid=0\n").unwrap();

        assert!(UpgradeLock::release_owned(dir.path(), "tx-b").is_err());
        assert!(lock_path.exists());
        UpgradeLock::release_owned(dir.path(), "tx-a").unwrap();
        assert!(!lock_path.exists());
    }
}
