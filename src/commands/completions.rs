use anyhow::Result;
use clap::CommandFactory;
use clap_complete::{Shell, generate};
use colored::*;
use std::io;

use crate::Cli;

pub fn handle_completions(shell: String) -> Result<()> {
    let shell = match shell.to_lowercase().as_str() {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        "powershell" => Shell::PowerShell,
        "elvish" => Shell::Elvish,
        other => {
            eprintln!(
                "{}",
                format!(
                    "Unsupported shell '{}'. Supported: bash, zsh, fish, powershell, elvish",
                    other
                )
                .red()
            );
            std::process::exit(1);
        }
    };

    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "gpui", &mut io::stdout());
    Ok(())
}
