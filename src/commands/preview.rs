//! Launch one explicitly registered scenario preview.

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::live::PreviewLaunch;
use super::run::Project;
use crate::scenario::{self, ScenarioDefinition, ScenarioFile};

#[derive(Args)]
pub struct PreviewArgs {
    /// Explicit component name from the preview registry
    pub component: String,
    /// Scenario id to load from the scenario file
    #[arg(long)]
    pub scenario: String,
    /// Scenario file, relative to the current project by default
    #[arg(long, default_value = "gpui.scenarios.toml")]
    pub file: PathBuf,
    /// Preview target; the first runtime slice supports desktop
    #[arg(long, default_value = "desktop")]
    pub target: String,
    /// Emit the launch envelope as JSON before attaching to the preview
    #[arg(long)]
    pub json: bool,
}

pub fn handle_preview(args: PreviewArgs) -> Result<()> {
    let project = Project::load(None)?;
    let file = resolve_project_path(&project.root, args.file);
    let report = scenario::validate_file(&file, &project.root)?;
    if !report.valid {
        bail!("scenario validation failed; preview was not launched");
    }
    let source = fs::read_to_string(&file)
        .with_context(|| format!("reading scenario file {}", file.display()))?;
    let model: ScenarioFile = toml::from_str(&source).context("parsing scenario file")?;
    let selected = model
        .scenarios
        .iter()
        .find(|scenario| scenario.id == args.scenario)
        .with_context(|| format!("scenario `{}` was not found", args.scenario))?;
    if selected.component != args.component {
        bail!(
            "scenario `{}` selects component `{}`, not `{}`",
            selected.id,
            selected.component,
            args.component
        );
    }
    let fixture_path = fixture_path(&file, &project.root, selected)?;
    let fixture_hash = report
        .scenarios
        .iter()
        .find(|entry| entry.id == selected.id)
        .and_then(|entry| entry.fixture_hash.clone())
        .context("validated scenario did not produce a fixture hash")?;
    let run_id = preview_run_id();
    let data_dir = project
        .root
        .join(".gpui/previews")
        .join(&run_id)
        .join("data");
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating preview data directory {}", data_dir.display()))?;

    let launch = PreviewLaunch {
        scenario_id: selected.id.clone(),
        component: selected.component.clone(),
        fixture_path,
        fixture_hash,
        data_dir,
        project_root: project.root.clone(),
        theme: selected.theme.clone(),
        locale: selected.locale.clone(),
        clock: selected.clock.clone(),
        clock_at: selected.clock_at.clone(),
        random_seed: selected.random_seed,
        uncontrolled_inputs: uncontrolled_inputs(selected),
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": "starting",
                "component": launch.component.clone(),
                "scenario_id": launch.scenario_id.clone(),
                "target": args.target.clone(),
                "fixture_hash": launch.fixture_hash.clone(),
                "data_dir": launch.data_dir.clone(),
                "snapshot_restore": false,
            }))?
        );
    } else {
        println!(
            "Starting preview `{}` for scenario `{}` (data: {})",
            launch.component,
            launch.scenario_id,
            launch.data_dir.display()
        );
    }
    super::live::handle_preview(&project, &args.target, launch)
}

fn resolve_project_path(root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn fixture_path(
    file: &Path,
    project_root: &Path,
    scenario: &ScenarioDefinition,
) -> Result<PathBuf> {
    let candidate = file
        .parent()
        .unwrap_or(project_root)
        .join(&scenario.fixture)
        .canonicalize()
        .with_context(|| format!("resolving fixture {}", scenario.fixture))?;
    if !candidate.starts_with(project_root) {
        bail!("fixture resolves outside the project root");
    }
    Ok(candidate)
}

fn preview_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("preview-{nanos:x}")
}

fn uncontrolled_inputs(scenario: &ScenarioDefinition) -> Vec<String> {
    let mut inputs = Vec::new();
    if scenario.clock == "real" {
        inputs.push("os.clock".to_string());
    }
    if scenario.random_seed.is_none() {
        inputs.push("uncontrolled.random".to_string());
    }
    if scenario.component == "LoginForm" {
        inputs.push("network".to_string());
    }
    inputs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_run_ids_are_path_safe() {
        let id = preview_run_id();
        assert!(id.starts_with("preview-"));
        assert!(
            id.strip_prefix("preview-")
                .unwrap()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        );
    }
}
