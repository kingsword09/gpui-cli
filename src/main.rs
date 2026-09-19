use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

pub mod commands;
pub mod device;
pub mod devserver;
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

/// Device selection, shared by `run` and `build`.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct DeviceArgs {
    /// Target a specific device: a UDID, a serial, or an AVD/simulator name
    #[arg(long, value_name = "ID")]
    pub device: Option<String>,
    /// iOS simulator, by name and optional runtime, e.g. `iPhone 17 Pro@26.2`
    #[arg(long, value_name = "NAME[@RUNTIME]")]
    pub sim: Option<String>,
    /// Android virtual device name
    #[arg(long, value_name = "NAME")]
    pub avd: Option<String>,
    /// Require a physical iOS device
    #[arg(long)]
    pub device_only: bool,
}

impl From<DeviceArgs> for device::DeviceFlags {
    fn from(args: DeviceArgs) -> Self {
        device::DeviceFlags {
            device: args.device,
            sim: args.sim,
            avd: args.avd,
            device_only: args.device_only,
        }
    }
}

#[derive(Subcommand)]
pub enum DeviceCommands {
    /// List installed simulators, emulators and physical devices
    List {
        /// Restrict to one platform
        #[arg(long, value_name = "ios|android")]
        platform: Option<String>,
        /// Include devices that cannot currently be used
        #[arg(long)]
        all: bool,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Create a new simulator or emulator
    Create {
        /// Platform to create for
        #[arg(long, value_name = "ios|android")]
        platform: String,
        /// Name of the new device
        #[arg(long, value_name = "NAME")]
        name: String,
        /// Android system image package, e.g. `system-images;android-36;google_apis_playstore;arm64-v8a`
        #[arg(long, value_name = "PACKAGE")]
        image: Option<String>,
        /// Android hardware profile, e.g. `pixel_9_pro`
        #[arg(long, value_name = "PROFILE")]
        device: Option<String>,
        /// Android: create from a first-party `android` CLI profile instead of an image
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
        /// iOS device type, e.g. `iPhone 17 Pro`
        #[arg(long = "type", value_name = "MODEL")]
        device_type: Option<String>,
        /// iOS runtime version, e.g. `26.2`
        #[arg(long, value_name = "VERSION")]
        runtime: Option<String>,
    },
    /// Boot a device, waiting until it is ready
    Boot {
        /// Device id, name or serial
        id: Option<String>,
        /// Boot the most recently used device
        #[arg(long)]
        last: bool,
    },
    /// Shut down a simulator or emulator
    Shutdown {
        /// Device id, name or serial
        id: Option<String>,
        /// Shut down every running simulator and emulator
        #[arg(long)]
        all: bool,
    },
    /// Delete a simulator or emulator
    Remove {
        /// Device id, name or serial
        id: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
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
        /// Watch sources, rebuild and relaunch on every save (debug only)
        #[arg(long)]
        live: bool,
        #[command(flatten)]
        device: DeviceArgs,
    },
    /// Build without launching: all, desktop, ios, android
    Build {
        /// Target: all, desktop, ios, android
        #[arg(default_value = "all")]
        target: String,
        /// Build in release mode
        #[arg(short, long)]
        release: bool,
        #[command(flatten)]
        device: DeviceArgs,
    },
    /// Discover, create and manage simulators, emulators and devices
    Device {
        #[command(subcommand)]
        command: DeviceCommands,
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
        Some(Commands::Run {
            target,
            release,
            live,
            device,
        }) => commands::run::handle_run(Some(target), release, live, device.into())?,
        Some(Commands::Build {
            target,
            release,
            device,
        }) => commands::build::handle_build(Some(target), release, device.into())?,
        Some(Commands::Device { command }) => commands::device::handle_device(command)?,
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
