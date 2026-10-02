//! Shared contract and evidence types for local iOS/Android runners.
//!
//! Platform adapters deliberately live outside this module. They must bind
//! every external install, launch, capture, log and cleanup command to the
//! [`DeviceLeaseSession`] passed to the trait methods below.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::lease::DeviceLeaseSession;

pub const RUNNER_CONTRACT_VERSION: u32 = 1;
pub const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;
pub const CAPTURE_ARTIFACT_MANIFEST_SCHEMA_VERSION: u32 = 1;
/// Maximum number of lifecycle events retained for one mobile run.
pub const MAX_EVIDENCE_EVENTS: usize = 128;
/// Maximum serialized size of one event's `details` value.
pub const MAX_EVIDENCE_DETAIL_BYTES: usize = 16 * 1024;

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
    /// Sidecar manifest path; skipped from wire evidence because it is host-local.
    #[serde(skip)]
    pub manifest_path: PathBuf,
    pub provider: String,
    pub bytes: u64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub logical_width: Option<u32>,
    #[serde(default)]
    pub logical_height: Option<u32>,
    #[serde(default)]
    pub scale_milli: Option<u32>,
    pub orientation: Option<String>,
    pub system_ui: bool,
    pub foreground_app: Option<String>,
    pub run_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureArtifactManifest {
    pub schema_version: u32,
    pub artifact_id: String,
    pub artifact_file: String,
    pub provider: String,
    pub bytes: u64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub logical_width: Option<u32>,
    pub logical_height: Option<u32>,
    pub scale_milli: Option<u32>,
    pub orientation: Option<String>,
    pub system_ui: bool,
    pub foreground_app: Option<String>,
    pub run_id: String,
    pub project_id: String,
    pub device_id: String,
    pub lease_session_id: String,
    pub fencing_token_sha256: String,
}

impl CaptureArtifactManifest {
    fn from_capture(artifact: &CaptureArtifact, identity: &RunIdentity) -> Result<Self> {
        let artifact_file = artifact
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .context("capture output filename is not valid UTF-8")?;
        Ok(Self {
            schema_version: CAPTURE_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            artifact_id: artifact.artifact_id.clone(),
            artifact_file: artifact_file.to_owned(),
            provider: artifact.provider.clone(),
            bytes: artifact.bytes,
            sha256: artifact.sha256.clone(),
            width: artifact.width,
            height: artifact.height,
            logical_width: artifact.logical_width,
            logical_height: artifact.logical_height,
            scale_milli: artifact.scale_milli,
            orientation: artifact.orientation.clone(),
            system_ui: artifact.system_ui,
            foreground_app: artifact.foreground_app.clone(),
            run_id: identity.run_id.clone(),
            project_id: identity.project_id.clone(),
            device_id: identity.device_id.clone(),
            lease_session_id: identity.lease_session_id.clone(),
            fencing_token_sha256: identity.fencing_token_sha256.clone(),
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("reading capture artifact manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing capture artifact manifest {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != CAPTURE_ARTIFACT_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported capture artifact manifest schema {}; expected {}",
                self.schema_version,
                CAPTURE_ARTIFACT_MANIFEST_SCHEMA_VERSION
            );
        }
        if self.artifact_file.is_empty()
            || self.artifact_file.contains('/')
            || self.artifact_file.contains('\\')
        {
            bail!("capture artifact manifest contains an unsafe artifact filename");
        }
        if self.provider.is_empty()
            || self.run_id.is_empty()
            || self.project_id.is_empty()
            || self.device_id.is_empty()
            || self.lease_session_id.is_empty()
        {
            bail!("capture artifact manifest identity fields must not be empty");
        }
        if self.bytes == 0 || self.width == 0 || self.height == 0 {
            bail!("capture artifact manifest contains zero-sized output metadata");
        }
        validate_sha256_value(&self.sha256, "capture hash")?;
        validate_sha256_value(&self.fencing_token_sha256, "fencing token digest")?;
        let expected_id = format!("png-{}", self.sha256);
        if self.artifact_id != expected_id {
            bail!("capture artifact manifest ID does not match its SHA-256");
        }
        Ok(())
    }

    fn verify_capture(&self, artifact: &CaptureArtifact) -> Result<()> {
        self.validate()?;
        let artifact_file = artifact
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .context("capture output filename is not valid UTF-8")?;
        if self.artifact_file != artifact_file
            || self.artifact_id != artifact.artifact_id
            || self.provider != artifact.provider
            || self.bytes != artifact.bytes
            || self.sha256 != artifact.sha256
            || self.width != artifact.width
            || self.height != artifact.height
            || self.logical_width != artifact.logical_width
            || self.logical_height != artifact.logical_height
            || self.scale_milli != artifact.scale_milli
            || self.orientation != artifact.orientation
            || self.system_ui != artifact.system_ui
            || self.foreground_app != artifact.foreground_app
            || self.run_id != artifact.run_id
        {
            bail!("capture artifact does not match its manifest");
        }
        Ok(())
    }

    fn verify_identity(&self, identity: &RunIdentity) -> Result<()> {
        if self.run_id != identity.run_id
            || self.project_id != identity.project_id
            || self.device_id != identity.device_id
            || self.lease_session_id != identity.lease_session_id
            || self.fencing_token_sha256 != identity.fencing_token_sha256
        {
            bail!("capture artifact manifest is not bound to the active run identity");
        }
        Ok(())
    }

    fn write_atomic(&self, path: &Path) -> Result<()> {
        self.validate()?;
        verify_capture_path(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("capture artifact manifest has no parent"))?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!("creating capture artifact manifest in {}", parent.display())
        })?;
        serde_json::to_writer_pretty(temporary.as_file_mut(), self)
            .context("serializing capture artifact manifest")?;
        use std::io::Write;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        if fs::symlink_metadata(path).is_ok() {
            fs::remove_file(path).with_context(|| {
                format!("replacing capture artifact manifest {}", path.display())
            })?;
        }
        temporary.persist(path).map_err(|error| {
            anyhow::anyhow!("publishing capture artifact manifest: {}", error.error)
        })?;
        Ok(())
    }
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
        let artifact = Self {
            artifact_id: format!("png-{sha256}"),
            path: scope.output.clone(),
            manifest_path: capture_manifest_path(&scope.output)?,
            provider: provider.into(),
            bytes: bytes.len() as u64,
            sha256,
            width,
            height,
            logical_width: None,
            logical_height: None,
            scale_milli: None,
            orientation: scope.orientation.clone(),
            system_ui,
            foreground_app: scope.foreground_app.clone(),
            run_id: scope.identity.run_id.clone(),
        };
        artifact.publish_manifest(&scope.identity)?;
        artifact.verify_for_identity(&scope.identity)?;
        Ok(artifact)
    }

    pub fn publish_manifest(&self, identity: &RunIdentity) -> Result<()> {
        let manifest = CaptureArtifactManifest::from_capture(self, identity)?;
        manifest.write_atomic(&self.manifest_path)
    }

    /// Revalidates the on-disk capture before it is published as matrix or
    /// scenario evidence. A runner may return after another process has
    /// replaced the output, so the metadata captured at write time is not
    /// sufficient on its own.
    pub fn verify(&self) -> Result<()> {
        verify_capture_path(&self.path)?;
        let metadata = fs::metadata(&self.path)
            .with_context(|| format!("reading capture metadata at {}", self.path.display()))?;
        if !metadata.is_file() {
            bail!(
                "capture output is not a regular file: {}",
                self.path.display()
            );
        }
        if metadata.len() == 0 || metadata.len() > MAX_CAPTURE_BYTES {
            bail!(
                "capture output size {} is outside the allowed range",
                metadata.len()
            );
        }
        let bytes = fs::read(&self.path)
            .with_context(|| format!("reading capture output {}", self.path.display()))?;
        if bytes.len() as u64 != self.bytes {
            bail!(
                "capture byte count changed: expected {}, found {}",
                self.bytes,
                bytes.len()
            );
        }
        let actual_sha256 = sha256(&bytes);
        if actual_sha256 != self.sha256 {
            bail!("capture SHA-256 changed after publication");
        }
        let (width, height) = png_dimensions(&bytes)?;
        if (width, height) != (self.width, self.height) {
            bail!(
                "capture dimensions changed: expected {}x{}, found {}x{}",
                self.width,
                self.height,
                width,
                height
            );
        }
        let expected_artifact_id = format!("png-{actual_sha256}");
        if self.artifact_id != expected_artifact_id {
            bail!(
                "capture artifact id does not match its SHA-256: expected {expected_artifact_id}"
            );
        }
        let manifest = CaptureArtifactManifest::read(&self.manifest_path)?;
        manifest.verify_capture(self)?;
        Ok(())
    }

    pub fn verify_for_identity(&self, identity: &RunIdentity) -> Result<()> {
        self.verify()?;
        let manifest = CaptureArtifactManifest::read(&self.manifest_path)?;
        manifest.verify_identity(identity)
    }

    /// Projects capture metadata into a bounded event detail value without
    /// exposing the host path or any lease secret.
    pub fn evidence_details(&self) -> Value {
        serde_json::json!({
            "artifact_id": self.artifact_id,
            "manifest_schema_version": CAPTURE_ARTIFACT_MANIFEST_SCHEMA_VERSION,
            "provider": self.provider,
            "bytes": self.bytes,
            "sha256": self.sha256,
            "width": self.width,
            "height": self.height,
            "logical_width": self.logical_width,
            "logical_height": self.logical_height,
            "scale_milli": self.scale_milli,
            "orientation": self.orientation,
            "system_ui": self.system_ui,
            "foreground_app": self.foreground_app,
            "run_id": self.run_id,
        })
    }
}

impl RunIdentity {
    fn verify_output_path(&self, path: &Path) -> Result<()> {
        verify_capture_path(path)
    }
}

fn capture_manifest_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("capture output has no parent"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("capture output filename is not valid UTF-8")?;
    Ok(parent.join(format!("{file_name}.manifest.json")))
}

fn validate_sha256_value(value: &str, field: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must be a 64-character hexadecimal digest");
    }
    Ok(())
}

fn verify_capture_path(path: &Path) -> Result<()> {
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
    /// PID observed at collection time, when the platform could verify one.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Hash of the platform process start identity, never the raw token.
    #[serde(default)]
    pub process_start_token_sha256: Option<String>,
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
    pub project_id: String,
    pub device_id: String,
    pub lease_session_id: String,
    pub fencing_token_sha256: String,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvidenceLog {
    pub contract_version: u32,
    pub events: Vec<EvidenceEvent>,
    /// Set when an event or its details had to be discarded or summarized.
    #[serde(default)]
    pub truncated: bool,
    /// Number of oldest events evicted after reaching `MAX_EVIDENCE_EVENTS`.
    #[serde(default)]
    pub dropped_events: u64,
}

#[derive(Deserialize)]
struct EvidenceLogWire {
    contract_version: u32,
    events: Vec<EvidenceEvent>,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    dropped_events: u64,
}

impl<'de> Deserialize<'de> for EvidenceLog {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EvidenceLogWire::deserialize(deserializer)?;
        let mut log = Self {
            contract_version: wire.contract_version,
            events: Vec::new(),
            truncated: wire.truncated,
            dropped_events: wire.dropped_events,
        };
        for event in wire.events {
            log.push_bounded_event(event);
        }
        Ok(log)
    }
}

impl Default for EvidenceLog {
    fn default() -> Self {
        Self::new()
    }
}

impl EvidenceLog {
    pub fn new() -> Self {
        Self {
            contract_version: RUNNER_CONTRACT_VERSION,
            events: Vec::new(),
            truncated: false,
            dropped_events: 0,
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
        let seq = self
            .events
            .iter()
            .map(|event| event.seq)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.push_bounded_event(EvidenceEvent {
            seq,
            at_ms,
            run_id: identity.run_id.clone(),
            project_id: identity.project_id.clone(),
            device_id: identity.device_id.clone(),
            lease_session_id: identity.lease_session_id.clone(),
            fencing_token_sha256: identity.fencing_token_sha256.clone(),
            stage,
            outcome,
            details,
        });
    }

    fn push_bounded_event(&mut self, mut event: EvidenceEvent) {
        let (details, details_truncated) = bounded_event_details(event.details);
        event.details = details;
        if details_truncated {
            self.truncated = true;
        }
        if self.events.len() >= MAX_EVIDENCE_EVENTS {
            if MAX_EVIDENCE_EVENTS == 0 {
                self.dropped_events = self.dropped_events.saturating_add(1);
                self.truncated = true;
                return;
            }
            self.events.remove(0);
            self.dropped_events = self.dropped_events.saturating_add(1);
            self.truncated = true;
        }
        self.events.push(event);
    }

    /// Records a process observation without treating a missing PID as an
    /// exit. A non-zero exit code is failed evidence, while an exit without a
    /// code remains unknown because the platform has not established why it
    /// stopped.
    pub fn record_process_observation(
        &mut self,
        identity: &RunIdentity,
        process: &ProcessEvidence,
        at_ms: u64,
    ) {
        let outcome = if process.exited {
            match process.exit_code {
                Some(0) => EvidenceOutcome::Succeeded,
                Some(_) => EvidenceOutcome::Failed,
                None => EvidenceOutcome::Unknown,
            }
        } else if process.pid.is_some() {
            EvidenceOutcome::Succeeded
        } else {
            EvidenceOutcome::Unknown
        };
        self.record(
            identity,
            EvidenceStage::Process,
            outcome,
            at_ms,
            serde_json::json!({
                "pid": process.pid,
                "start_token_sha256": process.start_token_sha256,
                "exited": process.exited,
                "exit_code": process.exit_code,
                "classification": if process.exited { "process_exit" } else { "process_observation" },
            }),
        );
    }

    /// Records channel state independently from process state. Disconnect or
    /// a channel that never established is always unknown, never process exit.
    pub fn record_channel_observation(
        &mut self,
        identity: &RunIdentity,
        state: ChannelState,
        reason: Option<&str>,
        at_ms: u64,
    ) {
        let outcome = match state {
            ChannelState::Connected => EvidenceOutcome::Succeeded,
            ChannelState::NotEstablished | ChannelState::Disconnected => EvidenceOutcome::Unknown,
        };
        self.record(
            identity,
            EvidenceStage::Channel,
            outcome,
            at_ms,
            serde_json::json!({
                "state": state,
                "reason": reason,
            }),
        );
    }

    /// Adds the process/channel boundary events for one launch result.
    pub fn record_launch_boundaries(
        &mut self,
        identity: &RunIdentity,
        launch: &LaunchEvidence,
        at_ms: u64,
    ) {
        self.record_process_observation(identity, &launch.process, at_ms);
        self.record_channel_observation(identity, launch.channel, None, at_ms);
    }
}

fn bounded_event_details(details: Value) -> (Value, bool) {
    match serde_json::to_vec(&details) {
        Ok(encoded) if encoded.len() <= MAX_EVIDENCE_DETAIL_BYTES => (details, false),
        Ok(encoded) => (
            serde_json::json!({
                "details_truncated": true,
                "original_bytes": encoded.len(),
                "max_bytes": MAX_EVIDENCE_DETAIL_BYTES,
                "original_sha256": sha256(&encoded),
            }),
            true,
        ),
        Err(_) => (
            serde_json::json!({
                "details_truncated": true,
                "reason": "details_serialization_failed",
                "max_bytes": MAX_EVIDENCE_DETAIL_BYTES,
            }),
            true,
        ),
    }
}

/// The platform adapter boundary for M01/M02.
///
/// Implementations must call [`run_leased_workload`] for every external
/// command that can change device state or produce a capture/log artifact.
pub trait MobileRunner {
    fn describe(&self) -> &RunnerInfo;
    fn capabilities(&self) -> RunnerCapabilities;

    /// Returns the bounded platform event sequence collected by adapters that
    /// implement native lifecycle evidence. Runners without an event log may
    /// leave this unset without changing the lifecycle contract.
    fn evidence_log(&self) -> Option<&EvidenceLog> {
        None
    }

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
    fn capture_evidence_details_keep_environment_without_host_or_lease_secrets() {
        let artifact = CaptureArtifact {
            artifact_id: "png-hash".into(),
            path: PathBuf::from("/private/host/capture.png"),
            manifest_path: PathBuf::new(),
            provider: "adb.exec_out.screencap".into(),
            bytes: 42,
            sha256: "hash".into(),
            width: 1080,
            height: 2400,
            logical_width: Some(411),
            logical_height: Some(914),
            scale_milli: Some(2636),
            orientation: Some("portrait".into()),
            system_ui: true,
            foreground_app: Some("com.example.app".into()),
            run_id: "run-1".into(),
        };
        let details = artifact.evidence_details();
        assert_eq!(details["logical_width"], 411);
        assert_eq!(details["scale_milli"], 2636);
        assert_eq!(details["foreground_app"], "com.example.app");
        assert!(details.get("path").is_none());
        assert!(details.get("fencing_token").is_none());
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
        assert_eq!(log.events[0].project_id, "project-1");
        assert_eq!(log.events[0].lease_session_id, "session-1");
        assert_eq!(log.events[0].fencing_token_sha256.len(), 64);
        let encoded = serde_json::to_string(&log).unwrap();
        assert!(!encoded.contains("secret-token"));
    }

    #[test]
    fn evidence_log_retains_recent_events_and_final_stop_event() {
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "secret-token".into(),
            fencing_token_sha256: token_sha256("secret-token"),
        };
        let mut log = EvidenceLog::new();
        for index in 0..(MAX_EVIDENCE_EVENTS + 3) {
            log.record(
                &identity,
                EvidenceStage::Capture,
                EvidenceOutcome::Succeeded,
                index as u64,
                serde_json::json!({"index": index}),
            );
        }
        log.record(
            &identity,
            EvidenceStage::Stop,
            EvidenceOutcome::Succeeded,
            1000,
            serde_json::json!({"owned": true}),
        );

        assert_eq!(log.events.len(), MAX_EVIDENCE_EVENTS);
        assert!(log.truncated);
        assert_eq!(log.dropped_events, 4);
        assert_eq!(log.events.first().unwrap().seq, 5);
        assert_eq!(
            log.events.last().unwrap().seq,
            (MAX_EVIDENCE_EVENTS + 4) as u64
        );
        assert_eq!(log.events.last().unwrap().stage, EvidenceStage::Stop);
    }

    #[test]
    fn evidence_log_summarizes_oversized_details_without_dropping_event() {
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
            EvidenceStage::NativeLogs,
            EvidenceOutcome::Succeeded,
            10,
            serde_json::json!({"output": "x".repeat(MAX_EVIDENCE_DETAIL_BYTES)}),
        );

        assert_eq!(log.events.len(), 1);
        assert!(log.truncated);
        assert_eq!(log.dropped_events, 0);
        assert_eq!(log.events[0].details["details_truncated"], true);
        assert_eq!(
            log.events[0].details["max_bytes"],
            MAX_EVIDENCE_DETAIL_BYTES
        );
        assert!(
            serde_json::to_vec(&log.events[0].details).unwrap().len() <= MAX_EVIDENCE_DETAIL_BYTES
        );
    }

    #[test]
    fn evidence_log_round_trip_preserves_bounds_and_continues_sequence() {
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "secret-token".into(),
            fencing_token_sha256: token_sha256("secret-token"),
        };
        let mut log = EvidenceLog::new();
        for index in 0..(MAX_EVIDENCE_EVENTS + 1) {
            log.record(
                &identity,
                EvidenceStage::Launch,
                EvidenceOutcome::Succeeded,
                index as u64,
                serde_json::json!({"index": index}),
            );
        }

        let encoded = serde_json::to_vec(&log).unwrap();
        let mut decoded: EvidenceLog = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, log);
        decoded.record(
            &identity,
            EvidenceStage::Stop,
            EvidenceOutcome::Succeeded,
            100,
            serde_json::json!({"owned": true}),
        );
        assert_eq!(
            decoded.events.last().unwrap().seq,
            (MAX_EVIDENCE_EVENTS + 2) as u64
        );
        assert_eq!(decoded.dropped_events, 2);
        assert_eq!(decoded.events.last().unwrap().stage, EvidenceStage::Stop);
    }

    #[test]
    fn evidence_log_deserialization_enforces_event_and_detail_bounds() {
        let events = (0..(MAX_EVIDENCE_EVENTS + 2))
            .map(|index| {
                serde_json::json!({
                    "seq": index + 1,
                    "at_ms": index,
                    "run_id": "run-1",
                    "project_id": "project-1",
                    "device_id": "sim-1",
                    "lease_session_id": "session-1",
                    "fencing_token_sha256": token_sha256("secret-token"),
                    "stage": "launch",
                    "outcome": "succeeded",
                    "details": if index == 0 {
                        serde_json::json!({"output": "x".repeat(MAX_EVIDENCE_DETAIL_BYTES)})
                    } else {
                        serde_json::json!({"index": index})
                    },
                })
            })
            .collect::<Vec<_>>();
        let wire = serde_json::json!({
            "contract_version": RUNNER_CONTRACT_VERSION,
            "events": events,
        });

        let log: EvidenceLog = serde_json::from_value(wire).unwrap();

        assert_eq!(log.events.len(), MAX_EVIDENCE_EVENTS);
        assert!(log.truncated);
        assert_eq!(log.dropped_events, 2);
        assert_eq!(log.events.first().unwrap().seq, 3);
        assert_eq!(
            log.events.last().unwrap().seq,
            (MAX_EVIDENCE_EVENTS + 2) as u64
        );
        assert!(
            serde_json::to_vec(&log.events[0].details).unwrap().len() <= MAX_EVIDENCE_DETAIL_BYTES
        );
    }

    #[test]
    fn process_exit_and_channel_disconnect_remain_independent_faults() {
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "secret-token".into(),
            fencing_token_sha256: token_sha256("secret-token"),
        };
        let mut log = EvidenceLog::new();
        let running = ProcessEvidence {
            pid: Some(42),
            start_token_sha256: Some("hash".into()),
            exited: false,
            exit_code: None,
        };
        log.record_process_observation(&identity, &running, 10);
        log.record_channel_observation(
            &identity,
            ChannelState::Disconnected,
            Some("transport_disconnected"),
            20,
        );

        assert_eq!(log.events[0].stage, EvidenceStage::Process);
        assert_eq!(log.events[0].outcome, EvidenceOutcome::Succeeded);
        assert_eq!(log.events[1].stage, EvidenceStage::Channel);
        assert_eq!(log.events[1].outcome, EvidenceOutcome::Unknown);
        assert_eq!(log.events[1].details["state"], "disconnected");
    }

    #[test]
    fn process_exit_without_code_is_unknown_but_nonzero_exit_is_failed() {
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "secret-token".into(),
            fencing_token_sha256: token_sha256("secret-token"),
        };
        let mut log = EvidenceLog::new();
        log.record_process_observation(
            &identity,
            &ProcessEvidence {
                pid: None,
                start_token_sha256: None,
                exited: true,
                exit_code: None,
            },
            10,
        );
        log.record_process_observation(
            &identity,
            &ProcessEvidence {
                pid: Some(42),
                start_token_sha256: None,
                exited: true,
                exit_code: Some(139),
            },
            20,
        );
        assert_eq!(log.events[0].outcome, EvidenceOutcome::Unknown);
        assert_eq!(log.events[1].outcome, EvidenceOutcome::Failed);
        assert_eq!(log.events[1].details["classification"], "process_exit");
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
        assert!(artifact.manifest_path.is_file());
        let manifest = CaptureArtifactManifest::read(&artifact.manifest_path).unwrap();
        assert_eq!(manifest.project_id, "project-1");
        assert_eq!(manifest.device_id, "sim-1");
        assert_eq!(manifest.fencing_token_sha256.len(), 64);
        assert!(
            !serde_json::to_string(&manifest)
                .unwrap()
                .contains("fencing_token\":\"token")
        );
        artifact.verify_for_identity(&scope.identity).unwrap();
    }

    #[test]
    fn capture_artifact_verification_rejects_replaced_output() {
        let root = lease_root();
        let output = root.path().join("capture.png");
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "token".into(),
            fencing_token_sha256: token_sha256("token"),
        };
        let scope = CaptureScope {
            identity,
            output: output.clone(),
            attempt: 1,
            orientation: Some("portrait".into()),
            foreground_app: Some("com.example.app".into()),
        };
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        png.extend_from_slice(&2_u32.to_be_bytes());
        png.extend_from_slice(&3_u32.to_be_bytes());
        png.extend_from_slice(&[0; 5]);
        fs::write(&output, png).unwrap();
        let artifact = CaptureArtifact::from_png(&scope, "simctl", true).unwrap();
        artifact.verify().unwrap();

        fs::write(&output, b"replaced").unwrap();
        let error = artifact.verify().unwrap_err();
        assert!(error.to_string().contains("capture byte count changed"));
    }

    #[test]
    fn capture_artifact_verification_rejects_tampered_manifest() {
        let root = lease_root();
        let output = root.path().join("capture.png");
        let identity = RunIdentity {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "sim-1".into(),
            lease_session_id: "session-1".into(),
            fencing_token: "token".into(),
            fencing_token_sha256: token_sha256("token"),
        };
        let scope = CaptureScope {
            identity,
            output: output.clone(),
            attempt: 1,
            orientation: Some("portrait".into()),
            foreground_app: Some("com.example.app".into()),
        };
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        png.extend_from_slice(&2_u32.to_be_bytes());
        png.extend_from_slice(&3_u32.to_be_bytes());
        png.extend_from_slice(&[0; 5]);
        fs::write(&output, png).unwrap();
        let artifact = CaptureArtifact::from_png(&scope, "simctl", true).unwrap();
        let mut manifest = CaptureArtifactManifest::read(&artifact.manifest_path).unwrap();
        manifest.bytes += 1;
        fs::write(
            &artifact.manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        assert!(artifact.verify_for_identity(&scope.identity).is_err());
    }
}
