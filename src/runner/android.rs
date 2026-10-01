//! Android emulator/device implementation of the shared mobile runner contract.

use anyhow::{Result, bail};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::lease::DeviceLeaseSession;
use super::mobile::{
    CaptureArtifact, CaptureScope, ChannelState, EvidenceLog, EvidenceOutcome, EvidenceStage,
    LaunchEvidence, LogEvidence, MobileRunner, PreparedRun, ProcessEvidence, RunIdentity,
    RunRequest, RunnerCapabilities, RunnerInfo, StopEvidence, run_leased_workload,
};
use crate::device::android;

pub struct AndroidRunner {
    info: RunnerInfo,
    capabilities: RunnerCapabilities,
    serial: String,
    bundle_id: String,
    apk: PathBuf,
    artifact_root: PathBuf,
    evidence: EvidenceLog,
}

impl AndroidRunner {
    pub fn new(
        serial: impl Into<String>,
        apk: impl Into<PathBuf>,
        bundle_id: impl Into<String>,
        artifact_root: impl Into<PathBuf>,
    ) -> Self {
        let serial = serial.into();
        let bundle_id = bundle_id.into();
        let apk = apk.into();
        let artifact_root = artifact_root.into();
        let mut tool_versions = BTreeMap::new();
        tool_versions.insert("adb".into(), "required-at-runtime".into());
        Self {
            info: RunnerInfo {
                runner_id: format!("android:{serial}"),
                host_id: std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into()),
                platform: "android".into(),
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                stable_device_id: serial.clone(),
                device_kind: "emulator_or_device".into(),
                tool_versions,
                resources: vec![serial.clone()],
            },
            capabilities: RunnerCapabilities {
                install: true,
                launch: true,
                capture: true,
                native_logs: true,
                stop_owned: true,
                rotate: false,
                foreground_probe: false,
            },
            serial,
            bundle_id,
            apk,
            artifact_root,
            evidence: EvidenceLog::new(),
        }
    }

    pub fn evidence(&self) -> &EvidenceLog {
        &self.evidence
    }

    fn record<T>(
        &mut self,
        identity: &RunIdentity,
        stage: EvidenceStage,
        result: &Result<T>,
        details: serde_json::Value,
    ) {
        self.evidence.record(
            identity,
            stage,
            if result.is_ok() {
                EvidenceOutcome::Succeeded
            } else {
                EvidenceOutcome::Failed
            },
            now_ms(),
            details,
        );
    }

    fn log_path(&self, identity: &RunIdentity) -> PathBuf {
        self.artifact_root
            .join("logs")
            .join(format!("{}.android.log", safe_component(&identity.run_id)))
    }

    fn check_target(&self, request: &RunRequest) -> Result<()> {
        if request.device_id != self.serial {
            bail!(
                "Android runner is bound to '{}', not '{}'",
                self.serial,
                request.device_id
            );
        }
        if request.bundle_id != self.bundle_id {
            bail!(
                "Android runner bundle id is '{}', not '{}'",
                self.bundle_id,
                request.bundle_id
            );
        }
        Ok(())
    }
}

impl MobileRunner for AndroidRunner {
    fn describe(&self) -> &RunnerInfo {
        &self.info
    }

    fn capabilities(&self) -> RunnerCapabilities {
        self.capabilities.clone()
    }

    fn evidence_log(&self) -> Option<&EvidenceLog> {
        Some(&self.evidence)
    }

    fn prepare(&mut self, request: &RunRequest, lease: &DeviceLeaseSession) -> Result<PreparedRun> {
        self.check_target(request)?;
        let prepared = request.prepare(lease)?;
        let serial = self.serial.clone();
        let apk = self.apk.clone();
        let result = run_leased_workload(&prepared.identity, lease, "android.install", || {
            android::install_apk(&serial, &apk)
        });
        self.record(
            &prepared.identity,
            EvidenceStage::Install,
            &result,
            json!({"serial": self.serial, "apk": self.apk}),
        );
        result.map(|()| prepared)
    }

    fn launch(
        &mut self,
        prepared: &PreparedRun,
        lease: &DeviceLeaseSession,
    ) -> Result<LaunchEvidence> {
        prepared.identity.verify_lease(lease)?;
        let serial = self.serial.clone();
        let bundle_id = self.bundle_id.clone();
        let mut observed_process = None;
        let result = run_leased_workload(&prepared.identity, lease, "android.launch", || {
            android::force_stop(&serial, &bundle_id)?;
            android::launch_app(&serial, &bundle_id)?;
            let process = android::wait_for_app_process(&serial, &bundle_id)?;
            let pid = process.as_ref().map(|process| process.pid);
            let start_token_sha256 = process.as_ref().map(process_start_token_sha256);
            observed_process = process.clone();
            Ok(LaunchEvidence {
                run_id: prepared.identity.run_id.clone(),
                installed: true,
                process: ProcessEvidence {
                    pid,
                    start_token_sha256,
                    exited: false,
                    exit_code: None,
                },
                channel: ChannelState::NotEstablished,
                at_ms: now_ms(),
            })
        });
        self.record(
            &prepared.identity,
            EvidenceStage::Launch,
            &result,
            json!({
                "serial": self.serial,
                "bundle_id": self.bundle_id,
                "process": process_details(observed_process.as_ref()),
            }),
        );
        if let Ok(launch) = &result {
            self.evidence
                .record_launch_boundaries(&prepared.identity, launch, now_ms());
        }
        result
    }

    fn capture(
        &mut self,
        scope: &CaptureScope,
        lease: &DeviceLeaseSession,
    ) -> Result<CaptureArtifact> {
        let output = scope.output.clone();
        let serial = self.serial.clone();
        let bundle_id = self.bundle_id.clone();
        let result = run_leased_workload(&scope.identity, lease, "android.capture", || {
            android::capture_screenshot(&serial, &output)?;
            let mut artifact = CaptureArtifact::from_png(scope, "adb.exec_out.screencap", true)?;
            if let Ok(display) = android::display_evidence(&serial, &bundle_id) {
                artifact.logical_width = display.logical_width;
                artifact.logical_height = display.logical_height;
                artifact.scale_milli = display.scale_milli;
                artifact.orientation = display.orientation;
                artifact.foreground_app = display.foreground_app;
            }
            artifact.publish_manifest(&scope.identity)?;
            Ok(artifact)
        });
        self.record(
            &scope.identity,
            EvidenceStage::Capture,
            &result,
            json!({
                "provider": "adb.exec_out.screencap",
                "system_ui": true,
                "serial": self.serial,
                "artifact": result.as_ref().ok().map(CaptureArtifact::evidence_details),
            }),
        );
        result
    }

    fn collect_logs(
        &mut self,
        identity: &RunIdentity,
        lease: &DeviceLeaseSession,
    ) -> Result<LogEvidence> {
        identity.verify_lease(lease)?;
        let output = self.log_path(identity);
        let serial = self.serial.clone();
        let bundle_id = self.bundle_id.clone();
        let mut observed_process = None;
        let result = run_leased_workload(identity, lease, "android.native_logs", || {
            let process = android::app_process_identity(&serial, &bundle_id)?;
            let pid = process.as_ref().map(|process| process.pid);
            let start_token_sha256 = process.as_ref().map(process_start_token_sha256);
            observed_process = process.clone();
            let (bytes, truncated) = android::collect_logcat(&serial, &output, pid)?;
            Ok(LogEvidence {
                run_id: identity.run_id.clone(),
                source: "adb.logcat".into(),
                path: output.clone(),
                bytes,
                truncated,
                pid,
                process_start_token_sha256: start_token_sha256,
                assigned_to_run: process.is_some(),
                unassigned_reason: process
                    .is_none()
                    .then(|| "no verified package process identity at log collection time".into()),
            })
        });
        self.record(
            identity,
            EvidenceStage::NativeLogs,
            &result,
            json!({
                "source": "adb.logcat",
                "serial": self.serial,
                "process": process_details(observed_process.as_ref()),
            }),
        );
        result
    }

    fn stop_owned(
        &mut self,
        identity: &RunIdentity,
        lease: &DeviceLeaseSession,
    ) -> Result<StopEvidence> {
        let serial = self.serial.clone();
        let bundle_id = self.bundle_id.clone();
        let result = run_leased_workload(identity, lease, "android.stop", || {
            android::force_stop(&serial, &bundle_id)?;
            Ok(StopEvidence {
                run_id: identity.run_id.clone(),
                stopped_owned_process: true,
                removed_owned_resources: Vec::new(),
                preserved_resources: vec![serial.clone()],
                at_ms: now_ms(),
            })
        });
        self.record(
            identity,
            EvidenceStage::Stop,
            &result,
            json!({"serial": self.serial, "bundle_id": self.bundle_id}),
        );
        result
    }
}

fn process_start_token_sha256(process: &android::AppProcessIdentity) -> String {
    format!("{:x}", Sha256::digest(process.start_token().as_bytes()))
}

fn process_details(process: Option<&android::AppProcessIdentity>) -> serde_json::Value {
    match process {
        Some(process) => json!({
            "verified": true,
            "pid": process.pid,
            "start_token_sha256": process_start_token_sha256(process),
            "boot_id_available": process.boot_id.is_some(),
        }),
        None => json!({
            "verified": false,
            "reason": "process_identity_unavailable",
        }),
    }
}

fn safe_component(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            result.push(byte as char);
        } else {
            result.push('_');
        }
    }
    if result.is_empty() {
        "run".into()
    } else {
        result
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn describes_the_serial_and_does_not_claim_foreground_probe() {
        let runner = AndroidRunner::new(
            "emulator-5554",
            "/tmp/app.apk",
            "com.example.app",
            "/tmp/artifacts",
        );
        assert_eq!(runner.describe().stable_device_id, "emulator-5554");
        assert!(runner.capabilities().native_logs);
        assert!(!runner.capabilities().foreground_probe);
    }

    #[test]
    fn log_path_stays_under_the_artifact_root() {
        let root = tempdir().unwrap();
        let runner = AndroidRunner::new(
            "emulator-5554",
            "/tmp/app.apk",
            "com.example.app",
            root.path(),
        );
        let identity = RunIdentity {
            run_id: "run/../../x".into(),
            project_id: "project".into(),
            device_id: "emulator-5554".into(),
            lease_session_id: "session".into(),
            fencing_token: "token".into(),
            fencing_token_sha256: "hash".into(),
        };
        assert!(runner.log_path(&identity).starts_with(root.path()));
    }

    #[test]
    fn evidence_details_hash_process_identity_without_exposing_raw_boot_id() {
        let process = android::AppProcessIdentity {
            pid: 42,
            start_time_ticks: 99,
            boot_id: Some("private-boot-id".into()),
        };
        let details = process_details(Some(&process));
        let rendered = details.to_string();
        assert_eq!(details["pid"], 42);
        assert_eq!(details["verified"], true);
        assert_eq!(details["boot_id_available"], true);
        assert!(!rendered.contains("private-boot-id"));
        assert_eq!(details["start_token_sha256"].as_str().unwrap().len(), 64);
    }
}
