//! Runtime-agnostic scenario check execution and assertion semantics.
//!
//! This module owns the bounded check state machine, but deliberately does not
//! know how a GPUI process is launched or how a platform captures a window.
//! A platform runner implements [`ScenarioRunner`] and supplies observations,
//! normal input results, and capture evidence. Keeping those boundaries here
//! makes an unavailable or uncertain runtime result impossible to turn into a
//! passing check by accident.

use super::baseline::BaselineKey;
use super::{ScenarioDefinition, ScenarioStep, Selector};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Final status of a scenario check.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    Inconclusive,
    Unavailable,
    Cancelled,
}

/// Status recorded for an individual scenario step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
    Inconclusive,
    Unavailable,
    Cancelled,
    Skipped,
}

/// Result of delivering a normal GPUI input action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Succeeded,
    Failed,
    Unknown,
    Cancelled,
}

/// Error class returned by an injected runtime driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverErrorKind {
    Failed,
    Unavailable,
    Unknown,
    Cancelled,
    Timeout,
}

/// A bounded runtime error with a stable machine-readable code.
#[derive(Clone, Debug)]
pub struct DriverError {
    pub kind: DriverErrorKind,
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl DriverError {
    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Failed, code, message)
    }

    pub fn unavailable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Unavailable, code, message)
    }

    pub fn unknown(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Unknown, code, message)
    }

    pub fn cancelled(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Cancelled, code, message)
    }

    pub fn timeout(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Timeout, code, message)
    }

    fn new(kind: DriverErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    fn as_check_error(&self) -> CheckError {
        CheckError {
            code: self.code.clone(),
            message: self.message.clone(),
            details: self.details.clone(),
        }
    }
}

/// Error information attached to a step or the primary report outcome.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CheckError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// A bounded semantic node from one immutable observation.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SemanticNode {
    pub logical_id: Option<String>,
    pub role: Option<String>,
    pub name: Option<String>,
    pub value: Option<Value>,
    pub text: Option<String>,
    pub enabled: Option<bool>,
    pub focused: Option<bool>,
    pub visible: Option<bool>,
    pub clipped: Option<bool>,
}

/// Evidence produced by a screenshot provider for a scenario observation.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ScreenshotEvidence {
    pub artifact_id: Option<String>,
    pub baseline_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_key: Option<BaselineKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffEvidence>,
    pub scope: Option<String>,
    pub provider: Option<String>,
    pub pixel_width: Option<u32>,
    pub pixel_height: Option<u32>,
    pub logical_width: Option<u32>,
    pub logical_height: Option<u32>,
    pub scale_milli: Option<u32>,
    pub comparable: bool,
    pub matches: Option<bool>,
    pub reason: Option<String>,
}

/// A local diagnostic diff written by a check after a visual mismatch.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DiffEvidence {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub changed_pixels: u64,
    pub total_pixels: u64,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// The smallest observation contract needed by the check core.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Observation {
    pub observation_id: String,
    pub window_id: Option<String>,
    pub revision: Option<String>,
    /// `None` means semantics were not available. An empty vector is a valid,
    /// bounded semantics tree with no matching nodes.
    pub nodes: Option<Vec<SemanticNode>>,
    /// `None` means runtime error collection was unavailable.
    pub runtime_errors: Option<Vec<Value>>,
    pub screenshot: Option<ScreenshotEvidence>,
    pub log_seq: Option<u64>,
}

/// Action evidence stored in the per-step report.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ActionEvidence {
    pub status: ActionStatus,
    pub operation_id: Option<String>,
}

/// Evidence stored when a capture provider publishes an artifact.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CaptureEvidence {
    pub artifact_id: Option<String>,
    pub kinds: Vec<String>,
}

/// Result returned by a driver after an input request reaches its transport
/// boundary. `Unknown` is intentionally distinct from `Failed`.
#[derive(Clone, Debug)]
pub struct ActionResult {
    pub status: ActionStatus,
    pub operation_id: Option<String>,
    pub error: Option<CheckError>,
}

impl ActionResult {
    pub fn succeeded(operation_id: impl Into<String>) -> Self {
        Self {
            status: ActionStatus::Succeeded,
            operation_id: Some(operation_id.into()),
            error: None,
        }
    }

    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: ActionStatus::Failed,
            operation_id: None,
            error: Some(CheckError {
                code: code.into(),
                message: message.into(),
                details: None,
            }),
        }
    }

    pub fn unknown(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: ActionStatus::Unknown,
            operation_id: None,
            error: Some(CheckError {
                code: code.into(),
                message: message.into(),
                details: None,
            }),
        }
    }
}

/// The normalized result of one assertion evaluation.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AssertionEvaluation {
    pub assertion: String,
    pub status: StepStatus,
    pub message: String,
    pub actual: Option<Value>,
}

/// One durable check step record. Observation ids and log sequence numbers
/// make the report useful even when a later step fails.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct StepReport {
    pub id: String,
    pub kind: String,
    pub status: StepStatus,
    pub duration_ms: u64,
    pub before_observation_id: Option<String>,
    pub after_observation_id: Option<String>,
    pub before_log_seq: Option<u64>,
    pub after_log_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion: Option<AssertionEvaluation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CheckError>,
}

/// Cleanup is reported separately so a cleanup failure cannot overwrite the
/// original failed/unknown step and make diagnosis ambiguous.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CleanupReport {
    pub attempted: bool,
    pub succeeded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CheckError>,
}

/// Structured result of one scenario execution.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CheckReport {
    pub schema_version: u32,
    pub scenario_id: String,
    pub component: String,
    pub fixture_hash: Option<String>,
    pub status: CheckStatus,
    pub steps: Vec<StepReport>,
    pub primary_error: Option<CheckError>,
    pub cleanup: CleanupReport,
}

/// A validated, immutable execution plan. Validation of the full TOML schema
/// remains the responsibility of [`crate::scenario::validate_file`]. This
/// extra boundary protects future callers that construct a definition in code.
#[derive(Clone, Debug)]
pub struct CheckPlan {
    pub scenario_id: String,
    pub component: String,
    pub timeout: Duration,
    pub steps: Vec<ScenarioStep>,
}

impl CheckPlan {
    pub fn from_scenario(scenario: &ScenarioDefinition) -> Result<Self, CheckError> {
        if scenario.id.is_empty() || scenario.component.is_empty() {
            return Err(CheckError::new(
                "invalid_check_plan",
                "scenario id and component are required",
            ));
        }
        if scenario.timeout_ms == 0 {
            return Err(CheckError::new(
                "invalid_check_plan",
                "scenario timeout must be greater than zero",
            ));
        }
        if scenario.steps.is_empty() {
            return Err(CheckError::new(
                "invalid_check_plan",
                "scenario must contain at least one step",
            ));
        }
        Ok(Self {
            scenario_id: scenario.id.clone(),
            component: scenario.component.clone(),
            timeout: Duration::from_millis(scenario.timeout_ms),
            steps: scenario.steps.clone(),
        })
    }
}

impl CheckError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
}

/// Platform-specific execution boundary. Implementations must enforce the
/// supplied deadline; the core never sleeps for a fixed duration to infer
/// readiness.
pub trait ScenarioRunner {
    fn prepare(
        &mut self,
        scenario: &ScenarioDefinition,
        deadline: Instant,
    ) -> Result<Observation, DriverError>;

    fn observe(&mut self, deadline: Instant) -> Result<Observation, DriverError>;

    /// Resolve the baseline-specific evidence needed by a screenshot
    /// assertion. Most runners can keep the observation unchanged; a runner
    /// with a real artifact store may load and compare the referenced PNG.
    fn resolve_screenshot(
        &mut self,
        _step: &ScenarioStep,
        observation: &Observation,
        _deadline: Instant,
    ) -> Result<Observation, DriverError> {
        Ok(observation.clone())
    }

    fn act(
        &mut self,
        step: &ScenarioStep,
        before: &Observation,
        deadline: Instant,
    ) -> Result<ActionResult, DriverError>;

    fn wait_for(
        &mut self,
        step: &ScenarioStep,
        before: &Observation,
        deadline: Instant,
    ) -> Result<Observation, DriverError>;

    fn capture(
        &mut self,
        step: &ScenarioStep,
        observation: &Observation,
        deadline: Instant,
    ) -> Result<CaptureEvidence, DriverError>;

    fn cleanup(&mut self) -> Result<(), DriverError>;
}

/// Execute a scenario through an injected runner and always attempt cleanup.
pub fn execute<R: ScenarioRunner>(
    runner: &mut R,
    scenario: &ScenarioDefinition,
    fixture_hash: Option<String>,
) -> CheckReport {
    let plan = match CheckPlan::from_scenario(scenario) {
        Ok(plan) => plan,
        Err(error) => {
            let mut report = empty_report(scenario, fixture_hash, CheckStatus::Failed);
            report.primary_error = Some(error);
            report.cleanup = cleanup_report(runner.cleanup());
            return report;
        }
    };
    let mut report = empty_report(scenario, fixture_hash, CheckStatus::Passed);
    let deadline = Instant::now() + plan.timeout;
    let mut current = match runner.prepare(scenario, deadline) {
        Ok(observation) => observation,
        Err(error) => {
            set_driver_failure(&mut report, &error);
            report.cleanup = cleanup_report(runner.cleanup());
            return report;
        }
    };

    for (index, step) in plan.steps.iter().enumerate() {
        if report.status != CheckStatus::Passed {
            report.steps.extend(
                plan.steps[index..]
                    .iter()
                    .map(|skipped| skipped_step(skipped, &current, &report.status)),
            );
            break;
        }

        let started = Instant::now();
        let before = current.clone();
        let result = execute_step(runner, step, &before, deadline);
        let (step_report, next_observation) = finish_step(step, &before, started, result);
        let step_status = step_report.status;
        if let Some(observation) = next_observation {
            current = observation;
        }
        if step_status != StepStatus::Passed {
            report.status = status_for_step(step_status);
            report.primary_error = step_report.error.clone().or_else(|| {
                Some(CheckError::new(
                    "step_failed",
                    format!("scenario step `{}` did not pass", step.id),
                ))
            });
        }
        report.steps.push(step_report);
    }

    report.cleanup = cleanup_report(runner.cleanup());
    if !report.cleanup.succeeded && report.status == CheckStatus::Passed {
        report.status = CheckStatus::Failed;
        report.primary_error = report.cleanup.error.clone();
    }
    report
}

fn empty_report(
    scenario: &ScenarioDefinition,
    fixture_hash: Option<String>,
    status: CheckStatus,
) -> CheckReport {
    CheckReport {
        schema_version: 1,
        scenario_id: scenario.id.clone(),
        component: scenario.component.clone(),
        fixture_hash,
        status,
        steps: Vec::new(),
        primary_error: None,
        cleanup: CleanupReport {
            attempted: false,
            succeeded: false,
            error: None,
        },
    }
}

fn cleanup_report(result: Result<(), DriverError>) -> CleanupReport {
    match result {
        Ok(()) => CleanupReport {
            attempted: true,
            succeeded: true,
            error: None,
        },
        Err(error) => CleanupReport {
            attempted: true,
            succeeded: false,
            error: Some(error.as_check_error()),
        },
    }
}

fn set_driver_failure(report: &mut CheckReport, error: &DriverError) {
    report.status = status_for_driver(error.kind);
    report.primary_error = Some(error.as_check_error());
}

fn status_for_driver(kind: DriverErrorKind) -> CheckStatus {
    match kind {
        DriverErrorKind::Failed | DriverErrorKind::Timeout => CheckStatus::Failed,
        DriverErrorKind::Unavailable => CheckStatus::Unavailable,
        DriverErrorKind::Unknown => CheckStatus::Inconclusive,
        DriverErrorKind::Cancelled => CheckStatus::Cancelled,
    }
}

fn step_status_for_driver(kind: DriverErrorKind) -> StepStatus {
    match kind {
        DriverErrorKind::Failed | DriverErrorKind::Timeout => StepStatus::Failed,
        DriverErrorKind::Unavailable => StepStatus::Unavailable,
        DriverErrorKind::Unknown => StepStatus::Inconclusive,
        DriverErrorKind::Cancelled => StepStatus::Cancelled,
    }
}

fn status_for_step(status: StepStatus) -> CheckStatus {
    match status {
        StepStatus::Passed => CheckStatus::Passed,
        StepStatus::Failed => CheckStatus::Failed,
        StepStatus::Inconclusive => CheckStatus::Inconclusive,
        StepStatus::Unavailable => CheckStatus::Unavailable,
        StepStatus::Cancelled => CheckStatus::Cancelled,
        StepStatus::Skipped => CheckStatus::Failed,
    }
}

fn skipped_step(
    step: &ScenarioStep,
    observation: &Observation,
    status: &CheckStatus,
) -> StepReport {
    StepReport {
        id: step.id.clone(),
        kind: step.kind.clone(),
        status: StepStatus::Skipped,
        duration_ms: 0,
        before_observation_id: Some(observation.observation_id.clone()),
        after_observation_id: None,
        before_log_seq: observation.log_seq,
        after_log_seq: None,
        action: None,
        assertion: None,
        capture: None,
        error: Some(CheckError::new(
            "skipped_after_terminal_step",
            format!("step skipped after scenario became {status:?}"),
        )),
    }
}

enum StepResult {
    Action {
        result: ActionResult,
        after: Observation,
        observation_error: Option<DriverError>,
    },
    Assertion(AssertionEvaluation, Observation),
    Capture(CaptureEvidence, Observation),
}

fn execute_step<R: ScenarioRunner>(
    runner: &mut R,
    step: &ScenarioStep,
    before: &Observation,
    deadline: Instant,
) -> Result<StepResult, DriverError> {
    match step.kind.as_str() {
        "click" | "type_text" | "key" | "scroll" => {
            let action = runner.act(step, before, deadline)?;
            if action.status != ActionStatus::Succeeded {
                return Ok(StepResult::Action {
                    result: action,
                    after: before.clone(),
                    observation_error: None,
                });
            }
            match runner.observe(deadline) {
                Ok(after) => Ok(StepResult::Action {
                    result: action,
                    after,
                    observation_error: None,
                }),
                Err(error) => Ok(StepResult::Action {
                    result: action,
                    after: before.clone(),
                    observation_error: Some(error),
                }),
            }
        }
        "wait_for" => {
            let after = runner.wait_for(step, before, deadline)?;
            let assertion = evaluate_assertion(
                step.assertion.as_deref().unwrap_or(""),
                step.selector.as_ref(),
                step.expected.as_ref(),
                &after,
            );
            Ok(StepResult::Assertion(assertion, after))
        }
        "assert" => {
            let assertion_observation = if step.assertion.as_deref() == Some("screenshot_matches") {
                runner.resolve_screenshot(step, before, deadline)?
            } else {
                before.clone()
            };
            let assertion = evaluate_assertion(
                step.assertion.as_deref().unwrap_or(""),
                step.selector.as_ref(),
                step.expected.as_ref(),
                &assertion_observation,
            );
            Ok(StepResult::Assertion(assertion, assertion_observation))
        }
        "capture" => {
            let capture = runner.capture(step, before, deadline)?;
            Ok(StepResult::Capture(capture, before.clone()))
        }
        other => Err(DriverError::failed(
            "unsupported_step",
            format!("scenario step type `{other}` is not executable"),
        )),
    }
}

fn finish_step(
    step: &ScenarioStep,
    before: &Observation,
    started: Instant,
    result: Result<StepResult, DriverError>,
) -> (StepReport, Option<Observation>) {
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let base = |status, after: &Observation, error, action, assertion, capture| StepReport {
        id: step.id.clone(),
        kind: step.kind.clone(),
        status,
        duration_ms,
        before_observation_id: Some(before.observation_id.clone()),
        after_observation_id: Some(after.observation_id.clone()),
        before_log_seq: before.log_seq,
        after_log_seq: after.log_seq,
        action,
        assertion,
        capture,
        error,
    };

    match result {
        Ok(StepResult::Action {
            result: action,
            after,
            observation_error,
        }) => {
            let (status, error) = if let Some(error) = observation_error {
                (
                    step_status_for_driver(error.kind),
                    Some(error.as_check_error()),
                )
            } else {
                match action.status {
                    ActionStatus::Succeeded => (StepStatus::Passed, None),
                    ActionStatus::Failed => (
                        StepStatus::Failed,
                        action.error.clone().or_else(|| {
                            Some(CheckError::new("action_failed", "input action failed"))
                        }),
                    ),
                    ActionStatus::Unknown => (
                        StepStatus::Inconclusive,
                        action.error.clone().or_else(|| {
                            Some(CheckError::new(
                                "action_outcome_unknown",
                                "input action was delivered but its outcome is unknown",
                            ))
                        }),
                    ),
                    ActionStatus::Cancelled => (
                        StepStatus::Cancelled,
                        action.error.clone().or_else(|| {
                            Some(CheckError::new(
                                "action_cancelled",
                                "input action was cancelled",
                            ))
                        }),
                    ),
                }
            };
            (
                base(
                    status,
                    &after,
                    error,
                    Some(ActionEvidence {
                        status: action.status,
                        operation_id: action.operation_id,
                    }),
                    None,
                    None,
                ),
                (status == StepStatus::Passed).then_some(after),
            )
        }
        Ok(StepResult::Assertion(assertion, after)) => {
            let status = assertion.status;
            let error = (status != StepStatus::Passed)
                .then(|| CheckError::new("assertion_failed", assertion.message.clone()));
            (
                base(status, &after, error, None, Some(assertion), None),
                (status == StepStatus::Passed).then_some(after),
            )
        }
        Ok(StepResult::Capture(capture, after)) => (
            base(StepStatus::Passed, &after, None, None, None, Some(capture)),
            Some(after),
        ),
        Err(error) => {
            let status = step_status_for_driver(error.kind);
            (
                base(
                    status,
                    before,
                    Some(error.as_check_error()),
                    None,
                    None,
                    None,
                ),
                None,
            )
        }
    }
}

/// Evaluate one schema-v1 assertion against one immutable observation.
pub fn evaluate_assertion(
    assertion: &str,
    selector: Option<&Selector>,
    expected: Option<&Value>,
    observation: &Observation,
) -> AssertionEvaluation {
    if assertion == "no_runtime_errors" {
        return match observation.runtime_errors.as_ref() {
            None => inconclusive(assertion, "runtime error evidence is unavailable", None),
            Some(errors) if errors.is_empty() => {
                passed(assertion, "no new runtime errors", Some(json!([])))
            }
            Some(errors) => failed(
                assertion,
                "unexpected runtime errors were recorded",
                Some(json!(errors)),
            ),
        };
    }
    if assertion == "screenshot_matches" {
        return match observation.screenshot.as_ref() {
            None => inconclusive(assertion, "screenshot evidence is unavailable", None),
            Some(evidence) if !evidence.comparable => inconclusive(
                assertion,
                evidence
                    .reason
                    .as_deref()
                    .unwrap_or("screenshot is not comparable"),
                Some(screenshot_actual(evidence)),
            ),
            Some(evidence) => match evidence.matches {
                Some(true) => passed(
                    assertion,
                    "screenshot matches baseline",
                    Some(screenshot_actual(evidence)),
                ),
                Some(false) => failed(
                    assertion,
                    "screenshot differs from baseline",
                    Some(screenshot_actual(evidence)),
                ),
                None => inconclusive(
                    assertion,
                    "screenshot comparison has no result",
                    Some(screenshot_actual(evidence)),
                ),
            },
        };
    }

    let Some(nodes) = observation.nodes.as_ref() else {
        return inconclusive(assertion, "semantic evidence is unavailable", None);
    };
    let Some(selector) = selector else {
        return failed(assertion, "assertion requires a selector", None);
    };
    let matches = nodes
        .iter()
        .filter(|node| node_matches(node, selector))
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return failed(
            assertion,
            "selector matched more than one semantic node",
            Some(json!({"matches": matches.len()})),
        );
    }
    if assertion == "absent" {
        return if matches.is_empty() {
            passed(assertion, "selector is absent", Some(json!(false)))
        } else {
            failed(assertion, "selector is present", Some(json!(true)))
        };
    }
    let Some(node) = matches.first() else {
        return failed(
            assertion,
            "selector did not match a semantic node",
            Some(json!(false)),
        );
    };

    match assertion {
        "exists" => passed(assertion, "selector exists", Some(json!(true))),
        "value_equals" => compare_value(assertion, node.value.as_ref(), expected),
        "text_equals" => compare_value(
            assertion,
            node.text
                .as_ref()
                .map(|text| Value::String(text.clone()))
                .as_ref(),
            expected,
        ),
        "enabled" => compare_bool(assertion, node.enabled),
        "focused" => compare_bool(assertion, node.focused),
        "visible" => compare_bool(assertion, node.visible),
        "not_clipped" => match node.clipped {
            Some(false) => passed(assertion, "node is not clipped", Some(json!(true))),
            Some(true) => failed(assertion, "node is clipped", Some(json!(false))),
            None => inconclusive(assertion, "clip state is unavailable", None),
        },
        _ => failed(
            assertion,
            "assertion is not supported by the check core",
            None,
        ),
    }
}

fn node_matches(node: &SemanticNode, selector: &Selector) -> bool {
    match (&selector.logical_id, &selector.role, &selector.name) {
        (Some(logical_id), None, None) => node.logical_id.as_ref() == Some(logical_id),
        (None, Some(role), Some(name)) => {
            node.role.as_ref() == Some(role) && node.name.as_ref() == Some(name)
        }
        _ => false,
    }
}

fn compare_value(
    assertion: &str,
    actual: Option<&Value>,
    expected: Option<&Value>,
) -> AssertionEvaluation {
    let Some(actual) = actual else {
        return inconclusive(assertion, "asserted value is unavailable", None);
    };
    let Some(expected) = expected else {
        return failed(
            assertion,
            "assertion expected value is missing",
            Some(actual.clone()),
        );
    };
    if actual == expected {
        passed(assertion, "asserted value matches", Some(actual.clone()))
    } else {
        failed(
            assertion,
            "asserted value does not match",
            Some(actual.clone()),
        )
    }
}

fn compare_bool(assertion: &str, actual: Option<bool>) -> AssertionEvaluation {
    match actual {
        Some(true) => passed(assertion, "semantic flag is true", Some(json!(true))),
        Some(false) => failed(assertion, "semantic flag is false", Some(json!(false))),
        None => inconclusive(assertion, "semantic flag is unavailable", None),
    }
}

fn screenshot_actual(evidence: &ScreenshotEvidence) -> Value {
    json!({
        "artifact_id": evidence.artifact_id,
        "baseline_id": evidence.baseline_id,
        "baseline_key": evidence.baseline_key,
        "diff": evidence.diff,
        "scope": evidence.scope,
        "provider": evidence.provider,
        "pixel_width": evidence.pixel_width,
        "pixel_height": evidence.pixel_height,
        "logical_width": evidence.logical_width,
        "logical_height": evidence.logical_height,
        "scale_milli": evidence.scale_milli,
        "comparable": evidence.comparable,
        "matches": evidence.matches,
        "reason": evidence.reason,
    })
}

fn passed(assertion: &str, message: &str, actual: Option<Value>) -> AssertionEvaluation {
    AssertionEvaluation {
        assertion: assertion.into(),
        status: StepStatus::Passed,
        message: message.into(),
        actual,
    }
}

fn failed(assertion: &str, message: &str, actual: Option<Value>) -> AssertionEvaluation {
    AssertionEvaluation {
        assertion: assertion.into(),
        status: StepStatus::Failed,
        message: message.into(),
        actual,
    }
}

fn inconclusive(assertion: &str, message: &str, actual: Option<Value>) -> AssertionEvaluation {
    AssertionEvaluation {
        assertion: assertion.into(),
        status: StepStatus::Inconclusive,
        message: message.into(),
        actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{ScenarioDefinition, ScenarioStep, Selector, Viewport};
    use std::collections::VecDeque;

    fn selector(logical_id: &str) -> Selector {
        Selector {
            logical_id: Some(logical_id.into()),
            role: None,
            name: None,
        }
    }

    fn node(logical_id: &str, value: Value) -> SemanticNode {
        SemanticNode {
            logical_id: Some(logical_id.into()),
            role: None,
            name: None,
            value: Some(value),
            text: None,
            enabled: Some(true),
            focused: None,
            visible: Some(true),
            clipped: Some(false),
        }
    }

    fn observation(id: &str, nodes: Vec<SemanticNode>) -> Observation {
        Observation {
            observation_id: id.into(),
            window_id: Some("window-1".into()),
            revision: Some(id.into()),
            nodes: Some(nodes),
            runtime_errors: Some(Vec::new()),
            screenshot: None,
            log_seq: Some(10),
        }
    }

    fn scenario(steps: Vec<ScenarioStep>) -> ScenarioDefinition {
        ScenarioDefinition {
            id: "counter-check".into(),
            component: "Counter".into(),
            fixture: "fixture.json".into(),
            tags: Vec::new(),
            timeout_ms: 500,
            requires: Vec::new(),
            theme: "light".into(),
            locale: "en-US".into(),
            random_seed: Some(7),
            clock: "fixed".into(),
            clock_at: Some("2026-09-21T06:00:00Z".into()),
            viewport: Viewport {
                width: 640,
                height: 480,
                scale: None,
            },
            ready_id: Some("counter.value".into()),
            steps,
        }
    }

    fn assert_step(id: &str, assertion: &str, expected: Option<Value>) -> ScenarioStep {
        ScenarioStep {
            id: id.into(),
            kind: "assert".into(),
            selector: Some(selector("counter.value")),
            assertion: Some(assertion.into()),
            expected,
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
        }
    }

    #[derive(Default)]
    struct FakeRunner {
        prepared: Option<Result<Observation, DriverError>>,
        observations: VecDeque<Result<Observation, DriverError>>,
        actions: VecDeque<Result<ActionResult, DriverError>>,
        waits: VecDeque<Result<Observation, DriverError>>,
        captures: VecDeque<Result<CaptureEvidence, DriverError>>,
        cleanup: Option<Result<(), DriverError>>,
        action_calls: usize,
        screenshot_resolutions: usize,
    }

    impl ScenarioRunner for FakeRunner {
        fn prepare(
            &mut self,
            _scenario: &ScenarioDefinition,
            _deadline: Instant,
        ) -> Result<Observation, DriverError> {
            self.prepared
                .take()
                .unwrap_or_else(|| Err(DriverError::unavailable("not_configured", "fake prepare")))
        }

        fn observe(&mut self, _deadline: Instant) -> Result<Observation, DriverError> {
            self.observations
                .pop_front()
                .unwrap_or_else(|| Err(DriverError::unknown("missing_observation", "fake observe")))
        }

        fn resolve_screenshot(
            &mut self,
            _step: &ScenarioStep,
            observation: &Observation,
            _deadline: Instant,
        ) -> Result<Observation, DriverError> {
            self.screenshot_resolutions += 1;
            let mut resolved = observation.clone();
            resolved.screenshot = Some(ScreenshotEvidence {
                artifact_id: Some("artifact-1".into()),
                baseline_id: Some("counter".into()),
                baseline_key: None,
                diff: None,
                scope: Some("window".into()),
                provider: Some("test".into()),
                pixel_width: Some(640),
                pixel_height: Some(480),
                logical_width: Some(640),
                logical_height: Some(480),
                scale_milli: Some(1000),
                comparable: true,
                matches: Some(true),
                reason: None,
            });
            Ok(resolved)
        }

        fn act(
            &mut self,
            _step: &ScenarioStep,
            _before: &Observation,
            _deadline: Instant,
        ) -> Result<ActionResult, DriverError> {
            self.action_calls += 1;
            self.actions
                .pop_front()
                .unwrap_or_else(|| Err(DriverError::unknown("missing_action", "fake action")))
        }

        fn wait_for(
            &mut self,
            _step: &ScenarioStep,
            _before: &Observation,
            _deadline: Instant,
        ) -> Result<Observation, DriverError> {
            self.waits
                .pop_front()
                .unwrap_or_else(|| Err(DriverError::timeout("wait_timeout", "fake wait")))
        }

        fn capture(
            &mut self,
            _step: &ScenarioStep,
            _observation: &Observation,
            _deadline: Instant,
        ) -> Result<CaptureEvidence, DriverError> {
            self.captures
                .pop_front()
                .unwrap_or_else(|| Err(DriverError::unavailable("missing_capture", "fake capture")))
        }

        fn cleanup(&mut self) -> Result<(), DriverError> {
            self.cleanup.take().unwrap_or(Ok(()))
        }
    }

    #[test]
    fn assertions_distinguish_missing_semantics_from_absent_nodes() {
        let mut unavailable = observation("o-1", Vec::new());
        unavailable.nodes = None;
        assert_eq!(
            evaluate_assertion("absent", Some(&selector("missing")), None, &unavailable).status,
            StepStatus::Inconclusive
        );
        assert_eq!(
            evaluate_assertion(
                "absent",
                Some(&selector("missing")),
                None,
                &observation("o-2", Vec::new())
            )
            .status,
            StepStatus::Passed
        );
    }

    #[test]
    fn assertions_report_unknown_fields_as_inconclusive() {
        let mut current = observation("o-1", vec![node("counter.value", json!(0))]);
        current.nodes.as_mut().unwrap()[0].enabled = None;
        assert_eq!(
            evaluate_assertion("enabled", Some(&selector("counter.value")), None, &current).status,
            StepStatus::Inconclusive
        );
        current.runtime_errors = None;
        assert_eq!(
            evaluate_assertion("no_runtime_errors", None, None, &current).status,
            StepStatus::Inconclusive
        );
    }

    #[test]
    fn execute_records_before_after_and_stops_after_unknown_action() {
        let action = ScenarioStep {
            id: "click".into(),
            kind: "click".into(),
            selector: Some(selector("counter.value")),
            assertion: None,
            expected: None,
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
        };
        let later = assert_step("later", "value_equals", Some(json!(1)));
        let mut runner = FakeRunner {
            prepared: Some(Ok(observation(
                "o-1",
                vec![node("counter.value", json!(0))],
            ))),
            actions: VecDeque::from([Ok(ActionResult::unknown(
                "action_outcome_unknown",
                "delivered but not confirmed",
            ))]),
            cleanup: Some(Err(DriverError::failed(
                "cleanup_failed",
                "fixture cleanup failed",
            ))),
            ..FakeRunner::default()
        };
        let report = execute(
            &mut runner,
            &scenario(vec![action, later]),
            Some("sha256:fixture".into()),
        );
        assert_eq!(report.status, CheckStatus::Inconclusive);
        assert_eq!(report.steps.len(), 2);
        assert_eq!(report.steps[0].status, StepStatus::Inconclusive);
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert_eq!(
            report.primary_error.as_ref().unwrap().code,
            "action_outcome_unknown"
        );
        assert!(!report.cleanup.succeeded);
        assert_eq!(runner.action_calls, 1);
    }

    #[test]
    fn cleanup_failure_changes_only_an_otherwise_passing_report() {
        let mut runner = FakeRunner {
            prepared: Some(Ok(observation(
                "o-1",
                vec![node("counter.value", json!(0))],
            ))),
            cleanup: Some(Err(DriverError::failed(
                "cleanup_failed",
                "could not remove run",
            ))),
            ..FakeRunner::default()
        };
        let report = execute(
            &mut runner,
            &scenario(vec![assert_step("initial", "value_equals", Some(json!(0)))]),
            None,
        );
        assert_eq!(report.status, CheckStatus::Failed);
        assert_eq!(
            report.primary_error.as_ref().unwrap().code,
            "cleanup_failed"
        );
        assert_eq!(report.steps[0].status, StepStatus::Passed);
    }

    #[test]
    fn action_delivery_is_retained_when_follow_up_observation_is_unknown() {
        let action = ScenarioStep {
            id: "click".into(),
            kind: "click".into(),
            selector: Some(selector("counter.value")),
            assertion: None,
            expected: None,
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
        };
        let mut runner = FakeRunner {
            prepared: Some(Ok(observation(
                "o-1",
                vec![node("counter.value", json!(0))],
            ))),
            actions: VecDeque::from([Ok(ActionResult::succeeded("op-1"))]),
            observations: VecDeque::from([Err(DriverError::unknown(
                "observation_unknown",
                "runtime disconnected after delivery",
            ))]),
            ..FakeRunner::default()
        };
        let report = execute(&mut runner, &scenario(vec![action]), None);
        assert_eq!(report.status, CheckStatus::Inconclusive);
        assert_eq!(report.steps[0].status, StepStatus::Inconclusive);
        assert_eq!(
            report.steps[0]
                .action
                .as_ref()
                .unwrap()
                .operation_id
                .as_deref(),
            Some("op-1")
        );
        assert_eq!(
            report.primary_error.as_ref().unwrap().code,
            "observation_unknown"
        );
    }

    #[test]
    fn screenshot_comparison_requires_comparable_evidence() {
        let mut current = observation("o-1", Vec::new());
        current.screenshot = Some(ScreenshotEvidence {
            artifact_id: None,
            baseline_id: Some("missing".into()),
            baseline_key: None,
            diff: None,
            scope: None,
            provider: None,
            pixel_width: None,
            pixel_height: None,
            logical_width: None,
            logical_height: None,
            scale_milli: None,
            comparable: false,
            matches: None,
            reason: Some("baseline_missing".into()),
        });
        let result = evaluate_assertion("screenshot_matches", None, None, &current);
        assert_eq!(result.status, StepStatus::Inconclusive);
        assert_eq!(result.message, "baseline_missing");
    }

    #[test]
    fn screenshot_assertion_resolves_and_reports_artifact_evidence() {
        let mut step = assert_step("visual", "screenshot_matches", None);
        step.selector = None;
        step.baseline_id = Some("counter".into());
        let mut runner = FakeRunner {
            prepared: Some(Ok(observation("o-1", Vec::new()))),
            ..FakeRunner::default()
        };
        let report = execute(&mut runner, &scenario(vec![step]), None);
        assert_eq!(report.status, CheckStatus::Passed);
        assert_eq!(runner.screenshot_resolutions, 1);
        assert_eq!(
            report.steps[0]
                .assertion
                .as_ref()
                .unwrap()
                .actual
                .as_ref()
                .unwrap()["artifact_id"],
            "artifact-1"
        );
        assert_eq!(
            report.steps[0]
                .assertion
                .as_ref()
                .unwrap()
                .actual
                .as_ref()
                .unwrap()["matches"],
            true
        );
    }
}
