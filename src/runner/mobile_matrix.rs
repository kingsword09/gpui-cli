//! Matrix cell adapter for the lease-bound mobile runners.
//!
//! This adapter owns one mobile runner, one host device lease and one run
//! identity. A cell performs the evidence-producing lifecycle
//! prepare -> launch -> capture -> native logs. Cleanup stops only the
//! prepared run and then releases the lease, including when launch or capture
//! fails after installation has completed.

use super::lease::DeviceLeaseSession;
use super::matrix::{MatrixCellSpec, MatrixCellState};
use super::matrix_executor::{MatrixCellExecution, MatrixCellRunner};
use super::mobile::{
    CaptureArtifact, CaptureScope, LaunchEvidence, LogEvidence, MobileRunner, PreparedRun,
    RunIdentity, RunRequest, StopEvidence,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

pub struct MobileMatrixCellRunner<R> {
    runner: R,
    lease: Option<DeviceLeaseSession>,
    request: RunRequest,
    artifact_root: PathBuf,
    run_identity: Option<RunIdentity>,
    prepared: Option<PreparedRun>,
    launch: Option<LaunchEvidence>,
    captures: Vec<CaptureArtifact>,
    native_logs: Option<LogEvidence>,
    stop: Option<StopEvidence>,
    cleanup_errors: Vec<String>,
    lease_released: bool,
}

impl<R: MobileRunner> MobileMatrixCellRunner<R> {
    pub fn new(
        runner: R,
        lease: DeviceLeaseSession,
        request: RunRequest,
        artifact_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            runner,
            lease: Some(lease),
            request,
            artifact_root: artifact_root.into(),
            run_identity: None,
            prepared: None,
            launch: None,
            captures: Vec::new(),
            native_logs: None,
            stop: None,
            cleanup_errors: Vec::new(),
            lease_released: false,
        }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    fn check_deadline(deadline: Instant, stage: &str) -> Result<()> {
        if Instant::now() >= deadline {
            bail!("mobile matrix cell deadline exceeded before {stage}");
        }
        Ok(())
    }

    fn capture_path(&self, cell: &MatrixCellSpec) -> PathBuf {
        self.artifact_root.join("captures").join(format!(
            "{}-{}.png",
            safe_component(&self.request.run_id),
            safe_component(&cell.cell_id)
        ))
    }
}

impl<R: MobileRunner + Send> MatrixCellRunner for MobileMatrixCellRunner<R> {
    fn run_cell(
        &mut self,
        cell: &MatrixCellSpec,
        deadline: Instant,
    ) -> Result<MatrixCellExecution> {
        Self::check_deadline(deadline, "prepare")?;
        // Establish the run identity before the platform adapter starts
        // installing. If the adapter fails after a side effect, cleanup still
        // has an identity with which to fence stop_owned.
        let prepared_seed = {
            let lease = self.lease.as_ref().ok_or_else(|| {
                anyhow::anyhow!("mobile matrix cell lease has already been released")
            })?;
            self.request.prepare(lease)?
        };
        self.run_identity = Some(prepared_seed.identity.clone());
        self.prepared = Some(prepared_seed);
        let prepared = {
            let lease = self.lease.as_ref().ok_or_else(|| {
                anyhow::anyhow!("mobile matrix cell lease has already been released")
            })?;
            self.runner.prepare(&self.request, lease)?
        };
        self.run_identity = Some(prepared.identity.clone());
        self.prepared = Some(prepared.clone());

        Self::check_deadline(deadline, "launch")?;
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mobile matrix cell lease has already been released"))?;
        let launch = self.runner.launch(&prepared, lease)?;
        self.launch = Some(launch.clone());
        if launch.process.exited {
            return Ok(MatrixCellExecution {
                status: MatrixCellState::Failed,
                error: Some(super::matrix::MatrixCellError {
                    code: "mobile_process_exited_during_launch".into(),
                    message: "mobile process exited during runner launch".into(),
                }),
                artifact_ids: Vec::new(),
                context: None,
                check_report: None,
            });
        }

        Self::check_deadline(deadline, "capture")?;
        let output = self.capture_path(cell);
        let parent = output
            .parent()
            .ok_or_else(|| anyhow::anyhow!("mobile capture path has no parent"))?;
        fs::create_dir_all(parent)?;
        let scope = CaptureScope {
            identity: prepared.identity.clone(),
            output,
            attempt: 1,
            orientation: None,
            foreground_app: None,
        };
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mobile matrix cell lease has already been released"))?;
        let capture = self
            .runner
            .capture(&scope, lease)
            .context("capturing mobile screenshot")?;
        capture
            .verify()
            .context("verifying mobile screenshot artifact")?;
        self.captures.push(capture.clone());

        Self::check_deadline(deadline, "native logs")?;
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mobile matrix cell lease has already been released"))?;
        let logs = self
            .runner
            .collect_logs(&prepared.identity, lease)
            .map_err(|error| anyhow::anyhow!("collecting mobile native logs: {error:#}"))?;
        self.native_logs = Some(logs.clone());

        let (status, error) = if launch.process.pid.is_none() {
            (
                MatrixCellState::Inconclusive,
                Some(super::matrix::MatrixCellError {
                    code: "mobile_process_identity_unavailable".into(),
                    message: "launch succeeded without a verified process identity".into(),
                }),
            )
        } else if !logs.assigned_to_run {
            (
                MatrixCellState::Inconclusive,
                Some(super::matrix::MatrixCellError {
                    code: "mobile_logs_unassigned".into(),
                    message: "native logs could not be assigned to this mobile run".into(),
                }),
            )
        } else {
            (MatrixCellState::Passed, None)
        };
        Ok(MatrixCellExecution {
            status,
            error,
            artifact_ids: vec![capture.artifact_id],
            context: None,
            check_report: None,
        })
    }

    fn cleanup_cell(&mut self, _cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
        let prepared = self.prepared.take();
        let mut errors = Vec::new();
        match (prepared.as_ref(), self.lease.as_ref()) {
            (Some(prepared), Some(lease)) => {
                match self.runner.stop_owned(&prepared.identity, lease) {
                    Ok(stop) => self.stop = Some(stop),
                    Err(error) => errors.push(format!("stopping mobile run: {error:#}")),
                }
            }
            (Some(_), None) => {
                errors.push("mobile run was prepared after its lease was released".into())
            }
            (None, _) => {}
        }
        if let Some(lease) = self.lease.take() {
            match lease.release() {
                Ok(()) => self.lease_released = true,
                Err(error) => errors.push(format!("releasing mobile device lease: {error}")),
            }
        }
        self.cleanup_errors.extend(errors.iter().cloned());
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("{}", errors.join("; "))
        }
    }

    fn context(&self) -> Option<crate::scenario::executor::CheckContext> {
        let relative_path = |path: &std::path::Path| {
            path.strip_prefix(&self.artifact_root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        };
        Some(crate::scenario::executor::CheckContext {
            reset_generation: None,
            snapshot_hash: None,
            build_key: None,
            environment: None,
            uncontrolled_inputs: Vec::new(),
            mobile_evidence: Some(json!({
                "run_id": self.request.run_id,
                "device_id": self.request.device_id,
                "run_id_bound": self.run_identity.is_some(),
                "run_identity": self.run_identity.as_ref().map(|identity| json!({
                    "run_id": identity.run_id,
                    "project_id": identity.project_id,
                    "device_id": identity.device_id,
                    "lease_session_id": identity.lease_session_id,
                    "fencing_token_sha256": identity.fencing_token_sha256,
                })),
                "runner": self.runner.describe(),
                "capabilities": self.runner.capabilities(),
                "event_log": self.runner.evidence_log(),
                "lease_released": self.lease_released,
                "launch": self.launch.as_ref().map(|launch| json!({
                    "run_id": launch.run_id,
                    "installed": launch.installed,
                    "process": launch.process,
                    "channel": launch.channel,
                    "at_ms": launch.at_ms,
                })),
                "captures": self.captures.iter().map(|capture| json!({
                    "artifact_id": capture.artifact_id,
                    "provider": capture.provider,
                    "path": relative_path(&capture.path),
                    "bytes": capture.bytes,
                    "sha256": capture.sha256,
                    "width": capture.width,
                    "height": capture.height,
                    "logical_width": capture.logical_width,
                    "logical_height": capture.logical_height,
                    "scale_milli": capture.scale_milli,
                    "orientation": capture.orientation,
                    "system_ui": capture.system_ui,
                    "foreground_app": capture.foreground_app,
                    "run_id": capture.run_id,
                })).collect::<Vec<_>>(),
                "native_logs": self.native_logs.as_ref().map(|logs| json!({
                    "run_id": logs.run_id,
                    "source": logs.source,
                    "path": relative_path(&logs.path),
                    "bytes": logs.bytes,
                    "truncated": logs.truncated,
                    "pid": logs.pid,
                    "process_start_token_sha256": logs.process_start_token_sha256,
                    "assigned_to_run": logs.assigned_to_run,
                    "unassigned_reason": logs.unassigned_reason,
                })),
                "stop": self.stop.as_ref().map(|stop| json!({
                    "run_id": stop.run_id,
                    "stopped_owned_process": stop.stopped_owned_process,
                    "removed_owned_resources": stop.removed_owned_resources,
                    "preserved_resources": stop.preserved_resources,
                    "at_ms": stop.at_ms,
                })),
                "cleanup_errors": self.cleanup_errors,
            })),
        })
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
        "cell".into()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::lease::DeviceLeaseSession;
    use crate::runner::matrix::MatrixCellState;
    use crate::runner::mobile::{
        CaptureArtifact, ChannelState, EvidenceLog, LaunchEvidence, LogEvidence, ProcessEvidence,
        RunIdentity, RunnerCapabilities, RunnerInfo, StopEvidence,
    };
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::tempdir;

    fn epoch_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    struct FakeMobileRunner {
        events: Arc<Mutex<Vec<String>>>,
        fail_launch: bool,
        unverified_process: bool,
        evidence: EvidenceLog,
    }

    impl FakeMobileRunner {
        fn event(&self, value: &str) {
            self.events.lock().unwrap().push(value.into());
        }
    }

    impl MobileRunner for FakeMobileRunner {
        fn describe(&self) -> &RunnerInfo {
            static INFO: std::sync::OnceLock<RunnerInfo> = std::sync::OnceLock::new();
            INFO.get_or_init(|| RunnerInfo {
                runner_id: "fake-mobile".into(),
                host_id: "test".into(),
                platform: "android".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                stable_device_id: "fake-device".into(),
                device_kind: "emulator".into(),
                tool_versions: BTreeMap::new(),
                resources: vec!["fake-device".into()],
            })
        }

        fn capabilities(&self) -> RunnerCapabilities {
            RunnerCapabilities {
                install: true,
                launch: true,
                capture: true,
                native_logs: true,
                stop_owned: true,
                rotate: false,
                foreground_probe: false,
            }
        }

        fn evidence_log(&self) -> Option<&EvidenceLog> {
            Some(&self.evidence)
        }

        fn prepare(
            &mut self,
            request: &RunRequest,
            lease: &DeviceLeaseSession,
        ) -> Result<PreparedRun> {
            self.event("prepare");
            request.prepare_at(lease, epoch_ms())
        }

        fn launch(
            &mut self,
            prepared: &PreparedRun,
            _lease: &DeviceLeaseSession,
        ) -> Result<LaunchEvidence> {
            self.event("launch");
            if self.fail_launch {
                bail!("fake launch failed");
            }
            Ok(LaunchEvidence {
                run_id: prepared.identity.run_id.clone(),
                installed: true,
                process: ProcessEvidence {
                    pid: (!self.unverified_process).then_some(42),
                    start_token_sha256: None,
                    exited: false,
                    exit_code: None,
                },
                channel: ChannelState::NotEstablished,
                at_ms: epoch_ms(),
            })
        }

        fn capture(
            &mut self,
            scope: &CaptureScope,
            _lease: &DeviceLeaseSession,
        ) -> Result<CaptureArtifact> {
            self.event("capture");
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            png.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
            png.extend_from_slice(&1_u32.to_be_bytes());
            png.extend_from_slice(&1_u32.to_be_bytes());
            png.extend_from_slice(&[0; 5]);
            fs::write(&scope.output, png).unwrap();
            let mut artifact = CaptureArtifact::from_png(scope, "fake", true)?;
            artifact.logical_width = Some(360);
            artifact.logical_height = Some(800);
            artifact.scale_milli = Some(3000);
            artifact.orientation = Some("portrait".into());
            artifact.foreground_app = Some("com.example.app".into());
            Ok(artifact)
        }

        fn collect_logs(
            &mut self,
            identity: &RunIdentity,
            _lease: &DeviceLeaseSession,
        ) -> Result<LogEvidence> {
            self.event("logs");
            Ok(LogEvidence {
                run_id: identity.run_id.clone(),
                source: "fake".into(),
                path: PathBuf::from("fake.log"),
                bytes: 0,
                truncated: false,
                pid: Some(42),
                process_start_token_sha256: None,
                assigned_to_run: true,
                unassigned_reason: None,
            })
        }

        fn stop_owned(
            &mut self,
            identity: &RunIdentity,
            _lease: &DeviceLeaseSession,
        ) -> Result<StopEvidence> {
            self.event("stop");
            Ok(StopEvidence {
                run_id: identity.run_id.clone(),
                stopped_owned_process: true,
                removed_owned_resources: Vec::new(),
                preserved_resources: vec!["fake-device".into()],
                at_ms: epoch_ms(),
            })
        }
    }

    fn request(root: &Path) -> RunRequest {
        RunRequest {
            run_id: "run-1".into(),
            project_id: "project-1".into(),
            device_id: "fake-device".into(),
            bundle_id: "com.example.app".into(),
            artifact_root: root.join("runs"),
            abi: Some("x86_64".into()),
        }
    }

    fn cell() -> MatrixCellSpec {
        MatrixCellSpec {
            cell_id: "android::smoke".into(),
            target_id: "android-emulator".into(),
            scenario_id: "smoke".into(),
            required: true,
            timeout_ms: None,
            resource_ids: vec!["device:fake-device".into()],
        }
    }

    #[test]
    fn mobile_cell_runs_evidence_lifecycle_and_releases_owned_run() {
        let root = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let runner = FakeMobileRunner {
            events: events.clone(),
            fail_launch: false,
            unverified_process: false,
            evidence: EvidenceLog::new(),
        };
        let lease = DeviceLeaseSession::acquire(root.path(), "fake-device").unwrap();
        let mut adapter =
            MobileMatrixCellRunner::new(runner, lease, request(root.path()), root.path());
        let execution = adapter
            .run_cell(&cell(), Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(execution.status, MatrixCellState::Passed);
        assert_eq!(execution.artifact_ids.len(), 1);
        assert!(execution.artifact_ids[0].starts_with("png-"));
        assert_eq!(execution.artifact_ids[0].len(), 68);
        adapter
            .cleanup_cell(&cell(), Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec!["prepare", "launch", "capture", "logs", "stop"]
        );
        let context = adapter.context().unwrap();
        let evidence = context.mobile_evidence.unwrap();
        assert_eq!(evidence["lease_released"], true);
        assert_eq!(evidence["run_id_bound"], true);
        assert_eq!(evidence["run_identity"]["run_id"], "run-1");
        assert_eq!(evidence["run_identity"]["device_id"], "fake-device");
        assert!(evidence["run_identity"]["lease_session_id"].is_string());
        assert_eq!(
            evidence["run_identity"]["fencing_token_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(evidence["run_identity"].get("fencing_token").is_none());
        assert_eq!(evidence["runner"]["runner_id"], "fake-mobile");
        assert_eq!(evidence["runner"]["stable_device_id"], "fake-device");
        assert_eq!(evidence["capabilities"]["foreground_probe"], false);
        assert_eq!(evidence["event_log"]["contract_version"], 1);
        assert_eq!(evidence["native_logs"]["assigned_to_run"], true);
        assert_eq!(evidence["stop"]["run_id"], "run-1");
        assert_eq!(evidence["captures"][0]["logical_width"], 360);
        assert_eq!(evidence["captures"][0]["logical_height"], 800);
        assert_eq!(evidence["captures"][0]["scale_milli"], 3000);
        assert_eq!(evidence["captures"][0]["orientation"], "portrait");
        assert_eq!(evidence["captures"][0]["system_ui"], true);
        assert_eq!(evidence["captures"][0]["foreground_app"], "com.example.app");
        assert_eq!(evidence["cleanup_errors"].as_array().unwrap().len(), 0);
        assert!(!root.path().join("captures/android__smoke.png").exists());
    }

    #[test]
    fn launch_failure_still_stops_the_prepared_run() {
        let root = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let runner = FakeMobileRunner {
            events: events.clone(),
            fail_launch: true,
            unverified_process: false,
            evidence: EvidenceLog::new(),
        };
        let lease = DeviceLeaseSession::acquire(root.path(), "fake-device-failure").unwrap();
        let mut request = request(root.path());
        request.device_id = "fake-device-failure".into();
        let mut adapter = MobileMatrixCellRunner::new(runner, lease, request, root.path());
        assert!(
            adapter
                .run_cell(&cell(), Instant::now() + std::time::Duration::from_secs(1))
                .is_err()
        );
        adapter
            .cleanup_cell(&cell(), Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(*events.lock().unwrap(), vec!["prepare", "launch", "stop"]);
    }

    #[test]
    fn missing_process_identity_is_inconclusive_with_capture_preserved() {
        let root = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let runner = FakeMobileRunner {
            events,
            fail_launch: false,
            unverified_process: true,
            evidence: EvidenceLog::new(),
        };
        let lease = DeviceLeaseSession::acquire(root.path(), "fake-device-unknown").unwrap();
        let mut request = request(root.path());
        request.device_id = "fake-device-unknown".into();
        let mut adapter = MobileMatrixCellRunner::new(runner, lease, request, root.path());
        let execution = adapter
            .run_cell(&cell(), Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(execution.status, MatrixCellState::Inconclusive);
        assert_eq!(
            execution.error.as_ref().map(|error| error.code.as_str()),
            Some("mobile_process_identity_unavailable")
        );
        assert_eq!(execution.artifact_ids.len(), 1);
        assert!(execution.artifact_ids[0].starts_with("png-"));
        assert_eq!(execution.artifact_ids[0].len(), 68);
        adapter.cleanup_cell(&cell(), Instant::now()).unwrap();
    }
}
