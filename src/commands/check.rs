//! Execute one schema-v1 scenario against an isolated desktop preview.

use anyhow::{Context, Result, bail};
use clap::Args;
use gpui_dev_protocol::ARTIFACT_CHUNK_BYTES;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::live::PreviewBuildOutputs;
use super::run::{Project, bundle_id_of, bundle_id_of_android};
use crate::device;
use crate::devserver::OwnedChild;
use crate::devserver::actions::Action;
use crate::devserver::control::{self, Command as ControlCommand, Registration};
use crate::devserver::events::{Event, Kind, Page};
use crate::runner::android::AndroidRunner;
use crate::runner::build_inputs::{
    DesktopBuildPlan, FrozenCheckInputs, android_build_key, android_preview_cache_policy,
    compiler_tool_cache_disabled_reason, desktop_build_key, desktop_build_plan,
    frozen_check_inputs, ios_build_key, prepare_android_signing_snapshot,
};
use crate::runner::ios::IosSimulatorRunner;
use crate::runner::lease::{DeviceLeaseDelegation, DeviceLeaseSession};
use crate::runner::matrix::{
    MatrixCellError, MatrixCellSpec, MatrixCellState, MatrixReport, MatrixStatus,
};
use crate::runner::matrix_admission::{
    MatrixAdmissionContext, MatrixDeviceAvailability, MatrixFile, MatrixPlatform, load_and_admit,
    probe_local_toolchain,
};
use crate::runner::matrix_executor::{
    MatrixCellExecution, MatrixCellRunner, execute_admitted_matrix_parallel,
};
use crate::runner::matrix_resources::MatrixResourcePool;
use crate::runner::mobile::{
    CaptureArtifact, CaptureScope, LogEvidence, MobileRunner, RunIdentity, RunRequest, StopEvidence,
};
use crate::runner::output_layout::{BuildOutputLayout, BuildPlatform};
use crate::scenario::baseline::{
    BaselineComparison, BaselineKey, BaselineLoad, DiffPng, MAX_IMAGE_BYTES, compare_png, diff_png,
    load_baseline,
};
use crate::scenario::executor::{
    ActionResult, CaptureEvidence, CheckContext, CheckReport, DiffEvidence, DriverError,
    DriverErrorKind, Observation, ScenarioRunner, ScreenshotEvidence, SemanticNode,
};
use crate::scenario::{self, ScenarioDefinition, ScenarioFile, ScenarioStep, Selector};

const PREVIEW_START_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const EVENT_POLL_TIMEOUT_MS: u64 = 1_000;
const QUERY_LIMIT: u32 = 200;

#[derive(Args)]
pub struct CheckArgs {
    /// Scenario id to execute from the scenario file
    #[arg(long)]
    pub scenario: Option<String>,
    /// Scenario file, relative to the current project by default
    #[arg(long, default_value = "gpui.scenarios.toml")]
    pub file: PathBuf,
    /// Matrix configuration; when present, expand and execute target×scenario cells.
    #[arg(long)]
    pub matrix: Option<PathBuf>,
    /// Check target for a single-scenario check; matrix targets come from the matrix file
    #[arg(long, default_value = "desktop")]
    pub target: String,
    /// Emit the structured check report as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn handle_check(args: CheckArgs) -> Result<()> {
    let project = Project::load(None)?;
    if let Some(matrix) = args.matrix {
        if args.scenario.is_some() {
            bail!("--scenario cannot be combined with --matrix");
        }
        return handle_matrix_check(&project, args.file, matrix, args.json);
    }
    let scenario_id = args
        .scenario
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--scenario is required unless --matrix is provided"))?;
    if !matches!(
        args.target.to_ascii_lowercase().as_str(),
        "desktop" | "macos"
    ) {
        bail!("the S04 check runner currently supports the desktop target only");
    }
    let file = resolve_project_path(&project.root, args.file);
    let validation = scenario::validate_file(&file, &project.root)?;
    if !validation.valid {
        bail!("scenario validation failed; check was not launched");
    }
    prepare_cargo_lock(&project.root)?;
    let frozen = prepare_frozen_desktop_check(&project.root, &file, scenario_id)?;
    let mut runner = DesktopCheckRunner::launch_until(
        &frozen.plan.snapshot.root,
        &project.root,
        &frozen.scenario_file,
        &frozen.scenario,
        frozen.fixture_hash.clone(),
        &args.target,
        CheckLaunchOptions {
            device: None,
            deadline: Instant::now() + PREVIEW_START_TIMEOUT,
            session_key: format!("single-{}-{}", std::process::id(), epoch_ms()),
            device_lease_delegation: None,
            snapshot_hash: Some(frozen.plan.snapshot.input_hash.clone()),
            build_key: Some(frozen.plan.key.key_hash().to_owned()),
            build_outputs: Some(preview_outputs_from_layout(&frozen.plan.layout)),
            android_abis: None,
        },
    )?;
    let report =
        crate::scenario::executor::execute(&mut runner, &frozen.scenario, frozen.fixture_hash);
    print_report(&report, args.json)?;
    if report.status == crate::scenario::executor::CheckStatus::Passed {
        Ok(())
    } else {
        bail!("scenario check ended with {:?}", report.status)
    }
}

fn handle_matrix_check(
    project: &Project,
    scenario_file: PathBuf,
    matrix_file: PathBuf,
    json_output: bool,
) -> Result<()> {
    prepare_cargo_lock(&project.root)?;
    let scenario_source_path = resolve_project_path(&project.root, scenario_file);
    let matrix_source_path = resolve_project_path(&project.root, matrix_file);
    let frozen =
        prepare_frozen_matrix_check(&project.root, &scenario_source_path, &matrix_source_path)?;
    let runtime_root = frozen.inputs.snapshot.root.clone();
    let snapshot_hash = frozen.inputs.snapshot.input_hash.clone();
    let scenario_path = frozen.scenario_file;
    let matrix_path = frozen.matrix_file;
    let validation = scenario::validate_file(&scenario_path, &runtime_root)?;
    if !validation.valid {
        bail!("scenario validation failed; matrix was not launched");
    }
    let scenario_source = fs::read_to_string(&scenario_path)
        .with_context(|| format!("reading scenario file {}", scenario_path.display()))?;
    let scenarios: ScenarioFile =
        toml::from_str(&scenario_source).context("parsing scenario file")?;
    let matrix_source = fs::read_to_string(&matrix_path)
        .with_context(|| format!("reading matrix file {}", matrix_path.display()))?;
    let matrix: MatrixFile = toml::from_str(&matrix_source).context("parsing matrix file")?;
    let context = matrix_admission_context(project, &matrix, &runtime_root)?;
    let mut admission = load_and_admit(&matrix_path, &scenario_path, &runtime_root, &context)?;

    let target_platforms = matrix
        .targets
        .iter()
        .map(|target| {
            let platform = MatrixPlatform::parse(&target.platform)
                .ok_or_else(|| anyhow::anyhow!("unknown matrix platform '{}'", target.platform))?;
            Ok((target.id.clone(), platform))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let target_devices = matrix
        .targets
        .iter()
        .map(|target| (target.id.clone(), target.device.clone()))
        .collect::<BTreeMap<_, _>>();
    let target_abis = matrix
        .targets
        .iter()
        .map(|target| (target.id.clone(), target.abi.clone()))
        .collect::<BTreeMap<_, _>>();
    let ready_target_ids = admission
        .ready_cells()
        .map(|cell| cell.target_id.clone())
        .collect::<BTreeSet<_>>();
    let target_builds = ready_target_ids
        .iter()
        .map(|target_id| {
            let platform = target_platforms
                .get(target_id)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("matrix target '{target_id}' disappeared"))?;
            let abi = target_abis.get(target_id).cloned().flatten();
            let build =
                matrix_target_build(&project.root, &runtime_root, platform, abi.as_deref())?;
            Ok((target_id.clone(), build))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for cell in &mut admission.plan.cells {
        if let Some(build) = target_builds.get(&cell.target_id) {
            let resource_id = format!("build:{}", build.key_hash);
            if !cell.resource_ids.contains(&resource_id) {
                cell.resource_ids.push(resource_id);
            }
        }
    }
    for admitted in &mut admission.cells {
        if let Some(build) = target_builds.get(&admitted.cell.target_id) {
            let resource_id = format!("build:{}", build.key_hash);
            if !admitted.cell.resource_ids.contains(&resource_id) {
                admitted.cell.resource_ids.push(resource_id);
            }
        }
    }
    admission.plan.validate()?;
    let scenario_by_id = scenarios
        .scenarios
        .iter()
        .map(|scenario| (scenario.id.clone(), scenario.clone()))
        .collect::<BTreeMap<_, _>>();
    let fixture_hashes = validation
        .scenarios
        .iter()
        .filter_map(|entry| {
            entry
                .fixture_hash
                .clone()
                .map(|hash| (entry.id.clone(), hash))
        })
        .collect::<BTreeMap<_, _>>();
    let project_root = project.root.clone();
    let runtime_root_for_factory = runtime_root.clone();
    let snapshot_hash_for_factory = snapshot_hash.clone();
    let scenario_path = scenario_path.clone();
    let factory = move |cell: &MatrixCellSpec| -> Result<MatrixCheckRunner> {
        let platform = target_platforms
            .get(&cell.target_id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("matrix target '{}' disappeared", cell.target_id))?;
        let scenario = scenario_by_id
            .get(&cell.scenario_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("matrix scenario '{}' disappeared", cell.scenario_id))?;
        let fixture_hash = fixture_hashes.get(&cell.scenario_id).cloned();
        let target_build = target_builds.get(&cell.target_id).cloned().ok_or_else(|| {
            anyhow::anyhow!("matrix target '{}' has no build outputs", cell.target_id)
        })?;
        Ok(MatrixCheckRunner::Control(Box::new(
            ControlMatrixCheckRunner {
                project_root: project_root.clone(),
                runtime_root: runtime_root_for_factory.clone(),
                scenario_file: scenario_path.clone(),
                scenario,
                fixture_hash,
                snapshot_hash: snapshot_hash_for_factory.clone(),
                build_key: target_build.key_hash,
                build_outputs: target_build.outputs,
                target: platform.label().into(),
                device: target_devices.get(&cell.target_id).cloned().flatten(),
                platform,
                abi: target_abis.get(&cell.target_id).cloned().flatten(),
            },
        )))
    };
    let report = execute_admitted_matrix_parallel(
        &admission,
        factory,
        MatrixResourcePool::new(),
        Instant::now(),
    )?;
    print_matrix_report(&report, json_output)?;
    if report.status == MatrixStatus::Passed {
        Ok(())
    } else {
        bail!("matrix check ended with {:?}", report.status)
    }
}

fn matrix_admission_context(
    project: &Project,
    matrix: &MatrixFile,
    source_root: &Path,
) -> Result<MatrixAdmissionContext> {
    let host_os = std::env::consts::OS;
    let mut context = MatrixAdmissionContext::for_local(host_os, project.targets.clone());
    let local = context
        .runners
        .get_mut("local")
        .ok_or_else(|| anyhow::anyhow!("local matrix runner was not initialized"))?;

    if cfg!(target_os = "macos") {
        for device in device::ios::simulators()? {
            local.devices.insert(
                device.id.clone(),
                MatrixDeviceAvailability {
                    available: device.launchable(),
                    arch: device.arch.clone(),
                },
            );
        }
    }
    for device in device::android::all() {
        let available = device.launchable() && device.serial().is_some();
        let availability = MatrixDeviceAvailability {
            available,
            arch: device.arch.clone(),
        };
        local
            .devices
            .insert(device.id.clone(), availability.clone());
        if let Some(serial) = device.serial() {
            local.devices.insert(serial.to_owned(), availability);
        }
    }

    let mut probed = BTreeSet::new();
    for target in &matrix.targets {
        if target.runner != "local" {
            continue;
        }
        let platform = MatrixPlatform::parse(&target.platform)
            .ok_or_else(|| anyhow::anyhow!("unknown matrix platform '{}'", target.platform))?;
        if !probed.insert(platform) {
            continue;
        }
        let report = probe_local_toolchain(platform, target.abi.as_deref(), source_root, host_os)?;
        context.toolchains.insert(platform, report);
    }
    Ok(context)
}

#[derive(Clone)]
struct MatrixTargetBuild {
    key_hash: String,
    outputs: PreviewBuildOutputs,
}

fn matrix_target_build(
    source_root: &Path,
    snapshot_root: &Path,
    platform: MatrixPlatform,
    abi: Option<&str>,
) -> Result<MatrixTargetBuild> {
    let source_root = fs::canonicalize(source_root).with_context(|| {
        format!(
            "resolving matrix build output root {}",
            source_root.display()
        )
    })?;
    let (
        key,
        build_platform,
        cache_hit_disabled_reason,
        android_debug_keystore_hash,
        android_signing_fingerprint,
    ) = match platform {
        MatrixPlatform::Macos | MatrixPlatform::Windows | MatrixPlatform::Linux => {
            let key = desktop_build_key(snapshot_root, false)?;
            let cache_hit_disabled_reason = compiler_tool_cache_disabled_reason(
                snapshot_root,
                Some(&key.material().target_triple),
            );
            (
                key,
                BuildPlatform::Desktop,
                cache_hit_disabled_reason,
                None,
                None,
            )
        }
        MatrixPlatform::Ios => {
            let key = ios_build_key(snapshot_root, false, "aarch64-apple-ios-sim")?;
            let cache_hit_disabled_reason = compiler_tool_cache_disabled_reason(
                snapshot_root,
                Some(&key.material().target_triple),
            );
            (
                key,
                BuildPlatform::Ios,
                cache_hit_disabled_reason,
                None,
                None,
            )
        }
        MatrixPlatform::Android => {
            let abi = abi.context("Android matrix target has no ABI for BuildKey")?;
            let policy = android_preview_cache_policy(snapshot_root, false, abi)?;
            (
                android_build_key(snapshot_root, false, &[abi.to_owned()])?,
                BuildPlatform::Android,
                policy.disabled_reason,
                policy.debug_keystore_hash,
                policy.android_signing_fingerprint,
            )
        }
    };
    let layout =
        BuildOutputLayout::for_key(&source_root.join(".gpui/builds"), &key, build_platform)?;
    layout.prepare()?;
    let mut outputs = preview_outputs_from_layout(&layout);
    outputs.cache_hit_disabled_reason = cache_hit_disabled_reason;
    outputs.android_debug_keystore_hash = android_debug_keystore_hash;
    outputs.android_signing_fingerprint = android_signing_fingerprint;
    Ok(MatrixTargetBuild {
        key_hash: key.key_hash().to_owned(),
        outputs,
    })
}

fn preview_outputs_from_layout(layout: &BuildOutputLayout) -> PreviewBuildOutputs {
    PreviewBuildOutputs {
        output_root: layout.root.clone(),
        cargo_target_dir: layout.cargo_target_dir.clone(),
        build_key_hash: Some(layout.key_hash.clone()),
        cache_hit_disabled_reason: None,
        android_debug_keystore_hash: None,
        android_signing_fingerprint: None,
        jni_libs_dir: layout.android_jni_dir.clone(),
        gradle_build_dir: layout.android_gradle_build_dir.clone(),
        ios_derived_data_dir: layout.ios_derived_data_dir.clone(),
    }
}

fn print_matrix_report(report: &MatrixReport, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("matrix {}: {:?}", report.plan_id, report.status);
    for cell in &report.cells {
        let detail = cell
            .error
            .as_ref()
            .map(|error| format!(" — {}: {}", error.code, error.message))
            .unwrap_or_default();
        println!("  {:<32} {:?}{}", cell.cell_id, cell.status, detail);
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct MobileScenarioEvidence {
    cell_id: String,
    target_id: String,
    scenario_id: String,
    requirements: Vec<String>,
    fixture_hash: Option<String>,
}

impl MobileScenarioEvidence {
    fn new(
        cell_id: &str,
        target_id: &str,
        scenario_id: &str,
        requirements: &[String],
        fixture_hash: Option<&str>,
    ) -> Self {
        Self {
            cell_id: cell_id.to_owned(),
            target_id: target_id.to_owned(),
            scenario_id: scenario_id.to_owned(),
            requirements: requirements.to_vec(),
            fixture_hash: fixture_hash.map(str::to_owned),
        }
    }
}

struct MobileCapture {
    runner: Box<dyn MobileRunner + Send>,
    lease: Option<DeviceLeaseSession>,
    identity: RunIdentity,
    scenario: MobileScenarioEvidence,
    run_id_bound: bool,
    artifact_root: PathBuf,
    captures: Vec<CaptureArtifact>,
    native_logs: Option<LogEvidence>,
    stop: Option<StopEvidence>,
    cleanup_errors: Vec<String>,
}

impl MobileCapture {
    fn new(
        platform: MatrixPlatform,
        device_id: &str,
        abi: Option<&str>,
        project_root: &Path,
        scenario: MobileScenarioEvidence,
    ) -> Result<Self> {
        let lease = DeviceLeaseSession::acquire(project_root, device_id)
            .context("acquiring mobile matrix scenario lease")?;
        let bundle_id = match platform {
            MatrixPlatform::Ios => bundle_id_of(&Project::load(Some(project_root.to_path_buf()))?),
            MatrixPlatform::Android => {
                bundle_id_of_android(&Project::load(Some(project_root.to_path_buf()))?)
            }
            _ => bail!("mobile capture requires an iOS or Android platform"),
        };
        let artifact_root = project_root
            .join(".gpui")
            .join("matrix-runs")
            .join(safe_matrix_component(&scenario.cell_id));
        let run_id = format!(
            "matrix-{}-{}",
            safe_matrix_component(&scenario.cell_id),
            epoch_ms()
        );
        let request = RunRequest {
            run_id,
            project_id: project_root.to_string_lossy().into_owned(),
            device_id: device_id.to_owned(),
            bundle_id: bundle_id.clone(),
            artifact_root: artifact_root.clone(),
            abi: abi.map(str::to_owned),
        };
        let identity = request.prepare(&lease)?.identity;
        let runner: Box<dyn MobileRunner + Send> = match platform {
            MatrixPlatform::Ios => Box::new(IosSimulatorRunner::new(
                device_id,
                project_root.join(".gpui/matrix-placeholder.app"),
                bundle_id.clone(),
                artifact_root.clone(),
            )),
            MatrixPlatform::Android => Box::new(AndroidRunner::new(
                device_id,
                project_root.join(".gpui/matrix-placeholder.apk"),
                bundle_id.clone(),
                artifact_root.clone(),
            )),
            _ => unreachable!("validated mobile platform"),
        };
        Ok(Self {
            runner,
            lease: Some(lease),
            identity,
            scenario,
            run_id_bound: false,
            artifact_root,
            captures: Vec::new(),
            native_logs: None,
            stop: None,
            cleanup_errors: Vec::new(),
        })
    }

    fn delegation(&self) -> DeviceLeaseDelegation {
        self.lease
            .as_ref()
            .expect("mobile capture lease is present before preview launch")
            .delegation()
    }

    fn bind_run_id(&mut self, run_id: String) {
        self.identity.run_id = run_id;
        self.run_id_bound = true;
    }

    fn capture(&mut self, observation_id: &str, deadline: Instant) -> Result<ScreenshotEvidence> {
        if Instant::now() >= deadline {
            bail!("mobile screenshot deadline exceeded");
        }
        let output = self
            .artifact_root
            .join("captures")
            .join(format!("{}.png", safe_matrix_component(observation_id)));
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let scope = CaptureScope {
            identity: self.identity.clone(),
            output,
            attempt: 1,
            orientation: None,
            foreground_app: None,
        };
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mobile capture lease has been released"))?;
        let artifact = self.runner.capture(&scope, lease)?;
        artifact
            .verify_for_identity(&self.identity)
            .context("verifying mobile screenshot artifact")?;
        self.captures.push(artifact.clone());
        screenshot_from_mobile_artifact(&artifact, &self.artifact_root)
    }

    fn finalize(&mut self) -> Result<()> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mobile capture lease has been released"))?;
        let mut logs = self.runner.collect_logs(&self.identity, lease)?;
        if !self.run_id_bound {
            logs.assigned_to_run = false;
            logs.unassigned_reason =
                Some("preview run identity was unavailable before scenario_ready".into());
        }
        let assigned = logs.assigned_to_run;
        self.native_logs = Some(logs);
        if !assigned {
            bail!("mobile native logs could not be assigned to the active run")
        }
        Ok(())
    }

    fn finalize_and_cleanup(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Err(error) = self.finalize() {
            errors.push(format!("finalizing mobile evidence: {error:#}"));
        }
        if let Err(error) = self.cleanup() {
            errors.push(format!("cleaning up mobile evidence: {error:#}"));
        }
        errors
    }

    fn artifact_ids(&self) -> Vec<String> {
        self.captures
            .iter()
            .map(|capture| capture.artifact_id.clone())
            .collect()
    }

    fn context(&self, snapshot_hash: String, build_key: String) -> CheckContext {
        CheckContext {
            reset_generation: None,
            snapshot_hash: Some(snapshot_hash),
            build_key: Some(build_key),
            environment: None,
            uncontrolled_inputs: Vec::new(),
            mobile_evidence: Some(self.evidence()),
        }
    }

    fn evidence(&self) -> Value {
        let relative_path = |path: &Path| {
            path.strip_prefix(&self.artifact_root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        };
        json!({
            "run_id": self.identity.run_id,
            "project_id": self.identity.project_id,
            "run_id_bound": self.run_id_bound,
            "device_id": self.identity.device_id,
            "lease_session_id": self.identity.lease_session_id,
            "fencing_token_sha256": self.identity.fencing_token_sha256,
            "run_identity": self.run_id_bound.then(|| json!({
                "run_id": self.identity.run_id,
                "project_id": self.identity.project_id,
                "device_id": self.identity.device_id,
                "lease_session_id": self.identity.lease_session_id,
                "fencing_token_sha256": self.identity.fencing_token_sha256,
            })),
            "scenario": {
                "cell_id": self.scenario.cell_id,
                "target_id": self.scenario.target_id,
                "scenario_id": self.scenario.scenario_id,
                "requirements": self.scenario.requirements,
                "fixture_hash": self.scenario.fixture_hash,
            },
            "runner": self.runner.describe(),
            "capabilities": self.runner.capabilities(),
            "event_log": self.runner.evidence_log(),
            "captures": self.captures.iter().map(|capture| json!({
                "artifact_id": capture.artifact_id,
                "provider": capture.provider,
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
                "path": relative_path(&capture.path),
                "manifest_path": relative_path(&capture.manifest_path),
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
        })
    }

    fn cleanup(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        if let Some(lease) = self.lease.as_ref() {
            match self.runner.stop_owned(&self.identity, lease) {
                Ok(stop) => self.stop = Some(stop),
                Err(error) => errors.push(format!("stop_owned: {error:#}")),
            }
        }
        if let Some(lease) = self.lease.take()
            && let Err(error) = lease.release()
        {
            errors.push(format!("lease_release: {error}"));
        }
        self.cleanup_errors.extend(errors.iter().cloned());
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        Ok(())
    }
}

impl Drop for MobileCapture {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn screenshot_from_mobile_artifact(
    artifact: &CaptureArtifact,
    artifact_root: &Path,
) -> Result<ScreenshotEvidence> {
    let manifest_path = artifact
        .manifest_path
        .strip_prefix(artifact_root)
        .with_context(|| {
            format!(
                "mobile screenshot manifest escaped artifact root: {}",
                artifact.manifest_path.display()
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(ScreenshotEvidence {
        artifact_id: Some(artifact.artifact_id.clone()),
        manifest_path: Some(manifest_path),
        baseline_id: None,
        baseline_key: None,
        diff: None,
        scope: Some("device".into()),
        provider: Some(artifact.provider.clone()),
        pixel_width: Some(artifact.width),
        pixel_height: Some(artifact.height),
        logical_width: artifact.logical_width,
        logical_height: artifact.logical_height,
        scale_milli: artifact.scale_milli,
        comparable: false,
        matches: None,
        reason: (artifact.logical_width.is_none()
            || artifact.logical_height.is_none()
            || artifact.scale_milli.is_none())
        .then_some("mobile_environment_metadata_unavailable".into()),
    })
}

fn safe_matrix_component(value: &str) -> String {
    let mut result = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                byte as char
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() {
        result.push('_');
    }
    result.truncate(128);
    result
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

enum MatrixCheckRunner {
    Control(Box<ControlMatrixCheckRunner>),
}

struct ControlMatrixCheckRunner {
    project_root: PathBuf,
    runtime_root: PathBuf,
    scenario_file: PathBuf,
    scenario: ScenarioDefinition,
    fixture_hash: Option<String>,
    snapshot_hash: String,
    build_key: String,
    build_outputs: PreviewBuildOutputs,
    target: String,
    device: Option<String>,
    platform: MatrixPlatform,
    abi: Option<String>,
}

impl MatrixCellRunner for MatrixCheckRunner {
    fn run_cell(
        &mut self,
        cell: &MatrixCellSpec,
        deadline: Instant,
    ) -> Result<MatrixCellExecution> {
        match self {
            Self::Control(control) => {
                let ControlMatrixCheckRunner {
                    project_root,
                    runtime_root,
                    scenario_file,
                    scenario,
                    fixture_hash,
                    snapshot_hash,
                    build_key,
                    build_outputs,
                    target,
                    device,
                    platform,
                    abi,
                } = control.as_mut();
                if Instant::now() >= deadline {
                    bail!("matrix desktop cell deadline exceeded before launch");
                }
                let mut bounded_scenario = scenario.clone();
                let remaining_ms = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX)
                    .max(1);
                bounded_scenario.timeout_ms = bounded_scenario.timeout_ms.min(remaining_ms);
                let mut mobile_capture =
                    if matches!(*platform, MatrixPlatform::Ios | MatrixPlatform::Android) {
                        Some(MobileCapture::new(
                            *platform,
                            device.as_deref().ok_or_else(|| {
                                anyhow::anyhow!("mobile matrix cell has no selected device")
                            })?,
                            abi.as_deref(),
                            project_root,
                            MobileScenarioEvidence::new(
                                &cell.cell_id,
                                target,
                                &scenario.id,
                                &scenario.requires,
                                fixture_hash.as_deref(),
                            ),
                        )?)
                    } else {
                        None
                    };
                let session_key = format!(
                    "{}-{}-{}",
                    safe_matrix_component(&cell.cell_id),
                    std::process::id(),
                    epoch_ms()
                );
                let launch_result = DesktopCheckRunner::launch_until(
                    runtime_root,
                    project_root,
                    scenario_file,
                    &bounded_scenario,
                    fixture_hash.clone(),
                    target,
                    CheckLaunchOptions {
                        device: device.as_deref(),
                        deadline,
                        session_key,
                        device_lease_delegation: mobile_capture
                            .as_ref()
                            .map(MobileCapture::delegation),
                        snapshot_hash: Some(snapshot_hash.clone()),
                        build_key: Some(build_key.clone()),
                        build_outputs: Some(build_outputs.clone()),
                        android_abis: abi.clone(),
                    },
                );
                let mut runner = match launch_result {
                    Ok(runner) => runner,
                    Err(error) => {
                        if let Some(mobile_capture) = mobile_capture.as_mut() {
                            let evidence_errors = mobile_capture.finalize_and_cleanup();
                            let mut message = format!("mobile preview launch failed: {error:#}");
                            if !evidence_errors.is_empty() {
                                message.push_str("; ");
                                message.push_str(&evidence_errors.join("; "));
                            }
                            return Ok(MatrixCellExecution {
                                status: MatrixCellState::Failed,
                                error: Some(MatrixCellError {
                                    code: "mobile_preview_launch_failed".into(),
                                    message,
                                }),
                                artifact_ids: mobile_capture.artifact_ids(),
                                context: Some(
                                    mobile_capture
                                        .context(snapshot_hash.clone(), build_key.clone()),
                                ),
                                check_report: None,
                            });
                        }
                        return Err(error);
                    }
                };
                if let Some(mobile_capture) = mobile_capture.as_mut() {
                    let run_id = runner
                        .run_id()
                        .ok_or_else(|| anyhow::anyhow!("mobile preview did not publish a run id"))?
                        .to_owned();
                    mobile_capture.bind_run_id(run_id);
                }
                runner.mobile_capture = mobile_capture.take();
                let report = crate::scenario::executor::execute(
                    &mut runner,
                    &bounded_scenario,
                    fixture_hash.clone(),
                );
                let check_report = report.clone();
                let status = match report.status {
                    crate::scenario::executor::CheckStatus::Passed => MatrixCellState::Passed,
                    crate::scenario::executor::CheckStatus::Failed => MatrixCellState::Failed,
                    crate::scenario::executor::CheckStatus::Inconclusive => {
                        MatrixCellState::Inconclusive
                    }
                    crate::scenario::executor::CheckStatus::Unavailable => {
                        MatrixCellState::Unavailable
                    }
                    crate::scenario::executor::CheckStatus::Cancelled => MatrixCellState::Cancelled,
                };
                let error = report.primary_error.map(|error| MatrixCellError {
                    code: error.code,
                    message: error.message,
                });
                let artifact_ids = report
                    .steps
                    .iter()
                    .filter_map(|step| step.capture.as_ref())
                    .filter_map(|capture| capture.artifact_id.clone())
                    .collect();
                Ok(MatrixCellExecution {
                    status,
                    error,
                    artifact_ids,
                    context: report.context,
                    check_report: Some(check_report),
                })
            }
        }
    }

    fn cleanup_cell(&mut self, _cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
        Ok(())
    }
}

fn resolve_project_path(root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn prepare_cargo_lock(project_root: &Path) -> Result<()> {
    if project_root.join("Cargo.lock").is_file() {
        return Ok(());
    }
    let status = Command::new("cargo")
        .current_dir(project_root)
        .args(["generate-lockfile"])
        .status()
        .context("generating Cargo.lock before starting the desktop check")?;
    if !status.success() {
        bail!("cargo generate-lockfile failed before starting the desktop check");
    }
    Ok(())
}

struct FrozenDesktopCheck {
    plan: DesktopBuildPlan,
    scenario_file: PathBuf,
    scenario: ScenarioDefinition,
    fixture_hash: Option<String>,
}

fn prepare_frozen_desktop_check(
    project_root: &Path,
    scenario_file: &Path,
    scenario_id: &str,
) -> Result<FrozenDesktopCheck> {
    let project_root = fs::canonicalize(project_root)
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    let scenario_file = fs::canonicalize(scenario_file)
        .with_context(|| format!("resolving scenario file {}", scenario_file.display()))?;
    let relative_scenario = scenario_file
        .strip_prefix(&project_root)
        .with_context(|| {
            format!(
                "scenario file {} is outside project root {}",
                scenario_file.display(),
                project_root.display()
            )
        })?
        .to_owned();
    let plan = desktop_build_plan(&project_root, false)?;
    if let Some(reason) = &plan.cache_hit_disabled_reason {
        bail!("strict frozen desktop check is unavailable: {reason}");
    }

    let snapshot_file = plan.snapshot.root.join(&relative_scenario);
    if !snapshot_file.is_file() {
        bail!(
            "frozen snapshot does not contain scenario file {}",
            relative_scenario.display()
        );
    }
    let validation = scenario::validate_file(&snapshot_file, &plan.snapshot.root)?;
    if !validation.valid {
        bail!("frozen scenario validation failed; check was not launched");
    }
    let source = fs::read_to_string(&snapshot_file)
        .with_context(|| format!("reading frozen scenario file {}", snapshot_file.display()))?;
    let model: ScenarioFile = toml::from_str(&source).context("parsing frozen scenario file")?;
    let scenario = model
        .scenarios
        .into_iter()
        .find(|scenario| scenario.id == scenario_id)
        .with_context(|| format!("scenario {scenario_id} was not found in frozen inputs"))?;
    let fixture_hash = validation
        .scenarios
        .iter()
        .find(|entry| entry.id == scenario.id)
        .and_then(|entry| entry.fixture_hash.clone());
    Ok(FrozenDesktopCheck {
        plan,
        scenario_file: snapshot_file,
        scenario,
        fixture_hash,
    })
}

struct FrozenMatrixCheck {
    inputs: FrozenCheckInputs,
    scenario_file: PathBuf,
    matrix_file: PathBuf,
}

fn prepare_frozen_matrix_check(
    project_root: &Path,
    scenario_file: &Path,
    matrix_file: &Path,
) -> Result<FrozenMatrixCheck> {
    let project_root = fs::canonicalize(project_root)
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    let scenario_file = fs::canonicalize(scenario_file)
        .with_context(|| format!("resolving scenario file {}", scenario_file.display()))?;
    let matrix_file = fs::canonicalize(matrix_file)
        .with_context(|| format!("resolving matrix file {}", matrix_file.display()))?;
    let relative_scenario = scenario_file
        .strip_prefix(&project_root)
        .with_context(|| {
            format!(
                "scenario file {} is outside project root {}",
                scenario_file.display(),
                project_root.display()
            )
        })?
        .to_owned();
    let relative_matrix = matrix_file
        .strip_prefix(&project_root)
        .with_context(|| {
            format!(
                "matrix file {} is outside project root {}",
                matrix_file.display(),
                project_root.display()
            )
        })?
        .to_owned();
    let inputs = frozen_check_inputs(&project_root)?;
    prepare_android_signing_snapshot(&project_root, &inputs.snapshot.root)?;
    if let Some(reason) = &inputs.cache_hit_disabled_reason {
        bail!("strict frozen matrix check is unavailable: {reason}");
    }
    let snapshot_scenario = inputs.snapshot.root.join(&relative_scenario);
    if !snapshot_scenario.is_file() {
        bail!(
            "frozen snapshot does not contain scenario file {}",
            relative_scenario.display()
        );
    }
    let snapshot_matrix = inputs.snapshot.root.join(&relative_matrix);
    if !snapshot_matrix.is_file() {
        bail!(
            "frozen snapshot does not contain matrix file {}",
            relative_matrix.display()
        );
    }
    Ok(FrozenMatrixCheck {
        inputs,
        scenario_file: snapshot_scenario,
        matrix_file: snapshot_matrix,
    })
}

fn print_report(report: &CheckReport, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!(
            "scenario `{}`: {:?} ({} step(s))",
            report.scenario_id,
            report.status,
            report.steps.len()
        );
        for step in &report.steps {
            println!("  {:<24} {:?}", step.id, step.status);
            if let Some(error) = &step.error {
                println!("    {}: {}", error.code, error.message);
            }
        }
        if let Some(error) = &report.primary_error {
            println!("error: {}: {}", error.code, error.message);
        }
        if !report.cleanup.succeeded
            && let Some(error) = &report.cleanup.error
        {
            println!("cleanup: {}: {}", error.code, error.message);
        }
    }
    Ok(())
}

struct DesktopCheckRunner {
    child: OwnedChild,
    project_root: PathBuf,
    registration: Registration,
    event_cursor: u64,
    scenario_id: String,
    component: String,
    target: String,
    fixture_hash: Option<String>,
    viewport_width: u32,
    viewport_height: u32,
    requested_theme: String,
    requested_locale: String,
    ready_fixture_hash: Option<String>,
    ready_run_id: Option<String>,
    ready_environment: Value,
    ready_reset_generation: Option<u64>,
    ready_uncontrolled_inputs: Vec<String>,
    default_requirements: Vec<String>,
    issue_floor: u64,
    mobile_capture: Option<MobileCapture>,
    snapshot_hash: Option<String>,
    build_key: Option<String>,
}

struct CheckLaunchOptions<'a> {
    device: Option<&'a str>,
    deadline: Instant,
    session_key: String,
    device_lease_delegation: Option<DeviceLeaseDelegation>,
    snapshot_hash: Option<String>,
    build_key: Option<String>,
    build_outputs: Option<PreviewBuildOutputs>,
    android_abis: Option<String>,
}

impl DesktopCheckRunner {
    fn launch_until(
        runtime_root: &Path,
        report_root: &Path,
        scenario_file: &Path,
        scenario: &ScenarioDefinition,
        fixture_hash: Option<String>,
        target: &str,
        options: CheckLaunchOptions<'_>,
    ) -> Result<Self> {
        let CheckLaunchOptions {
            device,
            deadline,
            session_key,
            device_lease_delegation,
            snapshot_hash,
            build_key,
            build_outputs,
            android_abis,
        } = options;
        let executable = std::env::current_exe().context("locating the gpui executable")?;
        let scenario_file = scenario_file
            .strip_prefix(runtime_root)
            .unwrap_or(scenario_file);
        let mut child = Command::new(executable);
        child.current_dir(runtime_root).args([
            "preview",
            &scenario.component,
            "--scenario",
            &scenario.id,
            "--file",
            &scenario_file.to_string_lossy(),
            "--target",
            target,
        ]);
        if let Some(device) = device {
            child.args(["--device", device]);
        }
        if let Some(delegation) = device_lease_delegation {
            child.env(
                "GPUI_PREVIEW_DEVICE_LEASE_DELEGATION",
                serde_json::to_string(&delegation)
                    .context("serializing the delegated preview device lease")?,
            );
        }
        child.env("GPUI_PREVIEW_SESSION_KEY", &session_key);
        if let Some(abis) = android_abis {
            child.env("GPUI_ANDROID_ABIS", abis);
        }
        if let Some(outputs) = &build_outputs {
            child.env("GPUI_PREVIEW_BUILD_OUTPUT_ROOT", &outputs.output_root);
            child.env("GPUI_PREVIEW_CARGO_TARGET_DIR", &outputs.cargo_target_dir);
            if let Some(key_hash) = &outputs.build_key_hash {
                child.env("GPUI_PREVIEW_BUILD_KEY_HASH", key_hash);
            }
            if let Some(reason) = &outputs.cache_hit_disabled_reason {
                child.env("GPUI_PREVIEW_CACHE_HIT_DISABLED_REASON", reason);
            }
            if let Some(hash) = &outputs.android_debug_keystore_hash {
                child.env("GPUI_PREVIEW_ANDROID_DEBUG_KEYSTORE_HASH", hash);
            }
            if let Some(fingerprint) = &outputs.android_signing_fingerprint {
                child.env("GPUI_PREVIEW_ANDROID_SIGNING_FINGERPRINT", fingerprint);
            }
            if let Some(path) = &outputs.jni_libs_dir {
                child.env("GPUI_PREVIEW_JNI_LIBS_DIR", path);
            }
            if let Some(path) = &outputs.gradle_build_dir {
                child.env("GPUI_PREVIEW_GRADLE_BUILD_DIR", path);
            }
            if let Some(path) = &outputs.ios_derived_data_dir {
                child.env("GPUI_PREVIEW_IOS_DERIVED_DATA_DIR", path);
            }
        }
        let mut child = OwnedChild::spawn_with_stdio(
            &mut child,
            Stdio::null(),
            Stdio::null(),
            Stdio::inherit(),
        )
        .context("starting isolated desktop preview")?;
        let registration =
            match wait_for_registration(runtime_root, &scenario.id, &session_key, deadline) {
                Ok(registration) => registration,
                Err(error) => {
                    let _ = child.terminate();
                    return Err(error);
                }
            };
        let mut runner = Self {
            child,
            project_root: report_root.to_owned(),
            registration,
            event_cursor: 0,
            scenario_id: scenario.id.clone(),
            component: scenario.component.clone(),
            target: target.to_ascii_lowercase(),
            fixture_hash,
            viewport_width: scenario.viewport.width,
            viewport_height: scenario.viewport.height,
            requested_theme: scenario.theme.clone(),
            requested_locale: scenario.locale.clone(),
            ready_fixture_hash: None,
            ready_run_id: None,
            ready_environment: Value::Object(serde_json::Map::new()),
            ready_reset_generation: None,
            ready_uncontrolled_inputs: Vec::new(),
            default_requirements: observation_requirements(&scenario.requires),
            issue_floor: 0,
            mobile_capture: None,
            snapshot_hash,
            build_key,
        };
        if let Err(error) = runner.wait_for_ready(deadline) {
            let _ = runner.child.terminate();
            return Err(error);
        }
        Ok(runner)
    }

    fn wait_for_ready(&mut self, deadline: Instant) -> Result<()> {
        loop {
            if Instant::now() >= deadline {
                bail!("preview did not emit scenario_ready before the check launch deadline")
            }
            let page = self.read_events(deadline).map_err(driver_to_anyhow)?;
            for event in page {
                if event.kind == Kind::ScenarioReady
                    && event.data["scenario_id"].as_str() == Some(self.scenario_id.as_str())
                    && event.data["component"].as_str() == Some(self.component.as_str())
                {
                    self.record_ready_environment(&event);
                    return Ok(());
                }
                if matches!(event.kind, Kind::AppLaunchFailed | Kind::AppExited) {
                    bail!("preview exited before scenario_ready: {}", event.data)
                }
            }
        }
    }

    fn wait_for_reset(&mut self, request_id: &str, deadline: Instant) -> Result<(), DriverError> {
        let mut accepted = false;
        let mut ready = false;
        loop {
            if Instant::now() >= deadline {
                return Err(DriverError::timeout(
                    "reset_timeout",
                    "scenario reset did not reach the runtime before the deadline",
                ));
            }
            for event in self.read_events(deadline)? {
                match event.kind {
                    Kind::ScenarioResetResult
                        if event.data["request_id"].as_str() == Some(request_id) =>
                    {
                        if event.data["accepted"] != true {
                            return Err(DriverError::failed(
                                "reset_rejected",
                                event.data["reason"]
                                    .as_str()
                                    .unwrap_or("preview rejected scenario reset"),
                            ));
                        }
                        accepted = true;
                        if ready {
                            return Ok(());
                        }
                    }
                    Kind::ScenarioReady
                        if event.data["scenario_id"].as_str()
                            == Some(self.scenario_id.as_str()) =>
                    {
                        self.record_ready_environment(&event);
                        ready = true;
                        if accepted {
                            return Ok(());
                        }
                    }
                    Kind::AppLaunchFailed | Kind::AppExited => {
                        return Err(DriverError::unknown(
                            "preview_exited",
                            "preview exited while resetting the scenario",
                        ));
                    }
                    _ => {}
                }
            }
        }
    }

    fn read_events(&mut self, deadline: Instant) -> Result<Vec<Event>, DriverError> {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let timeout_ms = remaining.min(EVENT_POLL_TIMEOUT_MS);
        let reply = control::request(
            &self.registration,
            &control::next_request_id("check.events"),
            ControlCommand::Events {
                after: self.event_cursor,
                timeout_ms,
            },
        )
        .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
        if !reply.ok {
            return Err(reply_error(&reply, "events_failed"));
        }
        let page: Page = serde_json::from_value(reply.result.unwrap_or_default())
            .map_err(|error| DriverError::failed("invalid_events", error.to_string()))?;
        if page.gap {
            return Err(DriverError::unknown(
                "event_cursor_expired",
                "the check event cursor expired before the required event was read",
            ));
        }
        self.event_cursor = page.next_seq;
        Ok(page.events)
    }

    fn wait_operation(&self, operation_id: &str, deadline: Instant) -> Result<Value, DriverError> {
        loop {
            if Instant::now() >= deadline {
                return Err(DriverError::timeout(
                    "operation_timeout",
                    "operation did not reach a terminal state before the scenario deadline",
                ));
            }
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX)
                .min(control::MAX_WAIT_MS);
            let reply = control::request(
                &self.registration,
                &control::next_request_id("check.operation"),
                ControlCommand::OperationGet {
                    operation_id: operation_id.to_owned(),
                    wait_ms: remaining,
                },
            )
            .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
            if !reply.ok {
                return Err(reply_error(&reply, "operation_failed"));
            }
            let operation = reply.result.unwrap_or_default();
            match operation["state"].as_str().unwrap_or("unknown") {
                "succeeded" => return Ok(operation),
                "failed" => return Err(operation_error(&operation, DriverErrorKind::Failed)),
                "unavailable" => {
                    return Err(operation_error(&operation, DriverErrorKind::Unavailable));
                }
                "cancelled" => return Err(operation_error(&operation, DriverErrorKind::Cancelled)),
                "timed_out" | "superseded" => {
                    return Err(operation_error(&operation, DriverErrorKind::Timeout));
                }
                "unknown" => return Err(operation_error(&operation, DriverErrorKind::Unknown)),
                _ => {}
            }
        }
    }

    fn observe_with_requirements(
        &mut self,
        requirements: Vec<String>,
        deadline: Instant,
    ) -> Result<Observation, DriverError> {
        let control_requirements = if self.mobile_capture.is_some() {
            let mut filtered = requirements
                .iter()
                .filter(|requirement| {
                    !matches!(
                        requirement.as_str(),
                        "screenshot" | "capture.scene" | "capture.window" | "capture.device"
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            if filtered.is_empty() {
                filtered.push("ui.heartbeat".into());
            }
            filtered
        } else {
            requirements.clone()
        };
        let reply = control::request(
            &self.registration,
            &control::next_request_id("check.observe"),
            ControlCommand::Observe {
                sync: false,
                window_id: None,
                require: control_requirements,
                deadline_ms: remaining_ms(deadline),
            },
        )
        .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
        if !reply.ok {
            return Err(reply_error(&reply, "observe_failed"));
        }
        let submitted = reply.result.unwrap_or_default();
        let operation_id = submitted["operation_id"]
            .as_str()
            .ok_or_else(|| DriverError::failed("invalid_observe", "observe has no operation_id"))?;
        let operation = self.wait_operation(operation_id, deadline)?;
        let result = operation["result"].clone();
        self.observation_from_result(result, &requirements, deadline)
    }

    fn observation_from_result(
        &mut self,
        result: Value,
        requirements: &[String],
        deadline: Instant,
    ) -> Result<Observation, DriverError> {
        let observation_id = result["observation_id"]
            .as_str()
            .ok_or_else(|| DriverError::failed("invalid_observation", "observation has no id"))?
            .to_owned();
        let nodes = if requires_semantics(requirements) {
            Some(self.query_nodes(&observation_id)?)
        } else {
            None
        };
        let diagnostics = control::request(
            &self.registration,
            &control::next_request_id("check.diagnostics"),
            ControlCommand::Diagnostics,
        )
        .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
        let (runtime_errors, log_seq) = if diagnostics.ok {
            let value = diagnostics.result.unwrap_or_default();
            let seq = value["seq"].as_u64();
            let issues = value["runtime_issues"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|issue| {
                    issue["seq"]
                        .as_u64()
                        .is_some_and(|seq| seq > self.issue_floor)
                })
                .collect();
            (Some(issues), seq)
        } else {
            (None, None)
        };
        let mut screenshot = screenshot_evidence(&result);
        if let Some(mobile_capture) = self.mobile_capture.as_mut()
            && requirements.iter().any(|requirement| {
                matches!(
                    requirement.as_str(),
                    "screenshot" | "capture.scene" | "capture.device"
                )
            })
        {
            screenshot = Some(mobile_capture.capture(&observation_id, deadline).map_err(
                |error| {
                    DriverError::unavailable(
                        "mobile_capture_unavailable",
                        format!("mobile capture failed: {error:#}"),
                    )
                },
            )?);
        }
        Ok(Observation {
            observation_id,
            window_id: result["window_id"].as_str().map(str::to_owned),
            revision: result["source_revision"]
                .as_u64()
                .map(|value| value.to_string()),
            nodes,
            runtime_errors,
            screenshot,
            log_seq,
        })
    }

    fn record_ready_environment(&mut self, event: &Event) {
        self.ready_fixture_hash = event.data["fixture_hash"].as_str().map(str::to_owned);
        self.ready_run_id = event.scope.run_id.clone();
        self.ready_environment = event.data["environment"].clone();
        self.ready_reset_generation = event.data["reset_generation"].as_u64();
        self.ready_uncontrolled_inputs = event.data["uncontrolled_inputs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }

    fn environment_string(&self, field: &str) -> Option<String> {
        self.ready_environment
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn baseline_key(&self, screenshot: &ScreenshotEvidence) -> Result<BaselineKey, String> {
        let mut missing = Vec::new();
        let fixture_hash = self.ready_fixture_hash.clone();
        if fixture_hash.is_none() {
            missing.push("fixture_hash");
        } else if self.fixture_hash.as_deref() != fixture_hash.as_deref() {
            return Err("baseline_not_comparable:fixture_hash".into());
        }
        let backend = self.environment_string("backend");
        if backend.is_none() {
            missing.push("backend");
        }
        let os = self
            .environment_string("os")
            .or_else(|| Some(std::env::consts::OS.to_owned()));
        let font_fingerprint = self.environment_string("font_fingerprint");
        if font_fingerprint.is_none() {
            missing.push("font_fingerprint");
        }
        let logical_width = screenshot.logical_width;
        if logical_width != Some(self.viewport_width) {
            return Err("baseline_not_comparable:viewport_width".into());
        }
        let logical_height = screenshot.logical_height;
        if logical_height != Some(self.viewport_height) {
            return Err("baseline_not_comparable:viewport_height".into());
        }
        let scale_milli = screenshot.scale_milli;
        if scale_milli.is_none() {
            missing.push("scale_milli");
        }
        let scope = screenshot.scope.clone();
        if scope.is_none() {
            missing.push("scope");
        }
        let theme = self.environment_string("theme");
        if theme.as_deref() != Some(self.requested_theme.as_str()) {
            return Err("baseline_not_comparable:theme".into());
        }
        let locale = self.environment_string("locale");
        if locale.as_deref() != Some(self.requested_locale.as_str()) {
            return Err("baseline_not_comparable:locale".into());
        }
        if !missing.is_empty() {
            return Err(format!("baseline_key_unavailable:{}", missing.join(",")));
        }
        Ok(BaselineKey {
            scenario: self.scenario_id.clone(),
            fixture_hash: fixture_hash.expect("checked above"),
            target: self.target.clone(),
            backend: backend.expect("checked above"),
            os: os.expect("always has a host OS fallback"),
            viewport_width: self.viewport_width,
            viewport_height: self.viewport_height,
            scale_milli: scale_milli.expect("checked above"),
            theme: theme.expect("checked above"),
            locale: locale.expect("checked above"),
            font_fingerprint: font_fingerprint.expect("checked above"),
            scope: scope.expect("checked above"),
        })
    }

    fn read_artifact(&self, artifact_id: &str, deadline: Instant) -> Result<Vec<u8>, DriverError> {
        let info_reply = control::request(
            &self.registration,
            &control::next_request_id("check.artifact.info"),
            ControlCommand::ArtifactInfo {
                artifact_id: artifact_id.to_owned(),
            },
        )
        .map_err(|error| DriverError::unknown("artifact_read_failed", error.to_string()))?;
        if !info_reply.ok {
            return Err(reply_error(&info_reply, "artifact_info_failed"));
        }
        let info = info_reply.result.unwrap_or_default();
        if info["status"] != "published" {
            return Err(DriverError::failed(
                "artifact_not_published",
                "screenshot artifact is not published",
            ));
        }
        if info["kind"] != "png" {
            return Err(DriverError::failed(
                "artifact_kind_mismatch",
                "screenshot evidence references a non-PNG artifact",
            ));
        }
        let declared_bytes = info["declared_bytes"]
            .as_u64()
            .ok_or_else(|| DriverError::failed("artifact_invalid", "artifact size is missing"))?;
        if declared_bytes == 0 || declared_bytes > MAX_IMAGE_BYTES {
            return Err(DriverError::failed(
                "artifact_size_invalid",
                "screenshot artifact exceeds the bounded image size",
            ));
        }
        let expected_hash = info["sha256"]
            .as_str()
            .and_then(normalize_sha256)
            .ok_or_else(|| {
                DriverError::failed("artifact_invalid", "artifact SHA-256 is invalid")
            })?;
        let mut bytes = Vec::with_capacity(declared_bytes as usize);
        let mut offset = 0_u64;
        while offset < declared_bytes {
            if Instant::now() >= deadline {
                return Err(DriverError::timeout(
                    "artifact_read_timeout",
                    "screenshot artifact read exceeded the scenario deadline",
                ));
            }
            let length = (declared_bytes - offset).min(ARTIFACT_CHUNK_BYTES as u64) as u32;
            let chunk_reply = control::request(
                &self.registration,
                &control::next_request_id("check.artifact.read"),
                ControlCommand::ArtifactRead {
                    artifact_id: artifact_id.to_owned(),
                    offset,
                    length,
                },
            )
            .map_err(|error| DriverError::unknown("artifact_read_failed", error.to_string()))?;
            if !chunk_reply.ok {
                return Err(reply_error(&chunk_reply, "artifact_read_failed"));
            }
            let chunk = chunk_reply.result.unwrap_or_default();
            if chunk["offset"].as_u64() != Some(offset) {
                return Err(DriverError::failed(
                    "artifact_invalid",
                    "artifact chunk offset does not match the requested offset",
                ));
            }
            let encoded = chunk["data"].as_str().ok_or_else(|| {
                DriverError::failed("artifact_invalid", "artifact chunk is missing data")
            })?;
            let data = crate::devserver::protocol::b64::decode(encoded).ok_or_else(|| {
                DriverError::failed("artifact_invalid", "artifact chunk is not valid base64")
            })?;
            if data.is_empty()
                || data.len() as u64 > declared_bytes - offset
                || chunk["bytes"].as_u64() != Some(data.len() as u64)
            {
                return Err(DriverError::failed(
                    "artifact_invalid",
                    "artifact chunk length is invalid",
                ));
            }
            offset += data.len() as u64;
            bytes.extend_from_slice(&data);
            let eof = chunk["eof"].as_bool().unwrap_or(false);
            if eof != (offset == declared_bytes) {
                return Err(DriverError::failed(
                    "artifact_invalid",
                    "artifact EOF does not match its declared size",
                ));
            }
        }
        let actual_hash = format!("{:x}", Sha256::digest(&bytes));
        if actual_hash != expected_hash {
            return Err(DriverError::unknown(
                "artifact_checksum_mismatch",
                "screenshot artifact checksum does not match its manifest",
            ));
        }
        Ok(bytes)
    }

    fn write_diff_artifact(
        &self,
        observation_id: &str,
        baseline_id: &str,
        diff: DiffPng,
    ) -> Result<DiffEvidence, String> {
        let gpui_dir = self.project_root.join(".gpui");
        if let Ok(metadata) = fs::symlink_metadata(&gpui_dir)
            && metadata.file_type().is_symlink()
        {
            return Err("diff output root is a symbolic link".into());
        }
        let checks_dir = gpui_dir.join("checks");
        if let Ok(metadata) = fs::symlink_metadata(&checks_dir)
            && metadata.file_type().is_symlink()
        {
            return Err("diff output directory is a symbolic link".into());
        }
        fs::create_dir_all(&checks_dir).map_err(|error| error.to_string())?;
        for path in [gpui_dir, checks_dir.clone()] {
            if fs::symlink_metadata(&path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("diff output path contains a symbolic link".into());
            }
        }
        let file_name = format!(
            "{}-{}-{}.diff.png",
            safe_diff_component(&self.scenario_id),
            safe_diff_component(observation_id),
            safe_diff_component(baseline_id)
        );
        let output = checks_dir.join(file_name);
        if let Ok(metadata) = fs::symlink_metadata(&output)
            && (metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err("diff output path is not a regular file".into());
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(&checks_dir).map_err(|error| error.to_string())?;
        temporary
            .write_all(&diff.bytes)
            .map_err(|error| error.to_string())?;
        temporary
            .as_file_mut()
            .sync_all()
            .map_err(|error| error.to_string())?;
        temporary
            .persist(&output)
            .map_err(|error| error.error.to_string())?;
        let path = output
            .strip_prefix(&self.project_root)
            .unwrap_or(&output)
            .to_string_lossy()
            .into_owned();
        Ok(DiffEvidence {
            path,
            bytes: diff.bytes.len() as u64,
            sha256: format!("sha256:{:x}", Sha256::digest(&diff.bytes)),
            changed_pixels: diff.changed_pixels,
            total_pixels: diff.total_pixels,
            pixel_width: diff.pixel_width,
            pixel_height: diff.pixel_height,
        })
    }

    fn query_nodes(&self, observation_id: &str) -> Result<Vec<SemanticNode>, DriverError> {
        let reply = control::request(
            &self.registration,
            &control::next_request_id("check.query"),
            ControlCommand::Query {
                observation_id: observation_id.to_owned(),
                node_ref: None,
                logical_id: None,
                role_name: None,
                name: None,
                parent: None,
                fields: vec![
                    "node_ref".into(),
                    "logical_id".into(),
                    "role".into(),
                    "name".into(),
                    "value".into(),
                    "enabled".into(),
                    "focused".into(),
                    "bounds".into(),
                    "clip_bounds".into(),
                    "children".into(),
                    "parent".into(),
                ],
                cursor: None,
                limit: QUERY_LIMIT,
            },
        )
        .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
        if !reply.ok {
            return Err(reply_error(&reply, "query_failed"));
        }
        let result = reply.result.unwrap_or_default();
        Ok(result["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(semantic_node)
            .collect())
    }

    fn logical_selector(selector: Option<&Selector>) -> Result<String, DriverError> {
        selector
            .and_then(|selector| selector.logical_id.clone())
            .ok_or_else(|| {
                DriverError::unavailable(
                    "selector_not_routable",
                    "desktop check actions currently require a logical_id selector",
                )
            })
    }

    fn action_for_step(step: &ScenarioStep) -> Result<Action, DriverError> {
        match step.kind.as_str() {
            "click" => Ok(Action::Click {
                button: step.button.clone(),
            }),
            "type_text" => Ok(Action::TypeText {
                text: step.text.clone().unwrap_or_default(),
                mode: step.mode.clone(),
            }),
            "key" => Ok(Action::Key {
                key: step.key.clone().unwrap_or_default(),
            }),
            "scroll" => Ok(Action::Scroll {
                delta_x: step.delta_x as f32,
                delta_y: step.delta_y as f32,
                duration_ms: step.duration_ms,
            }),
            _ => Err(DriverError::failed(
                "unsupported_action_step",
                format!("step `{}` is not an input action", step.kind),
            )),
        }
    }

    fn run_id(&self) -> Option<&str> {
        self.ready_run_id.as_deref()
    }
}

impl ScenarioRunner for DesktopCheckRunner {
    fn resolve_screenshot(
        &mut self,
        step: &ScenarioStep,
        before: &Observation,
        deadline: Instant,
    ) -> Result<Observation, DriverError> {
        let mut observation = if before
            .screenshot
            .as_ref()
            .and_then(|screenshot| screenshot.artifact_id.as_ref())
            .is_some()
        {
            before.clone()
        } else {
            self.observe_with_requirements(vec!["screenshot".into()], deadline)?
        };
        let Some(mut screenshot) = observation.screenshot.take() else {
            return Ok(observation);
        };
        screenshot.baseline_id = step.baseline_id.clone();
        let Some(baseline_id) = step.baseline_id.as_deref() else {
            observation.screenshot = Some(screenshot);
            return Ok(observation);
        };
        let key = match self.baseline_key(&screenshot) {
            Ok(key) => key,
            Err(reason) => {
                screenshot.reason = Some(reason);
                observation.screenshot = Some(screenshot);
                return Ok(observation);
            }
        };
        screenshot.baseline_key = Some(key.clone());
        let (load_status, loaded) =
            load_baseline(&self.project_root, &self.target, baseline_id, &key);
        let Some(baseline) = loaded else {
            screenshot.reason = Some(match load_status {
                BaselineLoad::Missing { .. } => "baseline_missing".into(),
                BaselineLoad::Invalid { code, .. } => format!("baseline_invalid:{code}"),
                BaselineLoad::NotComparable { mismatches } => {
                    format!("baseline_not_comparable:{}", mismatches.join(","))
                }
                BaselineLoad::Loaded => "baseline_load_failed".into(),
            });
            observation.screenshot = Some(screenshot);
            return Ok(observation);
        };
        let artifact_id = screenshot.artifact_id.clone().ok_or_else(|| {
            DriverError::failed(
                "screenshot_artifact_missing",
                "screenshot observation has no PNG artifact id",
            )
        })?;
        let actual = self.read_artifact(&artifact_id, deadline)?;
        match compare_png(&baseline, &actual) {
            BaselineComparison::Matched { .. } => {
                screenshot.comparable = true;
                screenshot.matches = Some(true);
                screenshot.reason = None;
            }
            BaselineComparison::Different { .. } => {
                screenshot.comparable = true;
                screenshot.matches = Some(false);
                screenshot.reason = Some("baseline_different".into());
                match diff_png(&baseline, &actual) {
                    Ok(diff) => match self.write_diff_artifact(
                        &observation.observation_id,
                        baseline_id,
                        diff,
                    ) {
                        Ok(evidence) => screenshot.diff = Some(evidence),
                        Err(error) => {
                            screenshot.reason =
                                Some(format!("baseline_different:diff_unavailable:{error}"));
                        }
                    },
                    Err(error) => {
                        screenshot.reason =
                            Some(format!("baseline_different:diff_unavailable:{error}"));
                    }
                }
            }
            BaselineComparison::NotComparable { code, message } => {
                screenshot.comparable = false;
                screenshot.matches = None;
                screenshot.reason = Some(format!("{code}:{message}"));
            }
        }
        observation.screenshot = Some(screenshot);
        Ok(observation)
    }

    fn prepare(
        &mut self,
        scenario: &ScenarioDefinition,
        deadline: Instant,
    ) -> Result<Observation, DriverError> {
        let diagnostics = control::request(
            &self.registration,
            &control::next_request_id("check.baseline"),
            ControlCommand::Diagnostics,
        )
        .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
        self.issue_floor = diagnostics
            .result
            .as_ref()
            .and_then(|value| value["seq"].as_u64())
            .unwrap_or(0);
        let reset = control::request(
            &self.registration,
            &control::next_request_id("check.reset"),
            ControlCommand::ScenarioReset {
                scenario_id: scenario.id.clone(),
            },
        )
        .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
        if !reset.ok {
            return Err(reply_error(&reset, "reset_failed"));
        }
        let reset_result = reset.result.unwrap_or_default();
        let request_id = reset_result["request_id"]
            .as_str()
            .ok_or_else(|| DriverError::failed("invalid_reset", "reset has no request_id"))?;
        self.wait_for_reset(request_id, deadline)?;
        let observation =
            self.observe_with_requirements(observation_requirements(&scenario.requires), deadline)?;
        Ok(observation)
    }

    fn observe(&mut self, deadline: Instant) -> Result<Observation, DriverError> {
        self.observe_with_requirements(self.default_requirements.clone(), deadline)
    }

    fn act(
        &mut self,
        step: &ScenarioStep,
        before: &Observation,
        deadline: Instant,
    ) -> Result<ActionResult, DriverError> {
        let logical_id = Self::logical_selector(step.selector.as_ref())?;
        let action = Self::action_for_step(step)?;
        let window_id = before.window_id.clone().ok_or_else(|| {
            DriverError::unavailable("window_unavailable", "observation has no window id")
        })?;
        let reply = control::request(
            &self.registration,
            &control::next_request_id("check.action"),
            ControlCommand::Act {
                observation_id: before.observation_id.clone(),
                window_id,
                logical_id,
                action,
                deadline_ms: remaining_ms(deadline),
            },
        )
        .map_err(|error| DriverError::unknown("control_disconnected", error.to_string()))?;
        if !reply.ok {
            return Err(reply_error(&reply, "action_failed"));
        }
        let submitted = reply.result.unwrap_or_default();
        let operation_id = submitted["operation_id"]
            .as_str()
            .ok_or_else(|| DriverError::failed("invalid_action", "action has no operation_id"))?;
        let operation = self.wait_operation(operation_id, deadline)?;
        Ok(ActionResult::succeeded(
            operation["operation_id"].as_str().unwrap_or(operation_id),
        ))
    }

    fn wait_for(
        &mut self,
        step: &ScenarioStep,
        _before: &Observation,
        deadline: Instant,
    ) -> Result<Observation, DriverError> {
        let requirements = requirements_for_step(step);
        let mut last = None;
        while Instant::now() < deadline {
            let observation = self.observe_with_requirements(requirements.clone(), deadline)?;
            let evaluation = crate::scenario::executor::evaluate_assertion(
                step.assertion.as_deref().unwrap_or(""),
                step.selector.as_ref(),
                step.expected.as_ref(),
                &observation,
            );
            last = Some(observation.clone());
            if evaluation.status == crate::scenario::executor::StepStatus::Passed {
                return Ok(observation);
            }
            thread::sleep(Duration::from_millis(25));
        }
        last.ok_or_else(|| {
            DriverError::timeout(
                "wait_timeout",
                "wait_for did not obtain an observation before the scenario deadline",
            )
        })
    }

    fn capture(
        &mut self,
        step: &ScenarioStep,
        _observation: &Observation,
        deadline: Instant,
    ) -> Result<CaptureEvidence, DriverError> {
        let requirements = if step.require.is_empty() {
            vec!["screenshot".into()]
        } else {
            step.require.clone()
        };
        let observed =
            self.observe_with_requirements(observation_requirements(&requirements), deadline)?;
        let mut kinds = Vec::new();
        if observed.screenshot.is_some() {
            kinds.push("screenshot".into());
        }
        if observed.nodes.is_some() {
            kinds.push("semantics".into());
        }
        Ok(CaptureEvidence {
            artifact_id: observed
                .screenshot
                .as_ref()
                .and_then(|screenshot| screenshot.artifact_id.clone()),
            kinds,
        })
    }

    fn finalize(&mut self, _deadline: Instant) -> Result<(), DriverError> {
        if let Some(mobile_capture) = self.mobile_capture.as_mut() {
            mobile_capture.finalize().map_err(|error| {
                DriverError::unknown("mobile_logs_unavailable", error.to_string())
            })?;
        }
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), DriverError> {
        let child_result = self
            .child
            .terminate()
            .map_err(|error| DriverError::failed("cleanup_failed", error.to_string()));
        if let Some(mobile_capture) = self.mobile_capture.as_mut()
            && let Err(error) = mobile_capture.cleanup()
        {
            return Err(DriverError::failed(
                "mobile_cleanup_failed",
                error.to_string(),
            ));
        }
        child_result?;
        Ok(())
    }

    fn context(&self) -> Option<CheckContext> {
        Some(CheckContext {
            reset_generation: self.ready_reset_generation,
            snapshot_hash: self.snapshot_hash.clone(),
            build_key: self.build_key.clone(),
            environment: Some(self.ready_environment.clone()),
            uncontrolled_inputs: self.ready_uncontrolled_inputs.clone(),
            mobile_evidence: self.mobile_capture.as_ref().map(MobileCapture::evidence),
        })
    }

    fn fixture_hash(&self) -> Option<String> {
        self.ready_fixture_hash
            .clone()
            .or_else(|| self.fixture_hash.clone())
    }
}

impl Drop for DesktopCheckRunner {
    fn drop(&mut self) {
        let _ = self.child.terminate();
        if let Some(mobile_capture) = self.mobile_capture.as_mut() {
            let _ = mobile_capture.cleanup();
        }
    }
}

fn wait_for_registration(
    project_root: &Path,
    scenario_id: &str,
    session_key: &str,
    deadline: Instant,
) -> Result<Registration> {
    let target_suffix = format!("::gpui-check:{session_key}");
    loop {
        if Instant::now() >= deadline {
            bail!("no control session appeared for scenario `{scenario_id}`")
        }
        if let Ok(registration) = control::discover_target_suffix(project_root, &target_suffix) {
            return Ok(registration);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn remaining_ms(deadline: Instant) -> u64 {
    deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
        .clamp(1, control::MAX_WAIT_MS)
}

fn requires_semantics(requirements: &[String]) -> bool {
    requirements.iter().any(|requirement| {
        matches!(
            requirement.as_str(),
            "semantics" | "semantics.read" | "semantics.bounds"
        )
    })
}

fn observation_requirements(requirements: &[String]) -> Vec<String> {
    let mut filtered = requirements
        .iter()
        .flat_map(|requirement| requirement.split(','))
        .map(str::trim)
        .filter(|requirement| {
            matches!(
                *requirement,
                "screenshot"
                    | "semantics"
                    | "semantics.read"
                    | "semantics.bounds"
                    | "capture.scene"
                    | "capture.window"
                    | "capture.device"
            )
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        filtered.push("semantics".into());
    }
    filtered.sort();
    filtered.dedup();
    filtered
}

fn requirements_for_step(step: &ScenarioStep) -> Vec<String> {
    let mut requirements = vec!["semantics".into()];
    if matches!(step.assertion.as_deref(), Some("visible" | "not_clipped")) {
        requirements.push("semantics.bounds".into());
    }
    requirements
}

fn semantic_node(value: Value) -> SemanticNode {
    let bounds = value.get("bounds");
    let clip_bounds = value.get("clip_bounds");
    let visible = bounds.and_then(|bounds| {
        let width = bounds["width"].as_f64()?;
        let height = bounds["height"].as_f64()?;
        Some(width > 0.0 && height > 0.0)
    });
    let clipped = bounds.zip(clip_bounds).and_then(|(bounds, clip)| {
        let x = bounds["x"].as_f64()?;
        let y = bounds["y"].as_f64()?;
        let width = bounds["width"].as_f64()?;
        let height = bounds["height"].as_f64()?;
        let clip_x = clip["x"].as_f64()?;
        let clip_y = clip["y"].as_f64()?;
        let clip_width = clip["width"].as_f64()?;
        let clip_height = clip["height"].as_f64()?;
        Some(
            x < clip_x
                || y < clip_y
                || x + width > clip_x + clip_width
                || y + height > clip_y + clip_height,
        )
    });
    let value_field = value.get("value").filter(|value| !value.is_null()).cloned();
    SemanticNode {
        logical_id: value["logical_id"].as_str().map(str::to_owned),
        role: value["role"].as_str().map(str::to_owned),
        name: value["name"].as_str().map(str::to_owned),
        text: value_field
            .as_ref()
            .and_then(Value::as_str)
            .map(str::to_owned),
        value: value_field,
        enabled: value["enabled"].as_bool(),
        focused: value["focused"].as_bool(),
        visible,
        clipped,
    }
}

fn screenshot_evidence(result: &Value) -> Option<ScreenshotEvidence> {
    let artifacts = result["artifacts"].as_array()?;
    let artifact = artifacts
        .iter()
        .find(|artifact| artifact["kind"] == "png")?;
    Some(ScreenshotEvidence {
        artifact_id: artifact["artifact_id"].as_str().map(str::to_owned),
        manifest_path: artifact["manifest_path"].as_str().map(str::to_owned),
        baseline_id: None,
        baseline_key: None,
        diff: None,
        scope: result["scope"].as_str().map(str::to_owned),
        provider: result["provider"].as_str().map(str::to_owned),
        pixel_width: result["pixel_width"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok()),
        pixel_height: result["pixel_height"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok()),
        logical_width: result["logical_width"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok()),
        logical_height: result["logical_height"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok()),
        scale_milli: result["scale_milli"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok()),
        comparable: false,
        matches: None,
        reason: None,
    })
}

fn normalize_sha256(value: &str) -> Option<String> {
    let hex = value.strip_prefix("sha256:").unwrap_or(value);
    (hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

fn safe_diff_component(value: &str) -> String {
    let mut result = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                byte as char
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() {
        result.push('_');
    }
    result.truncate(128);
    result
}

fn operation_error(operation: &Value, kind: DriverErrorKind) -> DriverError {
    let error = &operation["error"];
    let code = error["code"].as_str().unwrap_or("operation_failed");
    let kind = if kind == DriverErrorKind::Failed
        && matches!(
            code,
            "unavailable" | "target_unavailable" | "window_closed" | "ui_unresponsive"
        ) {
        DriverErrorKind::Unavailable
    } else {
        kind
    };
    DriverError {
        kind,
        code: code.into(),
        message: error["message"]
            .as_str()
            .unwrap_or("operation did not succeed")
            .into(),
        details: Some(json!({"operation": operation})),
    }
}

fn reply_error(reply: &control::Reply, fallback: &str) -> DriverError {
    let error = reply.error.as_ref();
    let code = error.map(|error| error.code.as_str()).unwrap_or(fallback);
    let message = error
        .map(|error| error.message.as_str())
        .unwrap_or("control request failed");
    let kind = match code {
        "unavailable" | "target_unavailable" | "window_closed" => DriverErrorKind::Unavailable,
        "cancelled" => DriverErrorKind::Cancelled,
        "unknown" | "action_outcome_unknown" => DriverErrorKind::Unknown,
        _ => DriverErrorKind::Failed,
    };
    DriverError {
        kind,
        code: code.into(),
        message: message.into(),
        details: error.and_then(|error| error.details.clone()),
    }
}

fn driver_to_anyhow(error: DriverError) -> anyhow::Error {
    anyhow::anyhow!("{}: {}", error.code, error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::mobile::{
        CaptureArtifact, EvidenceLog, LaunchEvidence, PreparedRun, RunnerCapabilities, RunnerInfo,
    };
    use std::collections::BTreeMap;

    struct EvidenceMobileRunner {
        fail_stop: bool,
    }

    impl MobileRunner for EvidenceMobileRunner {
        fn describe(&self) -> &RunnerInfo {
            static INFO: std::sync::OnceLock<RunnerInfo> = std::sync::OnceLock::new();
            INFO.get_or_init(|| RunnerInfo {
                runner_id: "evidence-test".into(),
                host_id: "test".into(),
                platform: "android".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                stable_device_id: "evidence-device".into(),
                device_kind: "emulator".into(),
                tool_versions: BTreeMap::new(),
                resources: vec!["evidence-device".into()],
            })
        }

        fn capabilities(&self) -> RunnerCapabilities {
            RunnerCapabilities {
                native_logs: true,
                stop_owned: true,
                ..RunnerCapabilities::default()
            }
        }

        fn evidence_log(&self) -> Option<&EvidenceLog> {
            static LOG: std::sync::OnceLock<EvidenceLog> = std::sync::OnceLock::new();
            Some(LOG.get_or_init(EvidenceLog::new))
        }

        fn prepare(
            &mut self,
            request: &RunRequest,
            lease: &DeviceLeaseSession,
        ) -> Result<PreparedRun> {
            request.prepare(lease)
        }

        fn launch(
            &mut self,
            _prepared: &PreparedRun,
            _lease: &DeviceLeaseSession,
        ) -> Result<LaunchEvidence> {
            Err(anyhow::anyhow!("launch is not used by this evidence test"))
        }

        fn capture(
            &mut self,
            _scope: &CaptureScope,
            _lease: &DeviceLeaseSession,
        ) -> Result<CaptureArtifact> {
            Err(anyhow::anyhow!("capture is not used by this evidence test"))
        }

        fn collect_logs(
            &mut self,
            identity: &RunIdentity,
            _lease: &DeviceLeaseSession,
        ) -> Result<LogEvidence> {
            Ok(LogEvidence {
                run_id: identity.run_id.clone(),
                source: "evidence-test".into(),
                path: PathBuf::from("native.log"),
                bytes: 12,
                truncated: false,
                pid: None,
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
            if self.fail_stop {
                return Err(anyhow::anyhow!("fake stop failed"));
            }
            Ok(StopEvidence {
                run_id: identity.run_id.clone(),
                stopped_owned_process: false,
                removed_owned_resources: Vec::new(),
                preserved_resources: vec![identity.device_id.clone()],
                at_ms: 1,
            })
        }
    }

    #[test]
    fn bounds_turn_into_conservative_visibility_flags() {
        let node = semantic_node(json!({
            "logical_id": "counter.value",
            "bounds": {"x": 0, "y": 0, "width": 40, "height": 20},
            "clip_bounds": {"x": 0, "y": 0, "width": 30, "height": 20}
        }));
        assert_eq!(node.visible, Some(true));
        assert_eq!(node.clipped, Some(true));
    }

    #[test]
    fn action_steps_require_logical_ids_for_real_routing() {
        let selector = Selector {
            logical_id: None,
            role: Some("button".into()),
            name: Some("Sign in".into()),
        };
        assert!(DesktopCheckRunner::logical_selector(Some(&selector)).is_err());
    }

    #[test]
    fn unavailable_operation_errors_remain_unavailable() {
        let operation = json!({
            "error": {
                "code": "unavailable",
                "message": "a11y inactive"
            }
        });
        assert_eq!(
            operation_error(&operation, DriverErrorKind::Failed).kind,
            DriverErrorKind::Unavailable
        );
    }

    #[test]
    fn pre_ready_mobile_failure_keeps_unbound_run_evidence() {
        let root = tempfile::tempdir().unwrap();
        let lease = DeviceLeaseSession::acquire(root.path(), "evidence-device").unwrap();
        let request = RunRequest {
            run_id: "prepared-run".into(),
            project_id: "project".into(),
            device_id: "evidence-device".into(),
            bundle_id: "com.example.app".into(),
            artifact_root: root.path().join("artifacts"),
            abi: Some("x86_64".into()),
        };
        let identity = request.prepare(&lease).unwrap().identity;
        let mut capture = MobileCapture {
            runner: Box::new(EvidenceMobileRunner { fail_stop: false }),
            lease: Some(lease),
            identity,
            scenario: MobileScenarioEvidence::new(
                "cell-1",
                "android",
                "counter",
                &["capture.device".into(), "input.keyboard".into()],
                Some("fixture-hash"),
            ),
            run_id_bound: false,
            artifact_root: root.path().join("artifacts"),
            captures: Vec::new(),
            native_logs: None,
            stop: None,
            cleanup_errors: Vec::new(),
        };

        let errors = capture.finalize_and_cleanup();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("finalizing mobile evidence"));
        let evidence = capture.evidence();
        assert_eq!(evidence["run_id_bound"], false);
        assert_eq!(evidence["project_id"], "project");
        assert_eq!(evidence["scenario"]["cell_id"], "cell-1");
        assert_eq!(evidence["scenario"]["target_id"], "android");
        assert_eq!(evidence["scenario"]["scenario_id"], "counter");
        assert_eq!(evidence["scenario"]["fixture_hash"], "fixture-hash");
        assert_eq!(evidence["scenario"]["requirements"][0], "capture.device");
        assert_eq!(evidence["run_identity"], Value::Null);
        assert!(evidence.get("fencing_token").is_none());
        assert_eq!(evidence["runner"]["runner_id"], "evidence-test");
        assert_eq!(evidence["capabilities"]["native_logs"], true);
        assert_eq!(evidence["event_log"]["contract_version"], 1);
        assert_eq!(evidence["native_logs"]["assigned_to_run"], false);
        assert_eq!(
            evidence["native_logs"]["unassigned_reason"],
            "preview run identity was unavailable before scenario_ready"
        );
        assert_eq!(evidence["stop"]["run_id"], "prepared-run");
        assert!(capture.lease.is_none());
    }

    #[test]
    fn cleanup_failure_retains_mobile_evidence_and_releases_lease() {
        let root = tempfile::tempdir().unwrap();
        let lease = DeviceLeaseSession::acquire(root.path(), "evidence-device-cleanup").unwrap();
        let request = RunRequest {
            run_id: "ready-run".into(),
            project_id: "project".into(),
            device_id: "evidence-device-cleanup".into(),
            bundle_id: "com.example.app".into(),
            artifact_root: root.path().join("artifacts"),
            abi: Some("x86_64".into()),
        };
        let identity = request.prepare(&lease).unwrap().identity;
        let mut capture = MobileCapture {
            runner: Box::new(EvidenceMobileRunner { fail_stop: true }),
            lease: Some(lease),
            identity,
            scenario: MobileScenarioEvidence::new(
                "cell-2",
                "android",
                "counter",
                &["capture.device".into()],
                None,
            ),
            run_id_bound: true,
            artifact_root: root.path().join("artifacts"),
            captures: Vec::new(),
            native_logs: None,
            stop: None,
            cleanup_errors: Vec::new(),
        };

        let errors = capture.finalize_and_cleanup();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("cleaning up mobile evidence"));
        let evidence = capture.evidence();
        assert_eq!(evidence["project_id"], "project");
        assert_eq!(evidence["run_identity"]["run_id"], "ready-run");
        assert_eq!(evidence["run_identity"]["project_id"], "project");
        assert_eq!(
            evidence["run_identity"]["device_id"],
            "evidence-device-cleanup"
        );
        assert!(evidence["run_identity"]["lease_session_id"].is_string());
        assert_eq!(
            evidence["run_identity"]["fencing_token_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(evidence["run_identity"].get("fencing_token").is_none());
        assert_eq!(evidence["native_logs"]["assigned_to_run"], true);
        assert_eq!(evidence["stop"], Value::Null);
        assert_eq!(
            evidence["cleanup_errors"][0],
            "stop_owned: fake stop failed"
        );
        assert!(capture.lease.is_none());
    }

    #[test]
    fn screenshot_evidence_keeps_the_png_artifact_and_capture_dimensions() {
        let result = json!({
            "scope": "window",
            "provider": "macos_screencapture",
            "pixel_width": 1280,
            "pixel_height": 720,
            "logical_width": 640,
            "logical_height": 480,
            "scale_milli": 2000,
            "artifacts": [
                {"kind": "tree", "artifact_id": "tree-1"},
                {"kind": "png", "artifact_id": "png-1"}
            ]
        });
        let evidence = screenshot_evidence(&result).unwrap();
        assert_eq!(evidence.artifact_id.as_deref(), Some("png-1"));
        assert_eq!(evidence.scope.as_deref(), Some("window"));
        assert_eq!(evidence.provider.as_deref(), Some("macos_screencapture"));
        assert_eq!(evidence.pixel_width, Some(1280));
        assert_eq!(evidence.logical_width, Some(640));
        assert_eq!(evidence.scale_milli, Some(2000));
        assert_eq!(
            normalize_sha256(&format!("sha256:{}", "A".repeat(64))),
            Some("a".repeat(64))
        );
    }

    #[test]
    fn screenshot_evidence_requires_a_png_artifact() {
        assert!(
            screenshot_evidence(&json!({
                "artifacts": [{"kind": "tree", "artifact_id": "tree-1"}]
            }))
            .is_none()
        );
    }

    #[test]
    fn mobile_screenshot_evidence_exposes_only_relative_manifest_path() {
        let root = tempfile::tempdir().unwrap();
        let artifact = CaptureArtifact {
            artifact_id: "png-test".into(),
            path: root.path().join("captures/observation.png"),
            manifest_path: root.path().join("captures/observation.png.manifest.json"),
            provider: "simctl".into(),
            bytes: 42,
            sha256: "a".repeat(64),
            width: 2,
            height: 3,
            logical_width: Some(1),
            logical_height: Some(2),
            scale_milli: Some(2000),
            orientation: Some("portrait".into()),
            system_ui: true,
            foreground_app: Some("com.example.app".into()),
            run_id: "run-1".into(),
        };

        let evidence = screenshot_from_mobile_artifact(&artifact, root.path()).unwrap();
        assert_eq!(
            evidence.manifest_path.as_deref(),
            Some("captures/observation.png.manifest.json")
        );
        assert!(!evidence.manifest_path.as_deref().unwrap().starts_with('/'));
        assert!(
            !evidence
                .manifest_path
                .as_deref()
                .unwrap()
                .contains(root.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn matrix_inputs_share_one_frozen_workspace_snapshot() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        fs::write(root.path().join("gpui.scenarios.toml"), "scenario = true\n").unwrap();
        fs::write(
            root.path().join("matrix.toml"),
            "source_mode = \"frozen\"\n",
        )
        .unwrap();
        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile"])
            .status()
            .unwrap();
        assert!(status.success());

        let frozen = prepare_frozen_matrix_check(
            root.path(),
            &root.path().join("gpui.scenarios.toml"),
            &root.path().join("matrix.toml"),
        )
        .unwrap();
        let source_root = fs::canonicalize(root.path()).unwrap();

        assert_ne!(frozen.inputs.snapshot.root, source_root);
        assert!(
            frozen
                .scenario_file
                .starts_with(&frozen.inputs.snapshot.root)
        );
        assert!(frozen.matrix_file.starts_with(&frozen.inputs.snapshot.root));
        assert!(!frozen.inputs.snapshot.input_hash.is_empty());
        assert!(frozen.inputs.cache_hit_disabled_reason.is_none());

        let build_key = matrix_target_build(
            root.path(),
            &frozen.inputs.snapshot.root,
            MatrixPlatform::Linux,
            None,
        )
        .unwrap()
        .key_hash;
        assert_eq!(build_key.len(), 64);
        assert!(build_key.bytes().all(|byte| byte.is_ascii_hexdigit()));

        let build = matrix_target_build(
            root.path(),
            &frozen.inputs.snapshot.root,
            MatrixPlatform::Linux,
            None,
        )
        .unwrap();
        assert!(
            build
                .outputs
                .cargo_target_dir
                .starts_with(source_root.join(".gpui/builds/desktop"))
        );
    }

    #[test]
    fn frozen_matrix_check_copies_supported_android_signing_inputs() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("mobile/android/gradle/app");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(root.path().join("app/src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        fs::write(
            app.join("build.gradle.kts"),
            r#"
                signingConfigs { create("release") { storeFile = file(keystoreProperties["storeFile"]) } }
                keystoreProperties.load(FileInputStream("keystore.properties"))
                signingConfig
            "#,
        )
        .unwrap();
        fs::write(
            root.path()
                .join("mobile/android/gradle/keystore.properties"),
            "storeFile=release.jks\n",
        )
        .unwrap();
        fs::write(app.join("release.jks"), b"private-keystore").unwrap();
        fs::write(root.path().join("gpui.scenarios.toml"), "scenario = true\n").unwrap();
        fs::write(
            root.path().join("matrix.toml"),
            "source_mode = \"frozen\"\n",
        )
        .unwrap();
        let status = Command::new("cargo")
            .current_dir(root.path())
            .args(["generate-lockfile"])
            .status()
            .unwrap();
        assert!(status.success());

        let frozen = prepare_frozen_matrix_check(
            root.path(),
            &root.path().join("gpui.scenarios.toml"),
            &root.path().join("matrix.toml"),
        )
        .unwrap();
        assert!(
            frozen
                .inputs
                .snapshot
                .root
                .join("mobile/android/gradle/keystore.properties")
                .is_file()
        );
        assert!(
            frozen
                .inputs
                .snapshot
                .root
                .join("mobile/android/gradle/app/release.jks")
                .is_file()
        );
    }
}
