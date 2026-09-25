use crate::scenario;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args)]
pub struct ScenarioArgs {
    #[command(subcommand)]
    pub command: ScenarioCommand,
}

#[derive(Subcommand)]
pub enum ScenarioCommand {
    /// Validate a schema-v1 scenario file without launching an app
    Validate {
        /// Scenario file, relative to the current project by default
        #[arg(long, default_value = "gpui.scenarios.toml")]
        file: PathBuf,
        /// Emit the structured validation report as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn handle_scenario(args: ScenarioArgs) -> Result<()> {
    match args.command {
        ScenarioCommand::Validate { file, json } => validate(file, json),
    }
}

fn validate(file: PathBuf, json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("reading current project directory")?;
    let file = if file.is_absolute() {
        file
    } else {
        project_root.join(file)
    };
    let report = scenario::validate_file(&file, &project_root)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serializing scenario report")?
        );
    } else {
        println!("scenario file: {}", report.file.display());
        println!("valid: {}", report.valid);
        println!("scenarios: {}", report.scenarios.len());
        if let Some(hash) = &report.scenario_hash {
            println!("scenario_hash: {hash}");
        }
        for warning in &report.warnings {
            println!("warning [{}]: {}", warning.path, warning.message);
        }
        for error in &report.errors {
            if let Some(location) = &error.location {
                println!(
                    "error [{}] at {}:{}: {}",
                    error.path, location.line, location.column, error.message
                );
            } else {
                println!("error [{}]: {}", error.path, error.message);
            }
        }
    }
    if report.valid {
        Ok(())
    } else {
        bail!("scenario validation failed")
    }
}
