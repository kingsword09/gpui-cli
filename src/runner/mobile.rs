//! Shared contract and evidence types for local iOS/Android runners.
//!
//! Platform adapters deliberately live outside this module. They must bind
//! every external install, launch, capture, log and cleanup command to the
//! [`DeviceLeaseSession`] passed to the trait methods below.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::lease::DeviceLeaseSession;

pub const RUNNER_CONTRACT_VERSION: u32 = 1;
pub const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunnerInfo {
    pub runner_id: String,
    pub host_id: String,
    pub platform: String,
    pub os: String,
    pub arch: String,
    pub stable_device_id: String,
    pub device_kind: String,
    #[serde(default)]
    pub tool_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub resources: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunnerCapabilities {
    pub install: bool,
    pub launch: bool,
    pub capture: bool,
    pub native_logs: bool,
    pub stop_owned: bool,
    pub rotate: bool,
    pub foreground_probe: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunRequest {
    pub run_id: String,
    pub project_id: String,
    pub device_id: String,
    pub bundle_id: String,
    pub artifact_root: PathBuf,
    #[serde(default)]
    pub abi: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunIdentity {
    pub run_id: String,
    pub project_id: String,
    pub device_id: String,
    pub lease_session_id: String,
    /// The raw token is intentionally never serialized into evidence.
    #[serde(skip)]
    pub fencing_token: String,
    pub fencing_token_sha256: String,
}

impl RunIdentity {
    pub fn verify_lease(&self, lease: &DeviceLeaseSession) -> Result<()> {
        lease.assert_owned().map_err(anyhow::Error::new)?;
        if lease.owner().device_id != self.device_id
            || lease.owner().session_id != self.lease_session_id
            || lease.fencing_token() != self.fencing_token
            || token_sha256(lease.fencing_token()) != self.fencing_token_sha256
        {
            bail!("mobile run identity is not bound to the current device lease");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedRun {
    pub contract_version: u32,
    pub identity: RunIdentity,
    pub bundle_id: String,
    pub artifact_root: PathBuf,
    pub abi: Option<String>,
    pub prepared_at_ms: u64,
}

impl RunRequest {
    pub fn prepare(&self, lease: &DeviceLeaseSession) -> Result<PreparedRun> {
        self.prepare_at(lease, epoch_ms())
    }

    pub fn prepare_at(&self, lease: &DeviceLeaseSession, now_ms: u64) -> Result<PreparedRun> {
        lease.assert_owned().map_err(anyhow::Error::new)?;
        if lease.owner().device_id != self.device_id {
            bail!(
                "run device id '{}' does not match lease device id '{}'",
                self.device_id,
                lease.owner().device_id
            );
        }
        if self.run_id.is_empty() || self.project_id.is_empty() || self.bundle_id.is_empty() {
            bail!("run identity fields must not be empty");
        }
        let fencing_token = lease.fencing_token().to_owned();
        Ok(PreparedRun {
            contract_version: RUNNER_CONTRACT_VERSION,
            identity: RunIdentity {
                run_id: self.run_id.clone(),
                project_id: self.project_id.clone(),
                device_id: self.device_id.clone(),
                lease_session_id: lease.owner().session_id.clone(),
                fencing_token_sha256: token_sha256(&fencing_token),
                fencing_token,
            },
            bundle_id: self.bundle_id.clone(),
            artifact_root: self.artifact_root.clone(),
            abi: self.abi.clone(),
            prepared_at_ms: now_ms,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureScope {
    pub identity: RunIdentity,
    pub output: PathBuf,
    pub attempt: u8,
    pub orientation: Option<String>,
    pub foreground_app: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureArtifact {
    pub artifact_id: String,
    pub path: PathBuf,
    pub provider: String,
    pub bytes: u64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub orientation: Option<String>,
    pub system_ui: bool,
    pub foreground_app: Option<String>,
    pub run_id: String,
}

impl CaptureArtifact {
    /// Reads a completed PNG and records only bounded, reproducible metadata.
    pub fn from_png(
        scope: &CaptureScope,
        provider: impl Into<String>,
        system_ui: bool,
    ) -> Result<Self> {
        scope.identity.verify_output_path(&scope.output)?;
        let metadata = fs::metadata(&scope.output)
            .with_context(|| format!("reading capture metadata at {}", scope.output.display()))?;
        if !metadata.is_file() {
            bail!(
                "capture output is not a regular file: {}",
                scope.output.display()
            );
        }
        if metadata.len() == 0 || metadata.len() > MAX_CAPTURE_BYTES {
            bail!(
                "capture output size {} is outside the allowed range",
                metadata.len()
            );
        }
        let bytes = fs::read(&scope.output)
            .with_context(|| format!("reading capture output {}", scope.output.display()))?;
        let (width, height) = png_dimensions(&bytes)?;
        let sha256 = sha256(&bytes);
        Ok(Self {
            artifact_id: format!("png-{sha256}"),
            path: scope.output.clone(),
            provider: provider.into(),
            bytes: bytes.len() as u64,
            sha256,
            width,
            height,
            orientation: scope.orientation.clone(),
            system_ui,
            foreground_app: scope.foreground_app.clone(),
            run_id: scope.identity.run_id.clone(),
        })
    }
}

impl RunIdentity {
    fn verify_output_path(&self, path: &Path) -> Result<()> {
        if path.as_os_str().is_empty() {
            bail!("capture output path is empty");
        }
        if let Ok(metadata) = fs::symlink_metadata(path)
            && metadata.file_type().is_symlink()
        {
            bail!(
                "refusing to read capture through symbolic link: {}",
                path.display()
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessEvidence {
    pub pid: Option<u32>,
    pub start_token_sha256: Option<String>,
    pub exited: bool,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchEvidence {
    pub run_id: String,
    pub installed: bool,
    pub process: ProcessEvidence,
    pub channel: ChannelState,
    pub at_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelState {
    NotEstablished,
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LogEvidence {
    pub run_id: String,
    pub source: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub truncated: bool,
    pub assigned_to_run: bool,
    pub unassigned_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StopEvidence {
    pub run_id: String,
    pub stopped_owned_process: bool,
    pub removed_owned_resources: Vec<String>,
    pub preserved_resources: Vec<String>,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceEvent {
    pub seq: u64,
    pub at_ms: u64,
    pub run_id: String,
    pub device_id: String,
    pub stage: EvidenceStage,
    pub outcome: EvidenceOutcome,
    pub details: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStage {
    Prepare,
    Install,
    Launch,
    Capture,
    NativeLogs,
    Channel,
    Process,
    Stop,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    Started,
    Succeeded,
    Failed,
    Unavailable,
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceLog {
    pub contract_version: u32,
    pub events: Vec<EvidenceEvent>,
}

impl EvidenceLog {
    pub fn new() -> Self {
        Self {
            contract_version: RUNNER_CONTRACT_VERSION,
            events: Vec::new(),
        }
    }

    pub fn record(
        &mut self,
        identity: &RunIdentity,
        stage: EvidenceStage,
        outcome: EvidenceOutcome,
        at_ms: u64,
        details: Value,
    ) {
        self.events.push(EvidenceEvent {
            seq: self.events.len() as u64 + 1,
            at_ms,
            run_id: identity.run_id.clone(),
            device_id: identity.device_id.clone(),
            stage,
            outcome,
            details,
        });
    }
}

/// The platform adapter boundary for M01/M02.
///
/// Implementations must call [`run_leased_workload`] for every external
/// command that can change device state or produce a capture/log artifact.
pub trait MobileRunner {
    fn describe(&self) -> &RunnerInfo;
    fn capabilities(&self) -> RunnerCapabilities;
    fn prepare(&mut self, request: &RunRequest, lease: &DeviceLeaseSession) -> Result<PreparedRun>;
    fn launch(
        &mut self,
        prepared: &PreparedRun,
        lease: &DeviceLeaseSession,
    ) -> Result<LaunchEvidence>;
    fn capture(
        &mut self,
        scope: &CaptureScope,
        lease: &DeviceLeaseSession,
    ) -> Result<CaptureArtifact>;
    fn collect_logs(
        &mut self,
        identity: &RunIdentity,
        lease: &DeviceLeaseSession,
    ) -> Result<LogEvidence>;
    fn stop_owned(
        &mut self,
        identity: &RunIdentity,
        lease: &DeviceLeaseSession,
    ) -> Result<StopEvidence>;
}

/// Validates the identity and fences one platform workload.
pub fn run_leased_workload<T>(
    identity: &RunIdentity,
    lease: &DeviceLeaseSession,
    stage: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    identity.verify_lease(lease)?;
    lease.execute(stage, operation)
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        bail!("capture is not a PNG with an IHDR header");
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("checked PNG header size"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("checked PNG header size"));
    if width == 0 || height == 0 {
        bail!("capture has zero dimensions");
    }
    Ok((width, height))
}

fn token_sha256(value: &str) -> String {
    sha256(value.as_bytes())
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn lease_root() -> tempfile::TempDir {
        tempdir().unwrap()
    }

    fn request(device_id: &str, root: &Path) -> RunRequest {
        RunRequest {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: device_id.into(),
            bundle_id: "com.example.app".into(),
            artifact_root: root.join("artifacts"),
            abi: None,
        }
    }

    #[test]
    fn prepared_identity_hides_raw_fencing_token_from_json() {
        let root = lease_root();
        let lease = DeviceLeaseSession::acquire(root.path(), "contract-test-device").unwrap();
        let prepared = request("contract-test-device", root.path())
            .prepare_at(&lease, 10)
            .unwrap();
        let json = serde_json::to_string(&prepared).unwrap();
        assert!(!json.contains(lease.fencing_token()));
        assert_eq!(prepared.identity.fencing_token_sha256.len(), 64);
        drop(lease);
    }

    #[test]
    fn mismatched_identity_is_rejected_before_external_work() {
        let root = lease_root();
        let lease = DeviceLeaseSession::acquire(root.path(), "contract-test-device-2").unwrap();
        let prepared = request("contract-test-device-2", root.path())
            .prepare_at(&lease, 10)
            .unwrap();
        let mut wrong = prepared.identity.clone();
        wrong.device_id = "other-device".into();
        let mut ran = false;
        let result = run_leased_workload(&wrong, &lease, "ios.capture", || {
            ran = true;
            Ok::<_, anyhow::Error>(())
        });
        assert!(result.is_err());
        assert!(!ran);
        drop(lease);
    }

    #[test]
    fn evidence_log_sequences_events_and_preserves_unknown_channel_boundary() {
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "secret-token".into(),
            fencing_token_sha256: token_sha256("secret-token"),
        };
        let mut log = EvidenceLog::new();
        log.record(
            &identity,
            EvidenceStage::Launch,
            EvidenceOutcome::Succeeded,
            10,
            serde_json::json!({"process": "started"}),
        );
        log.record(
            &identity,
            EvidenceStage::Channel,
            EvidenceOutcome::Unknown,
            20,
            serde_json::json!({"reason": "transport_disconnected"}),
        );
        assert_eq!(log.events[0].seq, 1);
        assert_eq!(log.events[1].seq, 2);
        assert_eq!(log.events[1].outcome, EvidenceOutcome::Unknown);
    }

    #[test]
    fn capture_artifact_rejects_non_png_and_records_dimensions() {
        let root = lease_root();
        let output = root.path().join("capture.png");
        fs::write(&output, b"not-a-png").unwrap();
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "token".into(),
            fencing_token_sha256: token_sha256("token"),
        };
        let scope = CaptureScope {
            identity: identity.clone(),
            output: output.clone(),
            attempt: 1,
            orientation: Some("portrait".into()),
            foreground_app: Some("com.example.app".into()),
        };
        assert!(CaptureArtifact::from_png(&scope, "simctl", true).is_err());

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        png.extend_from_slice(&2_u32.to_be_bytes());
        png.extend_from_slice(&3_u32.to_be_bytes());
        png.extend_from_slice(&[0; 5]);
        fs::write(&output, png).unwrap();
        let artifact = CaptureArtifact::from_png(&scope, "simctl", true).unwrap();
        assert_eq!((artifact.width, artifact.height), (2, 3));
        assert!(artifact.system_ui);
        assert_eq!(artifact.run_id, identity.run_id);
    }
}
