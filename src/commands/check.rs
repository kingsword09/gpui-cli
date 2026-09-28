//! Execute one schema-v1 scenario against an isolated desktop preview.

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::run::Project;
use crate::devserver::actions::Action;
use crate::devserver::control::{self, Command as ControlCommand, Registration};
use crate::devserver::events::{Event, Kind, Page};
use crate::scenario::executor::{
    ActionResult, CaptureEvidence, CheckReport, DriverError, DriverErrorKind, Observation,
    ScenarioRunner, ScreenshotEvidence, SemanticNode,
};
use crate::scenario::{self, ScenarioDefinition, ScenarioFile, ScenarioStep, Selector};

const PREVIEW_START_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const EVENT_POLL_TIMEOUT_MS: u64 = 1_000;
const QUERY_LIMIT: u32 = 200;

#[derive(Args)]
pub struct CheckArgs {
    /// Scenario id to execute from the scenario file
    #[arg(long)]
    pub scenario: String,
    /// Scenario file, relative to the current project by default
    #[arg(long, default_value = "gpui.scenarios.toml")]
    pub file: PathBuf,
    /// Check target; this slice supports desktop only
    #[arg(long, default_value = "desktop")]
    pub target: String,
    /// Emit the structured check report as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn handle_check(args: CheckArgs) -> Result<()> {
    let project = Project::load(None)?;
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
    let source = fs::read_to_string(&file)
        .with_context(|| format!("reading scenario file {}", file.display()))?;
    let model: ScenarioFile = toml::from_str(&source).context("parsing scenario file")?;
    let selected = model
        .scenarios
        .iter()
        .find(|scenario| scenario.id == args.scenario)
        .with_context(|| format!("scenario `{}` was not found", args.scenario))?;
    let fixture_hash = validation
        .scenarios
        .iter()
        .find(|entry| entry.id == selected.id)
        .and_then(|entry| entry.fixture_hash.clone());

    prepare_cargo_lock(&project.root)?;
    let mut runner =
        DesktopCheckRunner::launch(&project.root, &file, selected, fixture_hash.clone())?;
    let report = crate::scenario::executor::execute(&mut runner, selected, fixture_hash);
    print_report(&report, args.json)?;
    if report.status == crate::scenario::executor::CheckStatus::Passed {
        Ok(())
    } else {
        bail!("scenario check ended with {:?}", report.status)
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
    child: Child,
    registration: Registration,
    event_cursor: u64,
    scenario_id: String,
    component: String,
    default_requirements: Vec<String>,
    issue_floor: u64,
}

impl DesktopCheckRunner {
    fn launch(
        project_root: &Path,
        scenario_file: &Path,
        scenario: &ScenarioDefinition,
        _fixture_hash: Option<String>,
    ) -> Result<Self> {
        let executable = std::env::current_exe().context("locating the gpui executable")?;
        let scenario_file = scenario_file
            .strip_prefix(project_root)
            .unwrap_or(scenario_file);
        let mut child = Command::new(executable);
        child
            .current_dir(project_root)
            .args([
                "preview",
                &scenario.component,
                "--scenario",
                &scenario.id,
                "--file",
                &scenario_file.to_string_lossy(),
                "--target",
                "desktop",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let child = child.spawn().context("starting isolated desktop preview")?;
        let deadline = Instant::now() + PREVIEW_START_TIMEOUT;
        let registration = match wait_for_registration(project_root, &scenario.id, deadline) {
            Ok(registration) => registration,
            Err(error) => {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut runner = Self {
            child,
            registration,
            event_cursor: 0,
            scenario_id: scenario.id.clone(),
            component: scenario.component.clone(),
            default_requirements: observation_requirements(&scenario.requires),
            issue_floor: 0,
        };
        runner.wait_for_ready(deadline)?;
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
        let reply = control::request(
            &self.registration,
            &control::next_request_id("check.observe"),
            ControlCommand::Observe {
                sync: false,
                window_id: None,
                require: requirements.clone(),
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
        self.observation_from_result(result, &requirements)
    }

    fn observation_from_result(
        &self,
        result: Value,
        requirements: &[String],
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
        let screenshot = screenshot_evidence(&result);
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
}

impl ScenarioRunner for DesktopCheckRunner {
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
            artifact_id: None,
            kinds,
        })
    }

    fn cleanup(&mut self) -> Result<(), DriverError> {
        self.child
            .kill()
            .map_err(|error| DriverError::failed("cleanup_failed", error.to_string()))?;
        self.child
            .wait()
            .map_err(|error| DriverError::failed("cleanup_failed", error.to_string()))?;
        Ok(())
    }
}

impl Drop for DesktopCheckRunner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for_registration(
    project_root: &Path,
    scenario_id: &str,
    deadline: Instant,
) -> Result<Registration> {
    loop {
        if Instant::now() >= deadline {
            bail!("no control session appeared for scenario `{scenario_id}`")
        }
        if let Ok(registration) = control::discover(project_root, None) {
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
    let has_png = artifacts.iter().any(|artifact| artifact["kind"] == "png");
    has_png.then_some(ScreenshotEvidence {
        baseline_id: None,
        comparable: false,
        matches: None,
        reason: Some("baseline_not_loaded".into()),
    })
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
}
