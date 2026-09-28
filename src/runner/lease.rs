//! Host-level device leases with durable ownership and fencing tokens.
//!
//! The OS file lock is the liveness primitive. The JSON owner record is
//! diagnostic and carries the fencing identity used by later device runners.
//! A heartbeat or release never accepts a different owner token.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

const MAX_IDENTIFIER_BYTES: usize = 128;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeaseOwner {
    pub device_id: String,
    pub session_id: String,
    pub pid: u32,
    pub process_start_token: String,
    pub fencing_token: String,
    pub acquired_at_ms: u64,
    pub heartbeat_at_ms: u64,
}

#[derive(Debug)]
pub enum LeaseError {
    Busy {
        lock_path: Box<PathBuf>,
        owner: Option<Box<LeaseOwner>>,
    },
    FencingLost,
    InvalidIdentifier(String),
    InvalidOwner(String),
    Io {
        context: String,
        source: io::Error,
    },
    Serialization(String),
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy { lock_path, owner } => {
                write!(formatter, "device_busy: {}", lock_path.display())?;
                if let Some(owner) = owner {
                    write!(
                        formatter,
                        " (session={}, pid={}, fencing_token={})",
                        owner.session_id, owner.pid, owner.fencing_token
                    )?;
                }
                Ok(())
            }
            Self::FencingLost => write!(formatter, "device_lease_fencing_lost"),
            Self::InvalidIdentifier(value) => {
                write!(formatter, "invalid_device_lease_identifier: {value}")
            }
            Self::InvalidOwner(message) => {
                write!(formatter, "invalid_device_lease_owner: {message}")
            }
            Self::Io { context, source } => write!(formatter, "{context}: {source}"),
            Self::Serialization(message) => {
                write!(formatter, "device_lease_serialization: {message}")
            }
        }
    }
}

impl Error for LeaseError {}

#[derive(Debug)]
pub struct DeviceLease {
    lock_path: PathBuf,
    owner_path: PathBuf,
    owner: LeaseOwner,
    file: Option<File>,
}

impl DeviceLease {
    pub fn acquire(
        project_root: &Path,
        device_id: &str,
        session_id: &str,
        pid: u32,
        process_start_token: &str,
        now_ms: u64,
    ) -> Result<Self, LeaseError> {
        validate_identifier(device_id)?;
        validate_identifier(session_id)?;
        validate_identifier(process_start_token)?;
        let root = project_root
            .canonicalize()
            .map_err(|source| io_error("resolving lease project root", source))?;
        let leases_dir = root.join(".gpui").join("leases");
        reject_symlink_components(&root, &leases_dir)?;
        fs::create_dir_all(&leases_dir)
            .map_err(|source| io_error("creating device lease directory", source))?;
        reject_symlink_components(&root, &leases_dir)?;

        let lock_path = leases_dir.join(format!("{device_id}.lock"));
        let owner_path = leases_dir.join(format!("{device_id}.owner.json"));
        reject_existing_symlink(&lock_path)?;
        reject_existing_symlink(&owner_path)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| io_error("opening device lease lock", source))?;
        if let Err(error) = file.try_lock_exclusive() {
            if is_busy_error(&error) {
                return Err(LeaseError::Busy {
                    lock_path: Box::new(lock_path),
                    owner: read_owner(&owner_path).ok().map(Box::new),
                });
            }
            return Err(io_error("locking device lease", error));
        }

        let owner = LeaseOwner {
            device_id: device_id.into(),
            session_id: session_id.into(),
            pid,
            process_start_token: process_start_token.into(),
            fencing_token: random_token()?,
            acquired_at_ms: now_ms,
            heartbeat_at_ms: now_ms,
        };
        if let Err(error) = write_owner(&owner_path, &owner) {
            let _ = file.unlock();
            return Err(error);
        }
        Ok(Self {
            lock_path,
            owner_path,
            owner,
            file: Some(file),
        })
    }

    pub fn owner(&self) -> &LeaseOwner {
        &self.owner
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub fn owner_path(&self) -> &Path {
        &self.owner_path
    }

    pub fn heartbeat(&mut self, now_ms: u64) -> Result<(), LeaseError> {
        self.assert_owner()?;
        self.owner.heartbeat_at_ms = now_ms;
        write_owner(&self.owner_path, &self.owner)
    }

    pub fn release(mut self) -> Result<(), LeaseError> {
        self.release_inner()
    }

    fn assert_owner(&self) -> Result<(), LeaseError> {
        let current = read_owner(&self.owner_path)?;
        if current.fencing_token != self.owner.fencing_token
            || current.session_id != self.owner.session_id
            || current.device_id != self.owner.device_id
        {
            return Err(LeaseError::FencingLost);
        }
        Ok(())
    }

    fn release_inner(&mut self) -> Result<(), LeaseError> {
        let owner_result = self.assert_owner();
        if owner_result.is_ok() {
            reject_existing_symlink(&self.owner_path)?;
            match fs::remove_file(&self.owner_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("removing device lease owner", error)),
            }
        }
        if let Some(file) = self.file.take() {
            file.unlock()
                .map_err(|error| io_error("unlocking device lease", error))?;
        }
        owner_result
    }
}

impl Drop for DeviceLease {
    fn drop(&mut self) {
        if self.file.is_some() && self.assert_owner().is_ok() {
            let _ = fs::remove_file(&self.owner_path);
        }
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}

fn write_owner(path: &Path, owner: &LeaseOwner) -> Result<(), LeaseError> {
    reject_existing_symlink(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| LeaseError::InvalidOwner("owner path has no parent".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| io_error("creating device lease owner temp file", source))?;
    let mut bytes = serde_json::to_vec_pretty(owner)
        .map_err(|error| LeaseError::Serialization(error.to_string()))?;
    bytes.push(b'\n');
    temporary
        .write_all(&bytes)
        .map_err(|source| io_error("writing device lease owner", source))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| io_error("syncing device lease owner", source))?;
    if fs::symlink_metadata(path).is_ok() {
        fs::remove_file(path).map_err(|source| io_error("replacing device lease owner", source))?;
    }
    temporary
        .persist(path)
        .map_err(|error| io_error("publishing device lease owner", error.error))?;
    Ok(())
}

fn read_owner(path: &Path) -> Result<LeaseOwner, LeaseError> {
    reject_existing_symlink(path)?;
    let bytes = fs::read(path).map_err(|source| io_error("reading device lease owner", source))?;
    serde_json::from_slice(&bytes).map_err(|error| LeaseError::InvalidOwner(error.to_string()))
}

fn random_token() -> Result<String, LeaseError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|source| {
        LeaseError::Serialization(format!("generating fencing token: {source}"))
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_identifier(value: &str) -> Result<(), LeaseError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(LeaseError::InvalidIdentifier(value.into()));
    }
    Ok(())
}

fn reject_existing_symlink(path: &Path) -> Result<(), LeaseError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(LeaseError::InvalidOwner(
            format!("symbolic link: {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("checking device lease path", source)),
    }
}

fn reject_symlink_components(root: &Path, path: &Path) -> Result<(), LeaseError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| LeaseError::InvalidOwner("lease path escapes project root".into()))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(LeaseError::InvalidOwner(
                "unsafe lease path component".into(),
            ));
        }
        current.push(component.as_os_str());
        reject_existing_symlink(&current)?;
    }
    Ok(())
}

fn is_busy_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied
    ) || matches!(error.raw_os_error(), Some(32 | 33))
}

fn io_error(context: &str, source: io::Error) -> LeaseError {
    LeaseError::Io {
        context: context.into(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lease_is_exclusive_and_releases_for_the_next_owner() {
        let root = tempdir().unwrap();
        let first =
            DeviceLease::acquire(root.path(), "sim-1", "session-a", 10, "start-a", 100).unwrap();
        let busy = DeviceLease::acquire(root.path(), "sim-1", "session-b", 11, "start-b", 101)
            .unwrap_err();
        match busy {
            LeaseError::Busy { owner, .. } => {
                assert_eq!(owner.unwrap().session_id, "session-a");
            }
            other => panic!("expected busy, got {other}"),
        }
        first.release().unwrap();
        let second =
            DeviceLease::acquire(root.path(), "sim-1", "session-b", 11, "start-b", 102).unwrap();
        assert_eq!(second.owner().fencing_token.len(), 32);
    }

    #[test]
    fn heartbeat_and_release_reject_a_replaced_fencing_owner() {
        let root = tempdir().unwrap();
        let mut lease =
            DeviceLease::acquire(root.path(), "sim-1", "session-a", 10, "start-a", 100).unwrap();
        lease.heartbeat(200).unwrap();
        let mut replaced = lease.owner().clone();
        replaced.fencing_token = "other".into();
        fs::write(
            lease.owner_path(),
            serde_json::to_vec_pretty(&replaced).unwrap(),
        )
        .unwrap();
        assert!(matches!(lease.heartbeat(300), Err(LeaseError::FencingLost)));
        assert!(matches!(lease.release(), Err(LeaseError::FencingLost)));
        let next =
            DeviceLease::acquire(root.path(), "sim-1", "session-b", 11, "start-b", 400).unwrap();
        next.release().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lease_paths_are_rejected() {
        let root = tempdir().unwrap();
        let leases = root.path().join(".gpui/leases");
        fs::create_dir_all(&leases).unwrap();
        std::os::unix::fs::symlink(root.path(), leases.join("sim-1.lock")).unwrap();
        assert!(matches!(
            DeviceLease::acquire(root.path(), "sim-1", "session-a", 10, "start-a", 100),
            Err(LeaseError::InvalidOwner(_))
        ));
    }
}
