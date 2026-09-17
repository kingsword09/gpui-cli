use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

pub mod commands;
pub mod template;

/// Options shared by `init` and `new`.
#[derive(clap::Args, Default)]
pub struct InitArgs {
    /// Project name; skips the name prompt
    pub name: Option<String>,
    /// Directory to create the project in (default: ./<name>)
    #[arg(short, long)]
    pub path: Option<PathBuf>,
    /// Comma-separated targets, e.g. `macos,ios,android` (skips the picker)
    #[arg(long, value_name = "LIST")]
    pub targets: Option<String>,
    /// Window/app title (defaults to the project name)
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,
    /// Bundle identifier (defaults to com.example.<name>)
    #[arg(long, value_name = "ID")]
    pub bundle_id: Option<String>,
}

#[derive(Parser)]
#[command(
    name = "gpui",
    author = "Kingsword <kingsword09@gmail.com>",
    version = env!("CARGO_PKG_VERSION"),
    about = "Scaffold and run cross-platform GPUI apps (macOS, Windows, Linux, iOS, Android)",
    long_about = "Scaffold and run cross-platform GPUI apps.\n\n\
                  Desktop platforms share one Cargo binary; iOS and Android get their\n\
                  own host projects (an Xcode scheme and a Gradle module) generated\n\
                  alongside the Rust workspace.",
    subcommand_required = false,
    arg_required_else_help = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new GPUI project (interactive wizard)
    Init {
        #[command(flatten)]
        args: InitArgs,
        /// Add platforms to an existing project in the current directory
        #[arg(long)]
        add: bool,
    },
    /// Alias for `init`
    New {
        #[command(flatten)]
        args: InitArgs,
    },
    /// Check the Rust toolchain and mobile build prerequisites
    Doctor,
    /// Run the app: desktop, ios or android
    Run {
        /// Target: desktop, ios, android
        #[arg(default_value = "desktop")]
        target: String,
        /// Build in release mode
        #[arg(short, long)]
        release: bool,
    },
    /// Build without launching: all, desktop, ios, android
    Build {
        /// Target: all, desktop, ios, android
        #[arg(default_value = "all")]
        target: String,
        /// Build in release mode
        #[arg(short, long)]
        release: bool,
    },
    /// Print the project metadata read from gpui.toml
    Info,
    /// Generate shell completions
    Completions {
        /// Shell: bash, zsh, fish, powershell, elvish
        shell: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Init { args, add: true }) => {
            commands::init::handle_init(commands::init::InitMode::Add {
                path: args.path,
                targets: args.targets,
            })?
        }
        Some(Commands::Init { args, add: false }) => {
            commands::init::handle_init(commands::init::InitMode::New {
                name: args.name,
                path: args.path,
                targets: args.targets,
                title: args.title,
                bundle_id: args.bundle_id,
            })?
        }
        Some(Commands::New { args }) => {
            commands::init::handle_init(commands::init::InitMode::New {
                name: args.name,
                path: args.path,
                targets: args.targets,
                title: args.title,
                bundle_id: args.bundle_id,
            })?
        }
        Some(Commands::Doctor) => commands::doctor::handle_doctor()?,
        Some(Commands::Run { target, release }) => {
            commands::run::handle_run(Some(target), release)?
        }
        Some(Commands::Build { target, release }) => {
            commands::build::handle_build(Some(target), release)?
        }
        Some(Commands::Info) => commands::info::handle_info()?,
        Some(Commands::Completions { shell }) => commands::completions::handle_completions(shell)?,
        None => {
            println!(
                "{}",
                "No command given. Starting the project wizard...\n".dimmed()
            );
            commands::init::handle_init(commands::init::InitMode::New {
                name: None,
                path: None,
                targets: None,
                title: None,
                bundle_id: None,
            })?;
        }
    }

    Ok(())
}
