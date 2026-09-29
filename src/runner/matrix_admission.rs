//! Matrix configuration parsing and pre-dispatch admission.
//!
//! Admission is deliberately separate from execution. It expands the
//! user-facing target/scenario matrix, checks static project references and
//! records host/runner/device/toolchain availability before a runner is
//! created. A missing optional target is still represented as an unavailable
//! cell; it is never silently dropped from the final matrix report.

use super::matrix::{
    MAX_MATRIX_TIMEOUT_MS, MatrixCellError, MatrixCellSpec, MatrixConfig, MatrixPlan,
    MatrixScheduler,
};
use crate::scenario::{self, ScenarioDefinition, ScenarioFile};
use crate::toolchain::Target as ToolchainTarget;
use crate::toolchain::probe::{ProbeConfig, ProbeRunner};
use crate::toolchain::report::{CheckReport, CheckStatus};
use crate::toolchain::requirements::{Context as ToolchainContext, requirements_for};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub const MATRIX_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_PARALLEL: u16 = 2;
pub const DEFAULT_TARGET_TIMEOUT_MS: u64 = 10 * 60 * 1000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MatrixPlatform {
    Macos,
    Windows,
    Linux,
    Ios,
    Android,
}

impl MatrixPlatform {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "macos" | "mac" | "osx" => Some(Self::Macos),
            "windows" | "win" => Some(Self::Windows),
            "linux" => Some(Self::Linux),
            "ios" => Some(Self::Ios),
            "android" => Some(Self::Android),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Ios => "ios",
            Self::Android => "android",
        }
    }

    fn toolchain_target(self) -> ToolchainTarget {
        match self {
            Self::Macos | Self::Windows | Self::Linux => ToolchainTarget::Desktop,
            Self::Ios => ToolchainTarget::Ios,
            Self::Android => ToolchainTarget::Android,
        }
    }

    fn host_compatible(self, host_os: &str) -> bool {
        match self {
            Self::Macos => host_os == "macos",
            Self::Windows => host_os == "windows",
            Self::Linux => host_os == "linux",
            Self::Ios => host_os == "macos",
            Self::Android => matches!(host_os, "macos" | "windows" | "linux"),
        }
    }

    fn is_mobile(self) -> bool {
        matches!(self, Self::Ios | Self::Android)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixFile {
    pub schema_version: u32,
    #[serde(default = "default_source_mode")]
    pub source_mode: String,
    #[serde(default = "default_max_parallel")]
    pub max_parallel: u16,
    #[serde(default)]
    pub fail_fast: bool,
    #[serde(default)]
    pub plan_id: Option<String>,
    pub targets: Vec<MatrixTargetConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixTargetConfig {
    pub id: String,
    pub runner: String,
    pub platform: String,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub abi: Option<String>,
    #[serde(default = "default_required")]
    pub required: bool,
    pub scenarios: Vec<String>,
    #[serde(default = "default_target_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_source_mode() -> String {
    "frozen".into()
}

fn default_max_parallel() -> u16 {
    DEFAULT_MAX_PARALLEL
}

fn default_required() -> bool {
    true
}

fn default_target_timeout_ms() -> u64 {
    DEFAULT_TARGET_TIMEOUT_MS
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct MatrixDeviceAvailability {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct MatrixRunnerAvailability {
    pub available: bool,
    pub host_os: String,
    #[serde(default)]
    pub platforms: BTreeSet<MatrixPlatform>,
    #[serde(default)]
    pub devices: BTreeMap<String, MatrixDeviceAvailability>,
}

#[derive(Clone, Debug, Default)]
pub struct MatrixAdmissionContext {
    /// None means the caller has not supplied a project target declaration.
    /// A loaded project should pass Some so an undeclared target is
    /// unavailable before dispatch.
    pub project_targets: Option<BTreeSet<String>>,
    pub runners: BTreeMap<String, MatrixRunnerAvailability>,
    pub toolchains: BTreeMap<MatrixPlatform, MatrixToolchainReport>,
    /// Scenario capabilities advertised by the runner implementation.
    /// Missing entries leave this part of admission unspecified.
    pub scenario_capabilities: BTreeMap<MatrixPlatform, BTreeSet<String>>,
}

impl MatrixAdmissionContext {
    pub fn for_local(host_os: impl Into<String>, project_targets: Vec<String>) -> Self {
        let host_os = host_os.into().trim().to_ascii_lowercase();
        let platforms = [
            MatrixPlatform::Macos,
            MatrixPlatform::Windows,
            MatrixPlatform::Linux,
            MatrixPlatform::Ios,
            MatrixPlatform::Android,
        ]
        .into_iter()
        .filter(|platform| platform.host_compatible(&host_os))
        .collect();
        let desktop_capabilities = [
            "screenshot",
            "capture.window",
            "semantics",
            "semantics.read",
            "semantics.bounds",
            "scenario.reset",
            "input.pointer",
            "input.keyboard",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        let mobile_capabilities = [
            "screenshot",
            "capture.device",
            "semantics",
            "semantics.read",
            "semantics.bounds",
            "scenario.reset",
            "input.pointer",
            "input.keyboard",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        Self {
            project_targets: Some(
                project_targets
                    .into_iter()
                    .map(|target| target.trim().to_ascii_lowercase())
                    .filter(|target| !target.is_empty())
                    .collect(),
            ),
            runners: BTreeMap::from([(
                "local".into(),
                MatrixRunnerAvailability {
                    available: true,
                    host_os,
                    platforms,
                    devices: BTreeMap::new(),
                },
            )]),
            toolchains: BTreeMap::new(),
            scenario_capabilities: BTreeMap::from([
                (MatrixPlatform::Macos, desktop_capabilities.clone()),
                (MatrixPlatform::Windows, desktop_capabilities.clone()),
                (MatrixPlatform::Linux, desktop_capabilities),
                (MatrixPlatform::Ios, mobile_capabilities.clone()),
                (MatrixPlatform::Android, mobile_capabilities),
            ]),
        }
    }

    pub fn with_runner(
        mut self,
        runner_id: impl Into<String>,
        runner: MatrixRunnerAvailability,
    ) -> Self {
        self.runners.insert(runner_id.into(), runner);
        self
    }

    pub fn with_toolchain(
        mut self,
        platform: MatrixPlatform,
        report: MatrixToolchainReport,
    ) -> Self {
        self.toolchains.insert(platform, report);
        self
    }

    pub fn with_scenario_capabilities(
        mut self,
        platform: MatrixPlatform,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.scenario_capabilities
            .insert(platform, capabilities.into_iter().map(Into::into).collect());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MatrixToolchainReport {
    pub target: ToolchainTarget,
    pub checks: Vec<CheckReport>,
}

impl MatrixToolchainReport {
    pub fn ready(&self) -> bool {
        let required_count = self.checks.iter().filter(|check| check.required).count();
        required_count > 0
            && self
                .checks
                .iter()
                .filter(|check| check.required)
                .all(|check| check.status == CheckStatus::Pass)
    }

    fn blocking_checks(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|check| check.required && check.status != CheckStatus::Pass)
            .map(|check| format!("{}: {}", check.id, check.reason))
            .collect()
    }
}

/// Runs the same bounded target-aware probes as gpui doctor for a matrix
/// target. The result is data-only so callers can include it in admission
/// evidence and unit tests can inject deterministic reports.
pub fn probe_local_toolchain(
    platform: MatrixPlatform,
    abi: Option<&str>,
    project_root: &Path,
    host_os: &str,
) -> Result<MatrixToolchainReport> {
    let mut context = ToolchainContext::for_host(host_os.to_owned());
    if let Some(abi) = abi {
        context.android_abis = vec![abi.to_owned()];
    }
    let requirements = requirements_for(&context, platform.toolchain_target())?;
    let cwd = if platform == MatrixPlatform::Android {
        let gradle = project_root.join("mobile/android/gradle");
        gradle
            .is_dir()
            .then_some(gradle)
            .or(Some(project_root.to_path_buf()))
    } else {
        Some(project_root.to_path_buf())
    };
    let mut probe = ProbeRunner::new(ProbeConfig::default());
    let checks = probe.probe(&requirements, cwd.as_deref());
    Ok(MatrixToolchainReport {
        target: platform.toolchain_target(),
        checks,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixAdmissionState {
    Ready,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MatrixAdmissionIssue {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MatrixCellAdmission {
    pub cell: MatrixCellSpec,
    pub state: MatrixAdmissionState,
    pub issues: Vec<MatrixAdmissionIssue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MatrixAdmission {
    pub plan: MatrixPlan,
    pub scenario_hash: Option<String>,
    pub cells: Vec<MatrixCellAdmission>,
}

impl MatrixAdmission {
    /// Applies pre-dispatch unavailable cells to a scheduler while retaining
    /// all ready cells for a future runner executor.
    pub fn apply(&self, scheduler: &mut MatrixScheduler) -> Result<()> {
        for admitted in &self.cells {
            if admitted.state != MatrixAdmissionState::Unavailable {
                continue;
            }
            let issue = admitted
                .issues
                .first()
                .cloned()
                .unwrap_or(MatrixAdmissionIssue {
                    code: "admission_unavailable".into(),
                    message: "cell was unavailable during matrix admission".into(),
                });
            scheduler.mark_unavailable(
                &admitted.cell.cell_id,
                MatrixCellError {
                    code: issue.code,
                    message: issue.message,
                },
            )?;
        }
        Ok(())
    }

    pub fn ready_cells(&self) -> impl Iterator<Item = &MatrixCellSpec> {
        self.cells
            .iter()
            .filter(|cell| cell.state == MatrixAdmissionState::Ready)
            .map(|cell| &cell.cell)
    }
}

/// Parses and admits an in-memory matrix against an already parsed scenario
/// file. File-oriented callers should use load_and_admit so the scenario
/// validator and its stable hash are included in the same boundary.
pub fn admit_matrix(
    matrix: MatrixFile,
    scenarios: ScenarioFile,
    scenario_hash: Option<String>,
    context: &MatrixAdmissionContext,
) -> Result<MatrixAdmission> {
    validate_matrix_file(&matrix)?;
    validate_scenario_ids(&scenarios)?;

    let scenario_by_id: BTreeMap<_, _> = scenarios
        .scenarios
        .iter()
        .map(|scenario| (scenario.id.as_str(), scenario))
        .collect();
    let mut cells = Vec::new();
    let mut plan_cells = Vec::new();
    let mut max_timeout_ms = 0;

    for target in &matrix.targets {
        let platform = MatrixPlatform::parse(&target.platform).ok_or_else(|| {
            anyhow::anyhow!(
                "matrix target '{}' has unknown platform '{}'",
                target.id,
                target.platform
            )
        })?;
        max_timeout_ms = max_timeout_ms.max(target.timeout_ms);
        for scenario_id in &target.scenarios {
            let scenario = scenario_by_id.get(scenario_id.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "matrix target '{}' references unknown scenario '{}'",
                    target.id,
                    scenario_id
                )
            })?;
            let cell = MatrixCellSpec {
                cell_id: format!("{}::{scenario_id}", target.id),
                target_id: target.id.clone(),
                scenario_id: scenario.id.clone(),
                required: target.required,
                timeout_ms: Some(target.timeout_ms),
                resource_ids: target
                    .device
                    .as_deref()
                    .map(|device| vec![format!("device:{}:{}", target.runner, device)])
                    .unwrap_or_default(),
            };
            let issues = admission_issues(target, platform, scenario, context);
            let state = if issues.is_empty() {
                MatrixAdmissionState::Ready
            } else {
                MatrixAdmissionState::Unavailable
            };
            plan_cells.push(cell.clone());
            cells.push(MatrixCellAdmission {
                cell,
                state,
                issues,
            });
        }
    }

    if max_timeout_ms == 0 {
        bail!("matrix plan must contain at least one target scenario cell");
    }
    let plan = MatrixPlan {
        plan_id: matrix.plan_id.clone().unwrap_or_else(|| "matrix".into()),
        config: MatrixConfig {
            max_parallel: matrix.max_parallel,
            fail_fast: matrix.fail_fast,
            timeout_ms: max_timeout_ms,
        },
        cells: plan_cells,
    };
    plan.validate()?;
    Ok(MatrixAdmission {
        plan,
        scenario_hash,
        cells,
    })
}

/// Loads the matrix and scenario files, runs static scenario validation, and
/// returns a normalized admission result ready to apply to a scheduler.
pub fn load_and_admit(
    matrix_path: &Path,
    scenario_path: &Path,
    project_root: &Path,
    context: &MatrixAdmissionContext,
) -> Result<MatrixAdmission> {
    let matrix_source = fs::read_to_string(matrix_path)
        .with_context(|| format!("reading matrix file {}", matrix_path.display()))?;
    let matrix: MatrixFile = toml::from_str(&matrix_source)
        .with_context(|| format!("parsing matrix file {}", matrix_path.display()))?;
    let validation = scenario::validate_file(scenario_path, project_root)?;
    if !validation.valid {
        let details = validation
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .take(4)
            .collect::<Vec<_>>()
            .join("; ");
        bail!("scenario validation failed: {details}");
    }
    let source = fs::read_to_string(scenario_path)
        .with_context(|| format!("reading scenario file {}", scenario_path.display()))?;
    let scenarios: ScenarioFile = toml::from_str(&source)
        .with_context(|| format!("parsing scenario file {}", scenario_path.display()))?;
    admit_matrix(matrix, scenarios, validation.scenario_hash, context)
}

fn validate_matrix_file(matrix: &MatrixFile) -> Result<()> {
    if matrix.schema_version != MATRIX_SCHEMA_VERSION {
        bail!(
            "unsupported matrix schema_version {}; expected {}",
            matrix.schema_version,
            MATRIX_SCHEMA_VERSION
        );
    }
    if matrix.source_mode != "frozen" {
        bail!(
            "matrix source_mode '{}' is unsupported; only frozen inputs are admitted",
            matrix.source_mode
        );
    }
    if matrix.targets.is_empty() {
        bail!("matrix must contain at least one target");
    }
    let mut ids = BTreeSet::new();
    for target in &matrix.targets {
        if target.id.trim().is_empty() || !ids.insert(&target.id) {
            bail!("matrix target ids must be non-empty and unique");
        }
        if target.runner.trim().is_empty() {
            bail!("matrix target '{}' runner must not be empty", target.id);
        }
        let platform = MatrixPlatform::parse(&target.platform).ok_or_else(|| {
            anyhow::anyhow!(
                "matrix target '{}' has unknown platform '{}'",
                target.id,
                target.platform
            )
        })?;
        if target.scenarios.is_empty() {
            bail!(
                "matrix target '{}' must list at least one scenario",
                target.id
            );
        }
        let mut scenarios = BTreeSet::new();
        for scenario in &target.scenarios {
            if scenario.trim().is_empty() || !scenarios.insert(scenario) {
                bail!(
                    "matrix target '{}' scenarios must be non-empty and unique",
                    target.id
                );
            }
        }
        if target.timeout_ms == 0 || target.timeout_ms > MAX_MATRIX_TIMEOUT_MS {
            bail!(
                "matrix target '{}' timeout_ms must be between 1 and {} milliseconds",
                target.id,
                MAX_MATRIX_TIMEOUT_MS
            );
        }
        match platform {
            MatrixPlatform::Android => {
                let abi = target.abi.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("Android target '{}' requires abi", target.id)
                })?;
                if !android_abi_supported(abi) {
                    bail!(
                        "Android target '{}' has unsupported ABI '{}'",
                        target.id,
                        abi
                    );
                }
                if target.device.as_deref().is_none_or(str::is_empty) {
                    bail!("Android target '{}' requires device", target.id);
                }
            }
            MatrixPlatform::Ios => {
                if target.abi.is_some() {
                    bail!("iOS target '{}' must not specify abi", target.id);
                }
                if target.device.as_deref().is_none_or(str::is_empty) {
                    bail!("iOS target '{}' requires device", target.id);
                }
            }
            MatrixPlatform::Macos | MatrixPlatform::Windows | MatrixPlatform::Linux => {
                if target.device.is_some() || target.abi.is_some() {
                    bail!(
                        "desktop target '{}' must not specify device or abi",
                        target.id
                    );
                }
            }
        }
    }
    if matrix.max_parallel == 0 {
        bail!("matrix max_parallel must be greater than zero");
    }
    Ok(())
}

fn validate_scenario_ids(scenarios: &ScenarioFile) -> Result<()> {
    if scenarios.schema_version != scenario::SCHEMA_VERSION {
        bail!(
            "unsupported scenario schema_version {}; expected {}",
            scenarios.schema_version,
            scenario::SCHEMA_VERSION
        );
    }
    let mut ids = BTreeSet::new();
    for scenario in &scenarios.scenarios {
        if scenario.id.trim().is_empty() || !ids.insert(&scenario.id) {
            bail!("scenario ids must be non-empty and unique");
        }
    }
    Ok(())
}

fn admission_issues(
    target: &MatrixTargetConfig,
    platform: MatrixPlatform,
    scenario: &ScenarioDefinition,
    context: &MatrixAdmissionContext,
) -> Vec<MatrixAdmissionIssue> {
    let mut issues = Vec::new();
    if let Some(project_targets) = &context.project_targets {
        let declared = project_targets.iter().any(|declared| match platform {
            MatrixPlatform::Macos | MatrixPlatform::Windows | MatrixPlatform::Linux => {
                matches!(declared.as_str(), "desktop" | "macos" | "windows" | "linux")
            }
            MatrixPlatform::Ios => declared == "ios",
            MatrixPlatform::Android => declared == "android",
        });
        if !declared {
            issues.push(issue(
                "target_not_declared",
                format!(
                    "project does not declare matrix platform '{}'",
                    platform.label()
                ),
            ));
        }
    }

    let Some(runner) = context.runners.get(&target.runner) else {
        issues.push(issue(
            "runner_unavailable",
            format!("runner '{}' is not registered", target.runner),
        ));
        return issues;
    };
    if !runner.available {
        issues.push(issue(
            "runner_unavailable",
            format!("runner '{}' is unavailable", target.runner),
        ));
    }
    if !runner.platforms.is_empty() && !runner.platforms.contains(&platform) {
        issues.push(issue(
            "runner_platform_unavailable",
            format!(
                "runner '{}' does not advertise platform '{}'",
                target.runner,
                platform.label()
            ),
        ));
    }
    if !platform.host_compatible(&runner.host_os) {
        issues.push(issue(
            "host_unavailable",
            format!(
                "platform '{}' cannot run on host '{}'",
                platform.label(),
                runner.host_os
            ),
        ));
    }

    if let Some(device_id) = &target.device {
        match runner.devices.get(device_id) {
            Some(device) if !device.available => issues.push(issue(
                "device_unavailable",
                format!("device '{}' is unavailable", device_id),
            )),
            Some(device) => {
                if platform == MatrixPlatform::Android
                    && device.arch.as_deref() != target.abi.as_deref()
                {
                    issues.push(issue(
                        "abi_mismatch",
                        format!(
                            "Android device '{}' reports ABI {:?}, requested {:?}",
                            device_id, device.arch, target.abi
                        ),
                    ));
                }
            }
            None => issues.push(issue(
                "device_unavailable",
                format!(
                    "device '{}' is not registered on runner '{}'",
                    device_id, target.runner
                ),
            )),
        }
    }

    for requirement in &scenario.requires {
        let supported = match requirement.as_str() {
            "capture.device" => platform.is_mobile(),
            "capture.window" => !platform.is_mobile(),
            _ => true,
        };
        if !supported {
            issues.push(issue(
                "scenario_requirement_unsupported",
                format!(
                    "scenario '{}' requires '{}' which platform '{}' cannot provide",
                    scenario.id,
                    requirement,
                    platform.label()
                ),
            ));
        }
        if let Some(capabilities) = context.scenario_capabilities.get(&platform)
            && !capabilities.contains(requirement)
        {
            issues.push(issue(
                "scenario_capability_unavailable",
                format!(
                    "runner for platform '{}' does not advertise scenario capability '{}'",
                    platform.label(),
                    requirement
                ),
            ));
        }
    }

    match context.toolchains.get(&platform) {
        Some(report) if report.ready() => {}
        Some(report) => issues.push(issue(
            "toolchain_unavailable",
            format!(
                "required toolchain checks did not pass: {}",
                report.blocking_checks().join("; ")
            ),
        )),
        None => issues.push(issue(
            "toolchain_not_checked",
            format!(
                "toolchain admission was not run for platform '{}'",
                platform.label()
            ),
        )),
    }
    issues
}

fn issue(code: impl Into<String>, message: impl Into<String>) -> MatrixAdmissionIssue {
    MatrixAdmissionIssue {
        code: code.into(),
        message: message.into(),
    }
}

fn android_abi_supported(abi: &str) -> bool {
    matches!(abi, "arm64-v8a" | "armeabi-v7a" | "x86" | "x86_64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::matrix::{MatrixCellState, MatrixStatus};
    use crate::scenario::{ScenarioStep, Viewport};
    use serde_json::json;
    use std::time::Instant;

    fn scenarios(requires: Vec<&str>) -> ScenarioFile {
        ScenarioFile {
            schema_version: scenario::SCHEMA_VERSION,
            scenarios: vec![ScenarioDefinition {
                id: "counter-basic".into(),
                component: "Counter".into(),
                fixture: "fixtures/counter.json".into(),
                tags: Vec::new(),
                timeout_ms: 30_000,
                requires: requires.into_iter().map(str::to_owned).collect(),
                theme: "light".into(),
                locale: "en-US".into(),
                random_seed: None,
                clock: "real".into(),
                clock_at: None,
                viewport: Viewport {
                    width: 640,
                    height: 480,
                    scale: None,
                },
                ready_id: None,
                steps: vec![ScenarioStep {
                    id: "initial".into(),
                    kind: "assert".into(),
                    selector: None,
                    assertion: Some("exists".into()),
                    expected: Some(json!(true)),
                    label: None,
                    require: Vec::new(),
                    baseline_id: None,
                    text: None,
                    mode: "replace".into(),
                    button: "primary".into(),
                    key: None,
                    delta_x: 0,
                    delta_y: 0,
                    duration_ms: 0,
                    timeout_ms: None,
                }],
            }],
        }
    }

    fn ready_report(target: ToolchainTarget) -> MatrixToolchainReport {
        MatrixToolchainReport {
            target,
            checks: vec![CheckReport {
                id: "toolchain.ready".into(),
                required: true,
                status: CheckStatus::Pass,
                expected: json!({}),
                actual: json!({}),
                reason: "ready".into(),
                remediation: Vec::new(),
                duration_ms: 0,
                command: None,
            }],
        }
    }

    fn local_context(platform: MatrixPlatform) -> MatrixAdmissionContext {
        let mut context = MatrixAdmissionContext::for_local("macos", vec!["macos".into()]);
        let runner = context.runners.get_mut("local").unwrap();
        runner.platforms = [platform].into_iter().collect();
        if platform == MatrixPlatform::Android {
            runner.devices.insert(
                "emulator-1".into(),
                MatrixDeviceAvailability {
                    available: true,
                    arch: Some("x86_64".into()),
                },
            );
        }
        context = context.with_toolchain(platform, ready_report(platform.toolchain_target()));
        context
    }

    #[test]
    fn expands_targets_into_deterministic_cells_and_keeps_scenario_hash() {
        let matrix: MatrixFile = toml::from_str(
            r#"
                schema_version = 1
                max_parallel = 2
                [[targets]]
                id = "macos-local"
                runner = "local"
                platform = "macos"
                scenarios = ["counter-basic"]
                timeout_ms = 60000
            "#,
        )
        .unwrap();
        let admission = admit_matrix(
            matrix,
            scenarios(vec!["capture.window"]),
            Some("sha256:scenario".into()),
            &local_context(MatrixPlatform::Macos),
        )
        .unwrap();
        assert_eq!(admission.plan.config.timeout_ms, 60_000);
        assert_eq!(
            admission.plan.cells[0].cell_id,
            "macos-local::counter-basic"
        );
        assert_eq!(admission.cells[0].state, MatrixAdmissionState::Ready);
        assert_eq!(admission.scenario_hash.as_deref(), Some("sha256:scenario"));
    }

    #[test]
    fn local_windows_cell_is_retained_as_unavailable() {
        let matrix: MatrixFile = toml::from_str(
            r#"
                schema_version = 1
                [[targets]]
                id = "windows-local"
                runner = "local"
                platform = "windows"
                required = false
                scenarios = ["counter-basic"]
            "#,
        )
        .unwrap();
        let admission = admit_matrix(
            matrix,
            scenarios(vec!["capture.window"]),
            None,
            &local_context(MatrixPlatform::Macos),
        )
        .unwrap();
        assert_eq!(admission.cells[0].state, MatrixAdmissionState::Unavailable);
        assert!(
            admission.cells[0]
                .issues
                .iter()
                .any(|issue| issue.code == "runner_platform_unavailable")
        );
    }

    #[test]
    fn android_abi_mismatch_is_admission_unavailable() {
        let matrix: MatrixFile = toml::from_str(
            r#"
                schema_version = 1
                [[targets]]
                id = "android-emulator"
                runner = "local"
                platform = "android"
                device = "emulator-1"
                abi = "arm64-v8a"
                scenarios = ["counter-basic"]
            "#,
        )
        .unwrap();
        let admission = admit_matrix(
            matrix,
            scenarios(vec!["capture.device"]),
            None,
            &local_context(MatrixPlatform::Android),
        )
        .unwrap();
        assert_eq!(admission.cells[0].state, MatrixAdmissionState::Unavailable);
        assert_eq!(
            admission.plan.cells[0].resource_ids,
            vec!["device:local:emulator-1"]
        );
        assert_eq!(admission.cells[0].issues[0].code, "target_not_declared");
        assert!(
            admission.cells[0]
                .issues
                .iter()
                .any(|issue| issue.code == "abi_mismatch")
        );
    }

    #[test]
    fn desktop_device_capture_requirement_is_unavailable() {
        let matrix: MatrixFile = toml::from_str(
            r#"
                schema_version = 1
                [[targets]]
                id = "macos-local"
                runner = "local"
                platform = "macos"
                scenarios = ["counter-basic"]
            "#,
        )
        .unwrap();
        let admission = admit_matrix(
            matrix,
            scenarios(vec!["capture.device"]),
            None,
            &local_context(MatrixPlatform::Macos),
        )
        .unwrap();
        assert!(
            admission.cells[0]
                .issues
                .iter()
                .any(|issue| issue.code == "scenario_requirement_unsupported")
        );
    }

    #[test]
    fn unsupported_mobile_scene_requirement_is_unavailable() {
        let matrix: MatrixFile = toml::from_str(
            r#"
                schema_version = 1
                [[targets]]
                id = "android-emulator"
                runner = "local"
                platform = "android"
                device = "emulator-1"
                abi = "x86_64"
                scenarios = ["counter-basic"]
            "#,
        )
        .unwrap();
        let admission = admit_matrix(
            matrix,
            scenarios(vec!["capture.scene", "capture.device"]),
            None,
            &local_context(MatrixPlatform::Android),
        )
        .unwrap();
        assert!(
            admission.cells[0]
                .issues
                .iter()
                .any(|issue| issue.code == "scenario_capability_unavailable")
        );
    }

    #[test]
    fn admission_marks_unavailable_cells_in_scheduler_report() {
        let matrix: MatrixFile = toml::from_str(
            r#"
                schema_version = 1
                [[targets]]
                id = "windows-local"
                runner = "local"
                platform = "windows"
                required = false
                scenarios = ["counter-basic"]
            "#,
        )
        .unwrap();
        let admission = admit_matrix(
            matrix,
            scenarios(vec!["capture.window"]),
            None,
            &local_context(MatrixPlatform::Macos),
        )
        .unwrap();
        let mut scheduler = MatrixScheduler::new(admission.plan.clone(), Instant::now()).unwrap();
        admission.apply(&mut scheduler).unwrap();
        let report = scheduler.report().unwrap();
        assert_eq!(report.cells[0].status, MatrixCellState::Unavailable);
        assert_eq!(report.status, MatrixStatus::Partial);
    }

    #[test]
    fn unsupported_android_abi_fails_before_admission() {
        let matrix: MatrixFile = toml::from_str(
            r#"
                schema_version = 1
                [[targets]]
                id = "android"
                runner = "local"
                platform = "android"
                device = "emulator-1"
                abi = "mips"
                scenarios = ["counter-basic"]
            "#,
        )
        .unwrap();
        let error = admit_matrix(
            matrix,
            scenarios(vec!["capture.device"]),
            None,
            &local_context(MatrixPlatform::Android),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsupported ABI"));
    }
}
