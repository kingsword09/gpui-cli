//! iOS simulator implementation of the shared mobile runner contract.

use anyhow::{Result, bail};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::lease::DeviceLeaseSession;
use super::mobile::{
    CaptureArtifact, CaptureScope, ChannelState, EvidenceLog, EvidenceOutcome, EvidenceStage,
    LaunchEvidence, LogEvidence, MobileRunner, PreparedRun, ProcessEvidence, RunIdentity,
    RunRequest, RunnerCapabilities, RunnerInfo, StopEvidence, run_leased_workload,
};
use crate::device::ios;

pub struct IosSimulatorRunner {
    info: RunnerInfo,
    capabilities: RunnerCapabilities,
    udid: String,
    bundle_id: String,
    app: PathBuf,
    env: Vec<(String, String)>,
    artifact_root: PathBuf,
    evidence: EvidenceLog,
}

impl IosSimulatorRunner {
    pub fn new(
        udid: impl Into<String>,
        app: impl Into<PathBuf>,
        bundle_id: impl Into<String>,
        artifact_root: impl Into<PathBuf>,
    ) -> Self {
        let udid = udid.into();
        let bundle_id = bundle_id.into();
        let app = app.into();
        let artifact_root = artifact_root.into();
        let mut tool_versions = BTreeMap::new();
        tool_versions.insert("xcrun".into(), "required-at-runtime".into());
        Self {
            info: RunnerInfo {
                runner_id: format!("ios-simulator:{udid}"),
                host_id: std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into()),
                platform: "ios".into(),
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                stable_device_id: udid.clone(),
                device_kind: "emulator".into(),
                tool_versions,
                resources: vec![udid.clone()],
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
            udid,
            bundle_id,
            app,
            env: Vec::new(),
            artifact_root,
            evidence: EvidenceLog::new(),
        }
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
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
            .join(format!("{}.ios.log", safe_component(&identity.run_id)))
    }

    fn check_target(&self, request: &RunRequest) -> Result<()> {
        if request.device_id != self.udid {
            bail!(
                "iOS simulator runner is bound to '{}', not '{}'",
                self.udid,
                request.device_id
            );
        }
        if request.bundle_id != self.bundle_id {
            bail!(
                "iOS simulator runner bundle id is '{}', not '{}'",
                self.bundle_id,
                request.bundle_id
            );
        }
        Ok(())
    }
}

impl MobileRunner for IosSimulatorRunner {
    fn describe(&self) -> &RunnerInfo {
        &self.info
    }

    fn capabilities(&self) -> RunnerCapabilities {
        self.capabilities.clone()
    }

    fn prepare(&mut self, request: &RunRequest, lease: &DeviceLeaseSession) -> Result<PreparedRun> {
        self.check_target(request)?;
        let prepared = request.prepare(lease)?;
        let app = self.app.clone();
        let udid = self.udid.clone();
        let result = run_leased_workload(&prepared.identity, lease, "ios.install", || {
            ios::install_simulator(&udid, &app)
        });
        self.record(
            &prepared.identity,
            EvidenceStage::Install,
            &result,
            json!({"udid": self.udid, "app": self.app}),
        );
        result.map(|()| prepared)
    }

    fn launch(
        &mut self,
        prepared: &PreparedRun,
        lease: &DeviceLeaseSession,
    ) -> Result<LaunchEvidence> {
        prepared.identity.verify_lease(lease)?;
        let udid = self.udid.clone();
        let bundle_id = self.bundle_id.clone();
        let env = self.env.clone();
        let result = run_leased_workload(&prepared.identity, lease, "ios.launch", || {
            ios::launch_simulator_with_env(&udid, &bundle_id, &env)?;
            Ok(LaunchEvidence {
                run_id: prepared.identity.run_id.clone(),
                installed: true,
                process: ProcessEvidence {
                    pid: None,
                    start_token_sha256: None,
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
            json!({"udid": self.udid, "bundle_id": self.bundle_id}),
        );
        result
    }

    fn capture(
        &mut self,
        scope: &CaptureScope,
        lease: &DeviceLeaseSession,
    ) -> Result<CaptureArtifact> {
        let output = scope.output.clone();
        let udid = self.udid.clone();
        let result = run_leased_workload(&scope.identity, lease, "ios.capture", || {
            ios::capture_simulator_screenshot(&udid, &output)?;
            CaptureArtifact::from_png(scope, "simctl", true)
        });
        self.record(
            &scope.identity,
            EvidenceStage::Capture,
            &result,
            json!({"provider": "simctl", "system_ui": true, "udid": self.udid}),
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
        let udid = self.udid.clone();
        let result = run_leased_workload(identity, lease, "ios.native_logs", || {
            let (bytes, truncated) = ios::collect_simulator_logs(&udid, &output)?;
            Ok(LogEvidence {
                run_id: identity.run_id.clone(),
                source: "simctl.log.show".into(),
                path: output.clone(),
                bytes,
                truncated,
                pid: None,
                process_start_token_sha256: None,
                assigned_to_run: false,
                unassigned_reason: Some(
                    "simulator log snapshot has no verified process identity".into(),
                ),
            })
        });
        self.record(
            identity,
            EvidenceStage::NativeLogs,
            &result,
            json!({"source": "simctl.log.show", "assigned_to_run": false}),
        );
        result
    }

    fn stop_owned(
        &mut self,
        identity: &RunIdentity,
        lease: &DeviceLeaseSession,
    ) -> Result<StopEvidence> {
        let udid = self.udid.clone();
        let bundle_id = self.bundle_id.clone();
        let result = run_leased_workload(identity, lease, "ios.stop", || {
            ios::terminate_simulator(&udid, &bundle_id)?;
            Ok(StopEvidence {
                run_id: identity.run_id.clone(),
                stopped_owned_process: true,
                removed_owned_resources: Vec::new(),
                preserved_resources: vec![udid.clone()],
                at_ms: now_ms(),
            })
        });
        self.record(
            identity,
            EvidenceStage::Stop,
            &result,
            json!({"udid": self.udid, "bundle_id": self.bundle_id}),
        );
        result
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
    fn describes_a_bound_simulator_without_claiming_foreground_probe() {
        let runner = IosSimulatorRunner::new(
            "SIM-123",
            "/tmp/App.app",
            "com.example.app",
            "/tmp/artifacts",
        );
        assert_eq!(runner.describe().stable_device_id, "SIM-123");
        assert!(runner.capabilities().capture);
        assert!(!runner.capabilities().foreground_probe);
    }

    #[test]
    fn run_ids_are_safe_log_file_components() {
        assert_eq!(safe_component("run/../../old"), "run_.._.._old");
        assert_eq!(safe_component(""), "run");
    }

    #[test]
    fn log_path_stays_under_the_artifact_root() {
        let root = tempdir().unwrap();
        let runner =
            IosSimulatorRunner::new("SIM-123", "/tmp/App.app", "com.example.app", root.path());
        let identity = RunIdentity {
            run_id: "run/../../x".into(),
            project_id: "project".into(),
            device_id: "SIM-123".into(),
            lease_session_id: "session".into(),
            fencing_token: "token".into(),
            fencing_token_sha256: "hash".into(),
        };
        assert!(runner.log_path(&identity).starts_with(root.path()));
    }
}
