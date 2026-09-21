use anyhow::Result;
use clap::{Args, Subcommand};
use colored::*;

use crate::upgrade::{PlanStatus, UpgradePlan, plan_project};

#[derive(Subcommand)]
pub enum UpgradeCommands {
    /// Produce a read-only three-way template upgrade plan.
    Plan(UpgradePlanArgs),
}

#[derive(Args)]
pub struct UpgradePlanArgs {
    /// Target embedded template version, for example agent-native-v1-draft.
    #[arg(long, value_name = "TEMPLATE_VERSION")]
    pub to: String,
    /// Print the complete plan as JSON.
    #[arg(long)]
    pub json: bool,
}

pub fn handle_upgrade(command: UpgradeCommands) -> Result<i32> {
    match command {
        UpgradeCommands::Plan(args) => handle_plan(args),
    }
}

fn handle_plan(args: UpgradePlanArgs) -> Result<i32> {
    let root = std::env::current_dir()?;
    let plan = plan_project(&root, &args.to)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        print_human(&plan);
    }
    Ok(match plan.status {
        PlanStatus::Ready => 0,
        PlanStatus::Conflict
        | PlanStatus::ManualMigrationRequired
        | PlanStatus::BaselineUnavailable => 1,
    })
}

fn print_human(plan: &UpgradePlan) {
    println!(
        "{}",
        format!("\n⬆️  Upgrade plan for '{}'", plan.project)
            .bold()
            .cyan()
    );
    println!("  plan_id: {}", plan.plan_id);
    println!("  status: {:?}", plan.status);
    println!(
        "  base: {} ({:?})",
        plan.base.template_version, plan.base_resolution
    );
    println!("  target: {}", plan.target.template_version);

    let mut counts = std::collections::BTreeMap::new();
    for file in &plan.files {
        *counts.entry(format!("{:?}", file.action)).or_insert(0usize) += 1;
    }
    if !counts.is_empty() {
        println!(
            "  files: {}",
            serde_json::to_string(&counts).unwrap_or_default()
        );
    }
    if !plan.toolchain_changes.is_empty() {
        println!("  toolchain changes: {}", plan.toolchain_changes.len());
    }
    for note in &plan.notes {
        println!("  {} {}", "note:".yellow(), note);
    }
    println!("  validation:");
    for command in &plan.validation_commands {
        println!("    {}", command.join(" "));
    }
    println!();
}
