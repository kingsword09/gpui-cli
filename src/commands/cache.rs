use clap::{Args, Subcommand};
use colored::Colorize;
use std::env;

use crate::runner::build_cache::clean_build_cache;

#[derive(Subcommand)]
pub enum CacheCommands {
    /// Remove old completed BuildKey outputs until the byte budget is met
    Clean(CacheCleanArgs),
}

#[derive(Args)]
pub struct CacheCleanArgs {
    /// Maximum total bytes to retain across recognized BuildKey outputs
    #[arg(long, value_name = "BYTES")]
    pub max_bytes: u64,
    /// Report candidates without creating lock files or deleting anything
    #[arg(long)]
    pub dry_run: bool,
}

pub fn handle_cache(command: CacheCommands) -> anyhow::Result<()> {
    match command {
        CacheCommands::Clean(args) => {
            let builds_root = env::current_dir()?.join(".gpui/builds");
            let report = clean_build_cache(&builds_root, args.max_bytes, args.dry_run)?;
            let mode = if report.dry_run {
                "would clean"
            } else {
                "cleaned"
            };
            println!(
                "{} cache: {mode} {} key(s), evicted {} byte(s), {} byte(s) remaining (budget {})",
                "✓".green(),
                report.keys_cleaned,
                report.evicted_bytes,
                report.remaining_bytes,
                args.max_bytes,
            );
            if report.active_keys_skipped > 0 || report.unsafe_keys_skipped > 0 {
                println!(
                    "  skipped {} active key(s), {} unsafe key(s)",
                    report.active_keys_skipped, report.unsafe_keys_skipped
                );
            }
        }
    }
    Ok(())
}
