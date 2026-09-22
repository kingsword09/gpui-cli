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
}
