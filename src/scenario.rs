//! Static validation for schema-v1 scenario files.
//!
//! This module deliberately stops before preview/check execution. It validates
//! the portable plan, its fixture inputs, and bounded hashes without starting
//! an app or assuming that a component registry exists.

use crate::fixtures::Fixture;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
pub const MAX_STEPS: usize = 200;
pub const MAX_VIEWPORT: u32 = 8_192;
pub const MAX_FIXTURE_BYTES: u64 = 16 * 1024 * 1024;

const DEFAULT_REQUIRES: &[&str] = &["screenshot", "semantics", "scenario.reset"];
const ALLOWED_REQUIREMENTS: &[&str] = &[
    "screenshot",
    "semantics",
    "semantics.read",
    "semantics.bounds",
    "scenario.reset",
    "input.pointer",
    "input.keyboard",
    "capture.scene",
    "capture.window",
    "capture.device",
];
const ALLOWED_ASSERTIONS: &[&str] = &[
    "exists",
    "absent",
    "value_equals",
    "text_equals",
    "enabled",
    "focused",
    "visible",
    "not_clipped",
    "no_runtime_errors",
    "screenshot_matches",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioFile {
    pub schema_version: u32,
    pub scenarios: Vec<ScenarioDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDefinition {
    pub id: String,
    pub component: String,
    pub fixture: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_requires")]
    pub requires: Vec<String>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_locale")]
    pub locale: String,
    pub random_seed: Option<i64>,
    #[serde(default = "default_clock")]
    pub clock: String,
    pub clock_at: Option<String>,
    pub viewport: Viewport,
    pub ready_id: Option<String>,
    pub steps: Vec<ScenarioStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub scale: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioStep {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub selector: Option<Selector>,
    pub assertion: Option<String>,
    pub expected: Option<Value>,
    pub label: Option<String>,
    #[serde(default)]
    pub require: Vec<String>,
    pub baseline_id: Option<String>,
    pub text: Option<String>,
    #[serde(default = "default_text_mode")]
    pub mode: String,
    #[serde(default = "default_button")]
    pub button: String,
    pub key: Option<String>,
    #[serde(default)]
    pub delta_x: i64,
    #[serde(default)]
    pub delta_y: i64,
    #[serde(default)]
    pub duration_ms: u64,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub logical_id: Option<String>,
    pub role: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioEntryReport {
    pub id: String,
    pub component: String,
    pub fixture: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub schema_version: u32,
    pub file: PathBuf,
    pub valid: bool,
    pub scenario_hash: Option<String>,
    pub scenarios: Vec<ScenarioEntryReport>,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

pub fn validate_file(file: &Path, project_root: &Path) -> Result<ValidationReport> {
    let file = file
        .canonicalize()
        .with_context(|| format!("reading scenario file {}", file.display()))?;
    let project_root = project_root
        .canonicalize()
        .with_context(|| format!("reading project root {}", project_root.display()))?;
    if !file.starts_with(&project_root) {
        bail!(
            "scenario file is outside the project root: {}",
            file.display()
        );
    }
    let source = fs::read_to_string(&file)
        .with_context(|| format!("reading scenario file {}", file.display()))?;
    let parsed = toml::from_str::<ScenarioFile>(&source).map_err(|error| {
        let location = error
            .span()
            .map(|span| location_for_offset(&source, span.start));
        anyhow::anyhow!(format_diagnostic(
            &file,
            location.as_ref(),
            &error.to_string()
        ))
    })?;

    let mut report = ValidationReport {
        schema_version: parsed.schema_version,
        file: file.clone(),
        valid: false,
        scenario_hash: None,
        scenarios: Vec::new(),
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    let mut normalized = parsed.clone();
    let registry = load_registry(&project_root, &mut report);
    validate_file_model(
        &mut normalized,
        &source,
        &file,
        &project_root,
        registry.as_ref(),
        &mut report,
    );
    report.scenario_hash = Some(hash_json(&normalized));
    report.valid = report.errors.is_empty();
    Ok(report)
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_requires() -> Vec<String> {
    DEFAULT_REQUIRES
        .iter()
        .map(|value| (*value).into())
        .collect()
}

fn default_theme() -> String {
    "light".into()
}

fn default_locale() -> String {
    "en-US".into()
}

fn default_clock() -> String {
    "real".into()
}

fn default_text_mode() -> String {
    "replace".into()
}

fn default_button() -> String {
    "primary".into()
}

fn default_capture_requires() -> Vec<String> {
    vec!["screenshot".into()]
}

fn hash_json<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("scenario serialization is infallible");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn load_registry(project_root: &Path, report: &mut ValidationReport) -> Option<BTreeSet<String>> {
    let path = project_root.join(".gpui/registry-manifest.json");
    let Ok(bytes) = fs::read(&path) else {
        report.warnings.push(Diagnostic {
            path: "registry".into(),
            message: "registry_unavailable: no .gpui/registry-manifest.json was found".into(),
            location: None,
        });
        return None;
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            report.errors.push(Diagnostic {
                path: "registry".into(),
                message: format!("registry_invalid: {error}"),
                location: None,
            });
            return None;
        }
    };
    let Some(components) = value.get("components").and_then(Value::as_array) else {
        report.errors.push(Diagnostic {
            path: "registry.components".into(),
            message: "registry_invalid: components must be an array".into(),
            location: None,
        });
        return None;
    };
    let mut names = BTreeSet::new();
    for (index, component) in components.iter().enumerate() {
        let Some(name) = component.get("name").and_then(Value::as_str) else {
            report.errors.push(Diagnostic {
                path: format!("registry.components[{index}].name"),
                message: "registry_invalid: component name must be a string".into(),
                location: None,
            });
            continue;
        };
        names.insert(name.to_owned());
    }
    Some(names)
}

fn validate_file_model(
    file_model: &mut ScenarioFile,
    source: &str,
    file: &Path,
    project_root: &Path,
    registry: Option<&BTreeSet<String>>,
    report: &mut ValidationReport,
) {
    if file_model.schema_version != SCHEMA_VERSION {
        error(
            report,
            "schema_version",
            format!(
                "unsupported schema_version {}; expected {SCHEMA_VERSION}",
                file_model.schema_version
            ),
            locate_field(source, "schema_version"),
        );
    }
    if file_model.scenarios.is_empty() {
        error(
            report,
            "scenarios",
            "at least one scenario is required".into(),
            locate_field(source, "scenarios"),
        );
    }
    let mut scenario_ids = BTreeSet::new();
    for (index, scenario) in file_model.scenarios.iter_mut().enumerate() {
        let path = format!("scenarios[{index}]");
        let location = locate_marker(source, "id", &scenario.id);
        if !valid_identifier(&scenario.id) {
            error(
                report,
                &format!("{path}.id"),
                "id must contain only lowercase letters, digits, dots, and hyphens and be at most 128 bytes".into(),
                location.clone(),
            );
        }
        if !scenario_ids.insert(scenario.id.clone()) {
            error(
                report,
                &format!("{path}.id"),
                "duplicate scenario id".into(),
                location.clone(),
            );
        }
        if scenario.component.trim().is_empty() || scenario.component.len() > 128 {
            error(
                report,
                &format!("{path}.component"),
                "component must be non-empty and at most 128 bytes".into(),
                locate_marker(source, "component", &scenario.component),
            );
        }
        if let Some(registry) = registry
            && !registry.contains(&scenario.component)
        {
            error(
                report,
                &format!("{path}.component"),
                format!("unknown component `{}` in registry", scenario.component),
                locate_marker(source, "component", &scenario.component),
            );
        }
        validate_timeout(
            scenario.timeout_ms,
            &format!("{path}.timeout_ms"),
            source,
            report,
        );
        validate_requirements(
            &scenario.requires,
            &format!("{path}.requires"),
            source,
            report,
        );
        normalize_strings(&mut scenario.tags);
        normalize_strings(&mut scenario.requires);
        validate_environment(scenario, &path, source, report);
        validate_fixture(
            scenario,
            &path,
            file,
            project_root,
            registry,
            source,
            report,
        );
        validate_steps(scenario, &path, source, report);
        report.scenarios.push(ScenarioEntryReport {
            id: scenario.id.clone(),
            component: scenario.component.clone(),
            fixture: scenario.fixture.clone(),
            fixture_hash: fixture_hash(file, project_root, &scenario.fixture),
        });
    }
}

fn validate_timeout(timeout: u64, path: &str, source: &str, report: &mut ValidationReport) {
    if timeout == 0 || timeout > MAX_TIMEOUT_MS {
        error(
            report,
            path,
            format!("timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"),
            locate_field(source, "timeout_ms"),
        );
    }
}

fn validate_environment(
    scenario: &ScenarioDefinition,
    path: &str,
    source: &str,
    report: &mut ValidationReport,
) {
    if scenario.theme != "light" && scenario.theme != "dark" {
        error(
            report,
            &format!("{path}.theme"),
            "theme must be light or dark".into(),
            locate_field(source, "theme"),
        );
    }
    if scenario.locale.trim().is_empty() || scenario.locale.len() > 64 {
        error(
            report,
            &format!("{path}.locale"),
            "locale must be non-empty and at most 64 bytes".into(),
            locate_field(source, "locale"),
        );
    }
    match scenario.clock.as_str() {
        "real" if scenario.clock_at.is_some() => error(
            report,
            &format!("{path}.clock_at"),
            "clock_at is forbidden when clock is real".into(),
            locate_field(source, "clock_at"),
        ),
        "fixed"
            if scenario
                .clock_at
                .as_deref()
                .is_none_or(|value| !valid_rfc3339(value)) =>
        {
            error(
                report,
                &format!("{path}.clock_at"),
                "fixed clock requires an RFC 3339 clock_at".into(),
                locate_field(source, "clock_at"),
            )
        }
        "real" | "fixed" => {}
        _ => error(
            report,
            &format!("{path}.clock"),
            "clock must be real or fixed".into(),
            locate_field(source, "clock"),
        ),
    }
    if scenario.viewport.width == 0
        || scenario.viewport.width > MAX_VIEWPORT
        || scenario.viewport.height == 0
        || scenario.viewport.height > MAX_VIEWPORT
    {
        error(
            report,
            &format!("{path}.viewport"),
            format!("viewport width and height must be between 1 and {MAX_VIEWPORT}"),
            locate_field(source, "viewport"),
        );
    }
    if scenario
        .viewport
        .scale
        .is_some_and(|scale| !scale.is_finite() || scale <= 0.0 || scale > 16.0)
    {
        error(
            report,
            &format!("{path}.viewport.scale"),
            "viewport.scale must be finite and between 0 and 16".into(),
            locate_field(source, "scale"),
        );
    }
    if let Some(ready_id) = &scenario.ready_id
        && (ready_id.is_empty() || ready_id.len() > 256 || ready_id.contains('\0'))
    {
        error(
            report,
            &format!("{path}.ready_id"),
            "ready_id must be a non-empty logical id at most 256 bytes".into(),
            locate_field(source, "ready_id"),
        );
    }
}

fn validate_fixture(
    scenario: &ScenarioDefinition,
    path: &str,
    file: &Path,
    project_root: &Path,
    registry: Option<&BTreeSet<String>>,
    source: &str,
    report: &mut ValidationReport,
) {
    if !valid_relative_path(&scenario.fixture) {
        error(
            report,
            &format!("{path}.fixture"),
            "fixture must be a relative path that stays inside the project root".into(),
            locate_field(source, "fixture"),
        );
        return;
    }
    let candidate = file
        .parent()
        .unwrap_or(project_root)
        .join(&scenario.fixture);
    let Ok(resolved) = candidate.canonicalize() else {
        error(
            report,
            &format!("{path}.fixture"),
            format!("fixture does not exist: {}", candidate.display()),
            locate_field(source, "fixture"),
        );
        return;
    };
    if !resolved.starts_with(project_root) {
        error(
            report,
            &format!("{path}.fixture"),
            "fixture resolves outside the project root".into(),
            locate_field(source, "fixture"),
        );
        return;
    }
    let Ok(metadata) = fs::metadata(&resolved) else {
        error(
            report,
            &format!("{path}.fixture"),
            "fixture metadata is unavailable".into(),
            locate_field(source, "fixture"),
        );
        return;
    };
    if metadata.len() > MAX_FIXTURE_BYTES {
        error(
            report,
            &format!("{path}.fixture"),
            format!("fixture exceeds {MAX_FIXTURE_BYTES} bytes"),
            locate_field(source, "fixture"),
        );
        return;
    }
    let Ok(contents) = fs::read_to_string(&resolved) else {
        error(
            report,
            &format!("{path}.fixture"),
            "fixture must be UTF-8 JSON".into(),
            locate_field(source, "fixture"),
        );
        return;
    };
    let value: Value = match serde_json::from_str(&contents) {
        Ok(value) => value,
        Err(error_message) => {
            error(
                report,
                &format!("{path}.fixture"),
                format!("fixture is not valid JSON: {error_message}"),
                locate_field(source, "fixture"),
            );
            return;
        }
    };
    let fixture_component = value.get("component").and_then(Value::as_str);
    if fixture_component != Some(scenario.component.as_str()) {
        error(
            report,
            &format!("{path}.fixture"),
            format!(
                "fixture component `{}` does not match scenario component `{}`",
                fixture_component.unwrap_or("<missing>"),
                scenario.component
            ),
            locate_field(source, "fixture"),
        );
    } else if matches!(
        scenario.component.as_str(),
        "Counter" | "LoginForm" | "VirtualList"
    ) {
        if let Err(error_message) = Fixture::parse(&contents) {
            error(
                report,
                &format!("{path}.fixture"),
                format!("invalid fixture: {error_message}"),
                locate_field(source, "fixture"),
            );
        }
    } else {
        report.warnings.push(Diagnostic {
            path: format!("{path}.fixture"),
            message: if registry.is_some() {
                "fixture_schema_unavailable: registry component is known but its fixture schema is not available to S01".into()
            } else {
                "fixture_schema_unavailable: no registry manifest was found for this custom component".into()
            },
            location: locate_field(source, "fixture"),
        });
    }
}

fn fixture_hash(file: &Path, project_root: &Path, fixture: &str) -> Option<String> {
    if !valid_relative_path(fixture) {
        return None;
    }
    let path = file.parent().unwrap_or(project_root).join(fixture);
    let bytes = fs::read(path).ok()?;
    Some(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn validate_steps(
    scenario: &mut ScenarioDefinition,
    path: &str,
    source: &str,
    report: &mut ValidationReport,
) {
    if scenario.steps.is_empty() || scenario.steps.len() > MAX_STEPS {
        error(
            report,
            &format!("{path}.steps"),
            format!("steps must contain between 1 and {MAX_STEPS} items"),
            locate_field(source, "steps"),
        );
    }
    let mut step_ids = BTreeSet::new();
    for (index, step) in scenario.steps.iter_mut().enumerate() {
        let step_path = format!("{path}.steps[{index}]");
        if step.kind == "capture" && step.require.is_empty() {
            step.require = default_capture_requires();
        }
        let location = locate_marker(source, "id", &step.id);
        if !valid_identifier(&step.id) {
            error(
                report,
                &format!("{step_path}.id"),
                "step id must contain only lowercase letters, digits, dots, and hyphens and be at most 128 bytes".into(),
                location.clone(),
            );
        }
        if !step_ids.insert(step.id.clone()) {
            error(
                report,
                &format!("{step_path}.id"),
                "duplicate step id".into(),
                location,
            );
        }
        if step
            .timeout_ms
            .is_some_and(|timeout| timeout == 0 || timeout > scenario.timeout_ms)
        {
            error(
                report,
                &format!("{step_path}.timeout_ms"),
                "step timeout must be positive and no greater than scenario timeout_ms".into(),
                locate_field(source, "timeout_ms"),
            );
        }
        if let Some(selector) = &step.selector {
            validate_selector(selector, &format!("{step_path}.selector"), source, report);
        }
        match step.kind.as_str() {
            "click" => {
                require_selector(step, &step_path, source, report);
                if step.button != "primary" && step.button != "secondary" {
                    error(
                        report,
                        &format!("{step_path}.button"),
                        "button must be primary or secondary".into(),
                        locate_field(source, "button"),
                    );
                }
                reject_fields(
                    step,
                    &step_path,
                    source,
                    report,
                    &["selector", "button", "timeout_ms"],
                );
            }
            "type_text" => {
                require_selector(step, &step_path, source, report);
                if step.text.is_none() {
                    error(
                        report,
                        &format!("{step_path}.text"),
                        "type_text requires text".into(),
                        locate_field(source, "text"),
                    );
                }
                if step.mode != "replace" && step.mode != "append" {
                    error(
                        report,
                        &format!("{step_path}.mode"),
                        "mode must be replace or append".into(),
                        locate_field(source, "mode"),
                    );
                }
                reject_fields(
                    step,
                    &step_path,
                    source,
                    report,
                    &["selector", "text", "mode", "timeout_ms"],
                );
            }
            "key" => {
                require_selector(step, &step_path, source, report);
                if step
                    .key
                    .as_deref()
                    .is_none_or(|key| !["Enter", "Tab", "Escape", "Backspace"].contains(&key))
                {
                    error(
                        report,
                        &format!("{step_path}.key"),
                        "key must be Enter, Tab, Escape, or Backspace".into(),
                        locate_field(source, "key"),
                    );
                }
                reject_fields(
                    step,
                    &step_path,
                    source,
                    report,
                    &["selector", "key", "timeout_ms"],
                );
            }
            "scroll" => {
                require_selector(step, &step_path, source, report);
                if step.delta_x == 0 && step.delta_y == 0 {
                    error(
                        report,
                        &step_path,
                        "scroll requires a non-zero delta_x or delta_y".into(),
                        locate_field(source, "delta_y"),
                    );
                }
                if step.duration_ms > scenario.timeout_ms {
                    error(
                        report,
                        &format!("{step_path}.duration_ms"),
                        "duration_ms must not exceed scenario timeout_ms".into(),
                        locate_field(source, "duration_ms"),
                    );
                }
                reject_fields(
                    step,
                    &step_path,
                    source,
                    report,
                    &[
                        "selector",
                        "delta_x",
                        "delta_y",
                        "duration_ms",
                        "timeout_ms",
                    ],
                );
            }
            "wait_for" => {
                require_selector(step, &step_path, source, report);
                validate_assertion(step, &step_path, source, report);
                reject_fields(
                    step,
                    &step_path,
                    source,
                    report,
                    &[
                        "selector",
                        "assertion",
                        "expected",
                        "baseline_id",
                        "timeout_ms",
                    ],
                );
            }
            "assert" => {
                validate_assertion(step, &step_path, source, report);
                if !matches!(
                    step.assertion.as_deref(),
                    Some("no_runtime_errors") | Some("screenshot_matches")
                ) {
                    require_selector(step, &step_path, source, report);
                }
                reject_fields(
                    step,
                    &step_path,
                    source,
                    report,
                    &["selector", "assertion", "expected", "baseline_id"],
                );
            }
            "capture" => {
                if step.label.as_deref().is_none_or(str::is_empty) {
                    error(
                        report,
                        &format!("{step_path}.label"),
                        "capture requires a non-empty label".into(),
                        locate_field(source, "label"),
                    );
                }
                validate_requirements(
                    &step.require,
                    &format!("{step_path}.require"),
                    source,
                    report,
                );
                reject_fields(
                    step,
                    &step_path,
                    source,
                    report,
                    &["label", "require", "timeout_ms"],
                );
            }
            other => error(
                report,
                &format!("{step_path}.type"),
                format!("unsupported step type `{other}`"),
                locate_field(source, "type"),
            ),
        }
    }
}

fn reject_fields(
    step: &ScenarioStep,
    path: &str,
    source: &str,
    report: &mut ValidationReport,
    allowed: &[&str],
) {
    let fields = [
        ("selector", step.selector.is_some()),
        ("assertion", step.assertion.is_some()),
        ("expected", step.expected.is_some()),
        ("label", step.label.is_some()),
        ("require", !step.require.is_empty()),
        ("baseline_id", step.baseline_id.is_some()),
        ("text", step.text.is_some()),
        ("mode", step.mode != default_text_mode()),
        ("button", step.button != default_button()),
        ("key", step.key.is_some()),
        ("delta_x", step.delta_x != 0),
        ("delta_y", step.delta_y != 0),
        ("duration_ms", step.duration_ms != 0),
        ("timeout_ms", step.timeout_ms.is_some()),
    ];
    for (field, present) in fields {
        if present && !allowed.contains(&field) {
            error(
                report,
                &format!("{path}.{field}"),
                format!("field is not valid for step type `{}`", step.kind),
                locate_field(source, field),
            );
        }
    }
}

fn require_selector(step: &ScenarioStep, path: &str, source: &str, report: &mut ValidationReport) {
    if step.selector.is_none() {
        error(
            report,
            &format!("{path}.selector"),
            "this step requires a selector".into(),
            locate_field(source, "selector"),
        );
    }
}

fn validate_selector(selector: &Selector, path: &str, source: &str, report: &mut ValidationReport) {
    let logical = selector
        .logical_id
        .as_deref()
        .filter(|value| !value.is_empty());
    let role = selector.role.as_deref().filter(|value| !value.is_empty());
    let name = selector.name.as_deref().filter(|value| !value.is_empty());
    if logical.is_some() == (role.is_some() || name.is_some()) {
        error(
            report,
            path,
            "selector must choose exactly logical_id or role plus name".into(),
            locate_field(source, "selector"),
        );
    }
    if logical.is_none() && (role.is_none() || name.is_none()) {
        error(
            report,
            path,
            "role/name selectors require both role and name".into(),
            locate_field(source, "selector"),
        );
    }
    for (field, value) in [("logical_id", logical), ("role", role), ("name", name)] {
        if value.is_some_and(|value| value.len() > 256) {
            error(
                report,
                &format!("{path}.{field}"),
                "selector value must be at most 256 bytes".into(),
                locate_field(source, field),
            );
        }
    }
}

fn validate_assertion(
    step: &ScenarioStep,
    path: &str,
    source: &str,
    report: &mut ValidationReport,
) {
    let Some(assertion) = step.assertion.as_deref() else {
        error(
            report,
            &format!("{path}.assertion"),
            "assertion is required".into(),
            locate_field(source, "assertion"),
        );
        return;
    };
    if !ALLOWED_ASSERTIONS.contains(&assertion) {
        error(
            report,
            &format!("{path}.assertion"),
            format!("unsupported assertion `{assertion}`"),
            locate_field(source, "assertion"),
        );
        return;
    }
    match assertion {
        "value_equals" => {
            if step.expected.as_ref().is_none_or(|value| {
                !value.is_number() && !value.is_boolean() && !value.is_string() && !value.is_null()
            }) {
                error(
                    report,
                    &format!("{path}.expected"),
                    "value_equals expected must be a scalar".into(),
                    locate_field(source, "expected"),
                );
            }
        }
        "text_equals" => {
            if step.expected.as_ref().and_then(Value::as_str).is_none() {
                error(
                    report,
                    &format!("{path}.expected"),
                    "text_equals expected must be a string".into(),
                    locate_field(source, "expected"),
                );
            }
        }
        "screenshot_matches" => {
            if step.baseline_id.as_deref().is_none_or(str::is_empty) {
                error(
                    report,
                    &format!("{path}.baseline_id"),
                    "screenshot_matches requires baseline_id".into(),
                    locate_field(source, "baseline_id"),
                );
            }
            if step.expected.is_some() {
                error(
                    report,
                    &format!("{path}.expected"),
                    "screenshot_matches does not accept expected".into(),
                    locate_field(source, "expected"),
                );
            }
        }
        _ => {}
    }
    if matches!(
        assertion,
        "exists"
            | "absent"
            | "enabled"
            | "focused"
            | "visible"
            | "not_clipped"
            | "no_runtime_errors"
    ) && step.expected.is_some()
    {
        error(
            report,
            &format!("{path}.expected"),
            format!("{assertion} does not accept expected"),
            locate_field(source, "expected"),
        );
    }
}

fn validate_requirements(
    requirements: &[String],
    path: &str,
    source: &str,
    report: &mut ValidationReport,
) {
    if requirements.is_empty() {
        error(
            report,
            path,
            "at least one requirement is required".into(),
            locate_field(source, "requires"),
        );
        return;
    }
    let mut seen = BTreeSet::new();
    for requirement in requirements.iter() {
        if !ALLOWED_REQUIREMENTS.contains(&requirement.as_str()) {
            error(
                report,
                path,
                format!("unsupported requirement `{requirement}`"),
                locate_field(source, "requires"),
            );
        }
        if !seen.insert(requirement) {
            error(
                report,
                path,
                format!("duplicate requirement `{requirement}`"),
                locate_field(source, "requires"),
            );
        }
    }
}

fn normalize_strings(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
}

fn valid_relative_path(value: &str) -> bool {
    if value.is_empty() || value.contains('\0') || value.contains('\\') {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

fn valid_rfc3339(value: &str) -> bool {
    if value.len() < 20 {
        return false;
    }
    let bytes = value.as_bytes();
    bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':')
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
        && bytes[11..13].iter().all(u8::is_ascii_digit)
        && bytes[14..16].iter().all(u8::is_ascii_digit)
        && bytes[17..19].iter().all(u8::is_ascii_digit)
        && (bytes.last() == Some(&b'Z')
            || bytes[19..].contains(&b'+')
            || bytes[19..].contains(&b'-'))
}

fn error(
    report: &mut ValidationReport,
    path: &str,
    message: String,
    location: Option<SourceLocation>,
) {
    report.errors.push(Diagnostic {
        path: path.into(),
        message,
        location,
    });
}

fn location_for_offset(source: &str, offset: usize) -> SourceLocation {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];
    SourceLocation {
        line: prefix.bytes().filter(|byte| *byte == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().map_or(1, |line| line.len() + 1),
    }
}

fn locate_field(source: &str, field: &str) -> Option<SourceLocation> {
    source
        .lines()
        .enumerate()
        .find(|(_, line)| {
            let trimmed = line.trim_start();
            trimmed.starts_with(&format!("{field} ="))
                || trimmed.starts_with(&format!("{field}="))
                || trimmed.contains(&format!("{field} ="))
        })
        .map(|(line, text)| SourceLocation {
            line: line + 1,
            column: text.find(field).unwrap_or(0) + 1,
        })
        .or(Some(SourceLocation { line: 1, column: 1 }))
}

fn locate_marker(source: &str, field: &str, value: &str) -> Option<SourceLocation> {
    let quoted = format!("{field} = \"{value}\"");
    source
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(&quoted))
        .map(|(line, text)| SourceLocation {
            line: line + 1,
            column: text.find(field).unwrap_or(0) + 1,
        })
        .or_else(|| locate_field(source, field))
}

fn format_diagnostic(file: &Path, location: Option<&SourceLocation>, message: &str) -> String {
    match location {
        Some(location) => format!(
            "{}:{}:{}: {}",
            file.display(),
            location.line,
            location.column,
            message
        ),
        None => format!("{}: {}", file.display(), message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_project() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("gpui-scenario-test-{suffix}"));
        fs::create_dir_all(root.join("fixtures")).unwrap();
        fs::write(
            root.join("fixtures/counter.json"),
            include_str!("../tests/fixtures/agent-native/counter-zero.json"),
        )
        .unwrap();
        root
    }

    #[test]
    fn valid_scenario_normalizes_defaults_and_hashes_fixture() {
        let root = fixture_project();
        let file = root.join("gpui.scenarios.toml");
        fs::write(
            &file,
            r#"schema_version = 1

[[scenarios]]
id = "counter-basic"
component = "Counter"
fixture = "fixtures/counter.json"
viewport = { width = 640, height = 480 }

[[scenarios.steps]]
id = "initial"
type = "assert"
selector = { logical_id = "counter.value" }
assertion = "value_equals"
expected = 0
"#,
        )
        .unwrap();
        let report = validate_file(&file, &root).unwrap();
        assert!(report.valid);
        assert!(report.scenario_hash.is_some());
        assert_eq!(
            report.scenarios[0].fixture_hash.as_deref().unwrap().len(),
            71
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.message.starts_with("registry_unavailable"))
        );
    }

    #[test]
    fn invalid_selector_and_fixed_clock_are_rejected_with_locations() {
        let root = fixture_project();
        let file = root.join("gpui.scenarios.toml");
        fs::write(
            &file,
            r#"schema_version = 1

[[scenarios]]
id = "counter-basic"
component = "Counter"
fixture = "fixtures/counter.json"
clock = "fixed"
viewport = { width = 640, height = 480 }

[[scenarios.steps]]
id = "bad-step"
type = "click"
selector = { role = "button" }
"#,
        )
        .unwrap();
        let report = validate_file(&file, &root).unwrap();
        assert!(!report.valid);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.message.contains("fixed clock"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.message.contains("role/name selectors"))
        );
        assert!(report.errors.iter().all(|error| error.location.is_some()));
    }
}
