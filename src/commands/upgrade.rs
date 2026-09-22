use anyhow::Result;
use clap::{Args, Subcommand};
use colored::*;

use crate::upgrade::{
    PlanStatus, UpgradePlan, plan_project, save_plan,
    transaction::{RecoveryReport, recover_transaction},
};

#[derive(Subcommand)]
pub enum UpgradeCommands {
    /// Produce a read-only three-way template upgrade plan.
    Plan(UpgradePlanArgs),
    /// Apply a previously saved, validated upgrade plan.
    Apply(UpgradeApplyArgs),
    /// Recover an interrupted upgrade transaction.
    Recover(UpgradeRecoverArgs),
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

#[derive(Args)]
pub struct UpgradeApplyArgs {
    /// Saved plan id.
    #[arg(long, value_name = "PLAN_ID")]
    pub plan: String,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct UpgradeRecoverArgs {
    /// Transaction id recorded under the project upgrade directory.
    #[arg(long, value_name = "TRANSACTION_ID")]
    pub transaction: String,
    /// Print the result as JSON.
    #[arg(long)]
    pub json: bool,
}

pub fn handle_upgrade(command: UpgradeCommands) -> Result<i32> {
    match command {
        UpgradeCommands::Plan(args) => handle_plan(args),
        UpgradeCommands::Apply(args) => handle_apply(args),
        UpgradeCommands::Recover(args) => handle_recover(args),
    }
}

fn handle_plan(args: UpgradePlanArgs) -> Result<i32> {
    let root = std::env::current_dir()?;
    let plan = plan_project(&root, &args.to)?;
    let plan_path = save_plan(&root, &plan)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        print_human(&plan);
        println!("  saved: {}", plan_path.display());
    }
    Ok(match plan.status {
        PlanStatus::Ready => 0,
        PlanStatus::Conflict
        | PlanStatus::ManualMigrationRequired
        | PlanStatus::BaselineUnavailable => 1,
    })
}

fn handle_apply(args: UpgradeApplyArgs) -> Result<i32> {
    let root = std::env::current_dir()?;
    let report = crate::upgrade::apply::apply_cached_plan(&root, &args.plan)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}",
            format!(
                "\\n✓ upgrade transaction {} committed",
                report.transaction_id
            )
            .green()
        );
    }
    Ok(0)
}

fn handle_recover(args: UpgradeRecoverArgs) -> Result<i32> {
    let root = std::env::current_dir()?;
    let report: RecoveryReport = recover_transaction(&root, &args.transaction)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("transaction {}: {:?}", report.transaction_id, report.state);
        for path in &report.restored {
            println!("  restored {}", path);
        }
        for path in &report.preserved_user_changes {
            println!("  preserved user change {}", path);
        }
        for error in &report.errors {
            println!("  {} {}", "error:".red(), error);
        }
    }
    Ok((report.state == crate::upgrade::transaction::TransactionState::RecoveryRequired) as i32)
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
