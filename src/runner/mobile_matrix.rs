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
use super::mobile::{CaptureScope, MobileRunner, PreparedRun, RunRequest};
use anyhow::{Result, bail};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

pub struct MobileMatrixCellRunner<R> {
    runner: R,
    lease: Option<DeviceLeaseSession>,
    request: RunRequest,
    artifact_root: PathBuf,
    prepared: Option<PreparedRun>,
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
            prepared: None,
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
        self.prepared = Some(prepared_seed);
        let prepared = {
            let lease = self.lease.as_ref().ok_or_else(|| {
                anyhow::anyhow!("mobile matrix cell lease has already been released")
            })?;
            self.runner.prepare(&self.request, lease)?
        };
        self.prepared = Some(prepared.clone());

        Self::check_deadline(deadline, "launch")?;
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mobile matrix cell lease has already been released"))?;
        let launch = self.runner.launch(&prepared, lease)?;
        if launch.process.exited {
            return Ok(MatrixCellExecution {
                status: MatrixCellState::Failed,
                error: Some(super::matrix::MatrixCellError {
                    code: "mobile_process_exited_during_launch".into(),
                    message: "mobile process exited during runner launch".into(),
                }),
                artifact_ids: Vec::new(),
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
        let capture = self.runner.capture(&scope, lease)?;

        Self::check_deadline(deadline, "native logs")?;
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mobile matrix cell lease has already been released"))?;
        let logs = self
            .runner
            .collect_logs(&prepared.identity, lease)
            .map_err(|error| anyhow::anyhow!("collecting mobile native logs: {error:#}"))?;

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
        })
    }

    fn cleanup_cell(&mut self, _cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
        let prepared = self.prepared.take();
        let stop_result = match (prepared.as_ref(), self.lease.as_ref()) {
            (Some(prepared), Some(lease)) => self
                .runner
                .stop_owned(&prepared.identity, lease)
                .map(|_| ())
                .map_err(|error| anyhow::anyhow!("stopping mobile run: {error:#}")),
            (Some(_), None) => Err(anyhow::anyhow!(
                "mobile run was prepared after its lease was released"
            )),
            (None, _) => Ok(()),
        };
        let release_result = self
            .lease
            .take()
            .map(|lease| lease.release().map_err(anyhow::Error::new));

        let mut errors = Vec::new();
        if let Err(error) = stop_result {
            errors.push(error.to_string());
        }
        if let Some(Err(error)) = release_result {
            errors.push(format!("releasing mobile device lease: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("{}", errors.join("; "))
        }
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
        CaptureArtifact, ChannelState, LaunchEvidence, LogEvidence, ProcessEvidence, RunIdentity,
        RunnerCapabilities, RunnerInfo, StopEvidence,
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
            Ok(CaptureArtifact {
                artifact_id: "png-fake".into(),
                path: scope.output.clone(),
                provider: "fake".into(),
                bytes: 1,
                sha256: "fake".into(),
                width: 1,
                height: 1,
                orientation: None,
                system_ui: true,
                foreground_app: None,
                run_id: scope.identity.run_id.clone(),
            })
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
        };
        let lease = DeviceLeaseSession::acquire(root.path(), "fake-device").unwrap();
        let mut adapter =
            MobileMatrixCellRunner::new(runner, lease, request(root.path()), root.path());
        let execution = adapter
            .run_cell(&cell(), Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(execution.status, MatrixCellState::Passed);
        assert_eq!(execution.artifact_ids, vec!["png-fake"]);
        adapter
            .cleanup_cell(&cell(), Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec!["prepare", "launch", "capture", "logs", "stop"]
        );
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
        assert_eq!(execution.artifact_ids, vec!["png-fake"]);
        adapter.cleanup_cell(&cell(), Instant::now()).unwrap();
    }
}
