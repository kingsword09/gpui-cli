//! Host-level device leases with durable ownership and fencing tokens.
//!
//! The OS file lock is the liveness primitive. The JSON owner record is
//! diagnostic and carries the fencing identity used by later device runners.
//! A heartbeat or release never accepts a different owner token.

use anyhow::Context;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_IDENTIFIER_BYTES: usize = 128;
const HOST_LEASE_DIR_ENV: &str = "GPUI_DEVICE_LEASE_DIR";

/// Lease metadata is refreshed often enough that a 30 second stale-owner
/// observation is diagnostic only. It never authorizes taking the OS lock.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
pub const SUSPECT_AFTER: Duration = Duration::from_secs(30);

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

/// A child process may use the lease held by its supervisor without opening a
/// second OS lock. The supervisor remains responsible for the heartbeat and
/// release; the child only rechecks this owner record before and after each
/// device operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceLeaseDelegation {
    pub owner_path: PathBuf,
    pub device_id: String,
    pub session_id: String,
    pub fencing_token_sha256: String,
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
    HeartbeatFailed(String),
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
            Self::HeartbeatFailed(message) => {
                write!(formatter, "device_lease_heartbeat_failed: {message}")
            }
            Self::Serialization(message) => {
                write!(formatter, "device_lease_serialization: {message}")
            }
        }
    }
}

impl Error for LeaseError {}

/// Returns the host-shared lease directory.
///
/// The environment override is intentionally explicit so tests and isolated
/// supervisors can use a private directory. Normal runners use a per-user
/// host directory, which makes two different project roots contend for the
/// same physical simulator/emulator.
pub fn host_lease_directory(project_root: &Path) -> Result<PathBuf, LeaseError> {
    if let Some(path) = std::env::var_os(HOST_LEASE_DIR_ENV) {
        let path = PathBuf::from(path);
        return if path.is_absolute() {
            Ok(path)
        } else {
            Ok(project_root.join(path))
        };
    }

    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home_directory().map(|home| home.join("Library/Caches"))
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .or_else(|| std::env::var_os("XDG_STATE_HOME"))
            .map(PathBuf::from)
            .or_else(|| home_directory().map(|home| home.join(".cache")))
    };

    Ok(base
        .unwrap_or_else(|| project_root.join(".gpui"))
        .join("gpui/leases"))
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

#[derive(Debug)]
pub struct DeviceLease {
    lock_path: PathBuf,
    owner_path: PathBuf,
    owner: LeaseOwner,
    file: Option<File>,
}

impl DeviceLease {
    /// Acquires a host-shared lease for a stable device identifier.
    pub fn acquire(
        project_root: &Path,
        device_id: &str,
        session_id: &str,
        pid: u32,
        process_start_token: &str,
        now_ms: u64,
    ) -> Result<Self, LeaseError> {
        validate_device_identifier(device_id)?;
        validate_identifier(session_id)?;
        validate_identifier(process_start_token)?;
        let root = project_root
            .canonicalize()
            .map_err(|source| io_error("resolving lease project root", source))?;
        let leases_dir = host_lease_directory(&root)?;
        reject_symlink_path(&leases_dir)?;
        fs::create_dir_all(&leases_dir)
            .map_err(|source| io_error("creating device lease directory", source))?;
        reject_symlink_path(&leases_dir)?;

        Self::acquire_in_directory(
            &leases_dir,
            device_id,
            session_id,
            pid,
            process_start_token,
            now_ms,
        )
    }

    /// Creates an owner using this process's generated session/start identity.
    pub fn acquire_for_process(project_root: &Path, device_id: &str) -> Result<Self, LeaseError> {
        let pid = std::process::id();
        let process_start_token = current_process_start_token()?;
        let session_id = format!("gpui-{pid}-{}", random_token()?);
        Self::acquire(
            project_root,
            device_id,
            &session_id,
            pid,
            &process_start_token,
            epoch_ms(),
        )
    }

    fn acquire_in_directory(
        leases_dir: &Path,
        device_id: &str,
        session_id: &str,
        pid: u32,
        process_start_token: &str,
        now_ms: u64,
    ) -> Result<Self, LeaseError> {
        validate_device_identifier(device_id)?;
        validate_identifier(session_id)?;
        validate_identifier(process_start_token)?;
        reject_symlink_path(leases_dir)?;
        fs::create_dir_all(leases_dir)
            .map_err(|source| io_error("creating device lease directory", source))?;
        reject_symlink_path(leases_dir)?;

        let path_id = path_identifier(device_id);
        let lock_path = leases_dir.join(format!("{path_id}.lock"));
        let owner_path = leases_dir.join(format!("{path_id}.owner.json"));
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

    pub fn assert_owned(&self) -> Result<(), LeaseError> {
        self.assert_owner()
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

/// A lease held for the lifetime of a mobile runner session.
///
/// The background heartbeat means a long install, launch, asset transfer or
/// capture cannot silently look abandoned. Every device operation still calls
/// [`Self::execute`], which fences both before and after the external command.
#[derive(Debug)]
pub struct DeviceLeaseSession {
    shared: Arc<Mutex<Option<DeviceLease>>>,
    heartbeat_error: Arc<Mutex<Option<String>>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    owner: LeaseOwner,
    owner_path: PathBuf,
    delegated: bool,
    delegated_fencing_token_sha256: Option<String>,
    heartbeat_thread: Option<JoinHandle<()>>,
}

impl DeviceLeaseSession {
    pub fn acquire(project_root: &Path, device_id: &str) -> Result<Self, LeaseError> {
        let lease = DeviceLease::acquire_for_process(project_root, device_id)?;
        Self::start(lease)
    }

    fn start(lease: DeviceLease) -> Result<Self, LeaseError> {
        let owner = lease.owner().clone();
        let owner_path = lease.owner_path().to_owned();
        let shared = Arc::new(Mutex::new(Some(lease)));
        let heartbeat_error = Arc::new(Mutex::new(None));
        let stop = Arc::new((Mutex::new(false), Condvar::new()));

        let thread_shared = Arc::clone(&shared);
        let thread_error = Arc::clone(&heartbeat_error);
        let thread_stop = Arc::clone(&stop);
        let heartbeat_thread = thread::Builder::new()
            .name(format!("gpui-lease-{}", owner.device_id))
            .spawn(move || heartbeat_loop(thread_shared, thread_error, thread_stop))
            .map_err(|source| io_error("starting device lease heartbeat", source))?;

        Ok(Self {
            shared,
            heartbeat_error,
            stop,
            owner,
            owner_path,
            delegated: false,
            delegated_fencing_token_sha256: None,
            heartbeat_thread: Some(heartbeat_thread),
        })
    }

    /// Creates a non-owning child view of a supervisor-held lease.
    pub fn from_delegation(delegation: DeviceLeaseDelegation) -> Result<Self, LeaseError> {
        validate_device_identifier(&delegation.device_id)?;
        validate_identifier(&delegation.session_id)?;
        validate_sha256(&delegation.fencing_token_sha256)?;
        if !delegation.owner_path.is_absolute() {
            return Err(LeaseError::InvalidOwner(
                "delegated owner path must be absolute".into(),
            ));
        }
        reject_symlink_path(&delegation.owner_path)?;
        let current = read_owner(&delegation.owner_path)?;
        if current.device_id != delegation.device_id
            || current.session_id != delegation.session_id
            || token_sha256(&current.fencing_token) != delegation.fencing_token_sha256
        {
            return Err(LeaseError::FencingLost);
        }
        Ok(Self {
            shared: Arc::new(Mutex::new(None)),
            heartbeat_error: Arc::new(Mutex::new(None)),
            stop: Arc::new((Mutex::new(false), Condvar::new())),
            owner: LeaseOwner {
                device_id: delegation.device_id,
                session_id: delegation.session_id,
                pid: 0,
                process_start_token: "delegated".into(),
                fencing_token: String::new(),
                acquired_at_ms: 0,
                heartbeat_at_ms: 0,
            },
            owner_path: delegation.owner_path,
            delegated: true,
            delegated_fencing_token_sha256: Some(delegation.fencing_token_sha256),
            heartbeat_thread: None,
        })
    }

    pub fn owner(&self) -> &LeaseOwner {
        &self.owner
    }

    pub fn fencing_token(&self) -> &str {
        &self.owner.fencing_token
    }

    pub fn delegation(&self) -> DeviceLeaseDelegation {
        DeviceLeaseDelegation {
            owner_path: self.owner_path.clone(),
            device_id: self.owner.device_id.clone(),
            session_id: self.owner.session_id.clone(),
            fencing_token_sha256: self
                .delegated_fencing_token_sha256
                .clone()
                .unwrap_or_else(|| token_sha256(&self.owner.fencing_token)),
        }
    }

    pub fn assert_owned(&self) -> Result<(), LeaseError> {
        if let Some(message) = self
            .heartbeat_error
            .lock()
            .map_err(|_| LeaseError::HeartbeatFailed("heartbeat state was poisoned".into()))?
            .as_ref()
        {
            return Err(LeaseError::HeartbeatFailed(message.clone()));
        }
        if self.delegated {
            let current = read_owner(&self.owner_path)?;
            if current.session_id != self.owner.session_id
                || current.device_id != self.owner.device_id
                || self.delegated_fencing_token_sha256.as_deref()
                    != Some(token_sha256(&current.fencing_token).as_str())
            {
                return Err(LeaseError::FencingLost);
            }
            return Ok(());
        }
        let lease = self
            .shared
            .lock()
            .map_err(|_| LeaseError::HeartbeatFailed("lease state was poisoned".into()))?;
        lease
            .as_ref()
            .ok_or(LeaseError::FencingLost)?
            .assert_owned()
    }

    /// Performs an immediate metadata heartbeat and fencing check.
    pub fn heartbeat_now(&self) -> Result<(), LeaseError> {
        if self.delegated {
            return self.assert_owned();
        }
        let mut lease = self
            .shared
            .lock()
            .map_err(|_| LeaseError::HeartbeatFailed("lease state was poisoned".into()))?;
        lease
            .as_mut()
            .ok_or(LeaseError::FencingLost)?
            .heartbeat(epoch_ms())
    }

    /// Runs one external device workload under fencing checks.
    pub fn execute<T>(
        &self,
        stage: &str,
        operation: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.assert_owned()
            .map_err(anyhow::Error::new)
            .with_context(|| format!("device lease lost before {stage}"))?;
        let result = operation().with_context(|| format!("{stage} failed"));
        self.assert_owned()
            .map_err(anyhow::Error::new)
            .with_context(|| format!("device lease lost after {stage}"))?;
        result
    }

    pub fn release(mut self) -> Result<(), LeaseError> {
        if self.delegated {
            return Ok(());
        }
        self.request_stop();
        if let Some(thread) = self.heartbeat_thread.take() {
            thread
                .join()
                .map_err(|_| LeaseError::HeartbeatFailed("heartbeat thread panicked".into()))?;
        }
        let lease = self
            .shared
            .lock()
            .map_err(|_| LeaseError::HeartbeatFailed("lease state was poisoned".into()))?
            .take()
            .ok_or(LeaseError::FencingLost)?;
        lease.release()
    }

    fn request_stop(&self) {
        let (lock, condition) = &*self.stop;
        if let Ok(mut stop) = lock.lock() {
            *stop = true;
            condition.notify_all();
        }
    }
}

impl Drop for DeviceLeaseSession {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.heartbeat_thread.take() {
            let _ = thread.join();
        }
        // Dropping the remaining DeviceLease releases the OS lock and only
        // removes owner metadata if our fencing token still matches.
    }
}

fn heartbeat_loop(
    shared: Arc<Mutex<Option<DeviceLease>>>,
    heartbeat_error: Arc<Mutex<Option<String>>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
) {
    loop {
        let (lock, condition) = &*stop;
        let guard = match lock.lock() {
            Ok(guard) => guard,
            Err(_) => {
                record_heartbeat_error(&heartbeat_error, "heartbeat stop state was poisoned");
                return;
            }
        };
        let (guard, _) = match condition.wait_timeout(guard, HEARTBEAT_INTERVAL) {
            Ok(result) => result,
            Err(_) => {
                record_heartbeat_error(&heartbeat_error, "heartbeat stop state was poisoned");
                return;
            }
        };
        if *guard {
            return;
        }
        drop(guard);

        let result = match shared.lock() {
            Ok(mut lease) => lease
                .as_mut()
                .ok_or(LeaseError::FencingLost)
                .and_then(|lease| lease.heartbeat(epoch_ms())),
            Err(_) => Err(LeaseError::HeartbeatFailed(
                "lease state was poisoned".into(),
            )),
        };
        if let Err(error) = result {
            record_heartbeat_error(&heartbeat_error, &error.to_string());
            return;
        }
    }
}

fn record_heartbeat_error(target: &Mutex<Option<String>>, message: &str) {
    if let Ok(mut error) = target.lock() {
        *error = Some(message.to_string());
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

fn current_process_start_token() -> Result<String, LeaseError> {
    // The fencing token is the authorization primitive. This separate opaque
    // token prevents diagnostics from treating a reused PID as the same
    // process, including on hosts where querying native process start times is
    // not portable.
    Ok(format!("pid-{}-{}", std::process::id(), random_token()?))
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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

fn validate_sha256(value: &str) -> Result<(), LeaseError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LeaseError::InvalidIdentifier(value.into()));
    }
    Ok(())
}

fn token_sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn validate_device_identifier(value: &str) -> Result<(), LeaseError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\'))
    {
        return Err(LeaseError::InvalidIdentifier(value.into()));
    }
    Ok(())
}

fn path_identifier(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("_{byte:02x}"));
        }
    }
    encoded
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

fn reject_symlink_path(path: &Path) -> Result<(), LeaseError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(
            component,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        ) {
            current.push(component.as_os_str());
            continue;
        }
        if matches!(component, std::path::Component::ParentDir) {
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
        let leases = root.path().canonicalize().unwrap().join("leases");
        let first =
            DeviceLease::acquire_in_directory(&leases, "sim-1", "session-a", 10, "start-a", 100)
                .unwrap();
        let busy =
            DeviceLease::acquire_in_directory(&leases, "sim-1", "session-b", 11, "start-b", 101)
                .unwrap_err();
        match busy {
            LeaseError::Busy { owner, .. } => {
                assert_eq!(owner.unwrap().session_id, "session-a");
            }
            other => panic!("expected busy, got {other}"),
        }
        first.release().unwrap();
        let second =
            DeviceLease::acquire_in_directory(&leases, "sim-1", "session-b", 11, "start-b", 102)
                .unwrap();
        assert_eq!(second.owner().fencing_token.len(), 32);
    }

    #[test]
    fn heartbeat_and_release_reject_a_replaced_fencing_owner() {
        let root = tempdir().unwrap();
        let leases = root.path().canonicalize().unwrap().join("leases");
        let mut lease =
            DeviceLease::acquire_in_directory(&leases, "sim-1", "session-a", 10, "start-a", 100)
                .unwrap();
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
            DeviceLease::acquire_in_directory(&leases, "sim-1", "session-b", 11, "start-b", 400)
                .unwrap();
        next.release().unwrap();
    }

    #[test]
    fn session_fences_an_external_workload_before_it_runs() {
        let root = tempdir().unwrap();
        let leases = root.path().canonicalize().unwrap().join("leases");
        let lease = DeviceLease::acquire_in_directory(
            &leases,
            "emulator-5554",
            "session-a",
            10,
            "start-a",
            100,
        )
        .unwrap();
        let owner_path = lease.owner_path().to_owned();
        let session = DeviceLeaseSession::start(lease).unwrap();
        let mut replaced = session.owner().clone();
        replaced.fencing_token = "new-owner".into();
        fs::write(&owner_path, serde_json::to_vec_pretty(&replaced).unwrap()).unwrap();

        let mut ran = false;
        let error = session
            .execute("android.install", || {
                ran = true;
                Ok::<_, anyhow::Error>(())
            })
            .unwrap_err();
        assert!(!ran);
        assert!(error.to_string().contains("device lease lost"));
        drop(session);
    }

    #[test]
    fn delegated_session_rechecks_parent_owner_without_releasing_it() {
        let root = tempdir().unwrap();
        let leases = root.path().canonicalize().unwrap().join("leases");
        let lease = DeviceLease::acquire_in_directory(
            &leases,
            "emulator-5554",
            "session-a",
            10,
            "start-a",
            100,
        )
        .unwrap();
        let parent = DeviceLeaseSession::start(lease).unwrap();
        let delegation = parent.delegation();
        let serialized = serde_json::to_string(&delegation).unwrap();
        assert!(!serialized.contains(parent.fencing_token()));
        let child = DeviceLeaseSession::from_delegation(delegation).unwrap();
        let mut ran = false;
        child
            .execute("android.launch", || {
                ran = true;
                Ok::<_, anyhow::Error>(())
            })
            .unwrap();
        assert!(ran);
        child.release().unwrap();
        assert!(parent.assert_owned().is_ok());
        assert!(parent.owner_path.exists());
        parent.release().unwrap();
    }

    #[test]
    fn host_lease_directory_can_be_shared_by_different_project_roots() {
        let project_a = tempdir().unwrap();
        let project_b = tempdir().unwrap();
        let directory_a = host_lease_directory(project_a.path()).unwrap();
        let directory_b = host_lease_directory(project_b.path()).unwrap();
        assert_eq!(directory_a, directory_b);

        let first = DeviceLease::acquire_in_directory(
            &directory_a,
            "serial:5555",
            "session-a",
            10,
            "start-a",
            100,
        )
        .unwrap();
        assert!(
            !first
                .lock_path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(':')
        );
        let busy = DeviceLease::acquire_in_directory(
            &directory_b,
            "serial:5555",
            "session-b",
            11,
            "start-b",
            101,
        )
        .unwrap_err();
        assert!(matches!(busy, LeaseError::Busy { .. }));
        first.release().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lease_paths_are_rejected() {
        let root = tempdir().unwrap();
        let leases = root.path().join(".gpui/leases");
        fs::create_dir_all(&leases).unwrap();
        std::os::unix::fs::symlink(root.path(), leases.join("sim-1.lock")).unwrap();
        assert!(matches!(
            DeviceLease::acquire_in_directory(&leases, "sim-1", "session-a", 10, "start-a", 100,),
            Err(LeaseError::InvalidOwner(_))
        ));
    }
}
