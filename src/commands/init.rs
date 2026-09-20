use anyhow::{Context, Result, bail};
use colored::*;
use inquire::{MultiSelect, Text};
use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::template::{Platform, ProjectConfig, UiFramework, add_platforms, scaffold};

/// Platforms offered by the wizard, in display order.
const PLATFORM_CHOICES: [(&str, Platform); 5] = [
    ("macOS (Metal)", Platform::MacOs),
    ("Windows (Direct3D / Vulkan)", Platform::Windows),
    ("Linux (Vulkan, Wayland / X11)", Platform::Linux),
    ("iOS (Metal + UIKit host)", Platform::IOs),
    ("Android (Vulkan + JNI host)", Platform::Android),
];

pub enum InitMode {
    /// Create a project in a fresh directory. Any option left as `None` is
    /// asked for interactively.
    New {
        name: Option<String>,
        path: Option<PathBuf>,
        /// Comma-separated target list, e.g. `macos,ios`.
        targets: Option<String>,
        title: Option<String>,
        bundle_id: Option<String>,
    },
    /// Add platforms to an existing generated project, detected from `gpui.toml`.
    Add {
        path: Option<PathBuf>,
        /// Comma-separated targets to add, e.g. `ios,android` (skips the picker)
        targets: Option<String>,
    },
}

pub fn handle_init(mode: InitMode) -> Result<()> {
    match mode {
        InitMode::New {
            name,
            path,
            targets,
            title,
            bundle_id,
        } => create_new(name, path, targets, title, bundle_id),
        InitMode::Add { path, targets } => add_to_existing(path, targets),
    }
}

/// Parses a `--targets` list, rejecting unknown names.
fn parse_targets(spec: &str) -> Result<Vec<Platform>> {
    let mut out: Vec<Platform> = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let platform = Platform::parse(part).with_context(|| {
            format!("unknown target '{part}' (expected macos, windows, linux, ios or android)")
        })?;
        if !out.contains(&platform) {
            out.push(platform);
        }
    }
    if out.is_empty() {
        bail!("--targets was given but contained no target names");
    }
    Ok(out)
}

fn create_new(
    name: Option<String>,
    path: Option<PathBuf>,
    targets: Option<String>,
    title: Option<String>,
    bundle_id: Option<String>,
) -> Result<()> {
    println!(
        "{}",
        "\n🚀 Initializing new GPUI cross-platform project\n"
            .bold()
            .cyan()
    );

    let raw_name = match name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => {
            Text::new("Project name:")
                .with_default("my-gpui-app")
                .with_help_message("Used for Cargo package names and the project folder")
                .prompt()?
        }
        _ => bail!("No project name given and stdin is not a terminal. Pass a name argument."),
    };

    let project_name = sanitize_crate_name(&raw_name);
    if project_name.is_empty() {
        bail!("'{raw_name}' does not contain any valid characters for a package name");
    }
    if project_name != raw_name {
        println!("  {} using '{}'", "note:".yellow(), project_name.cyan());
    }

    let fallback_bundle = default_bundle_id(&project_name);
    // Without a terminal (CI, piped input) fall back to the defaults instead of
    // failing on a prompt that can never be answered.
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    let title = match title {
        Some(t) => t,
        None if interactive => Text::new("Application display title:")
            .with_default(&raw_name)
            .prompt()?,
        None => {
            println!("  {} title = {raw_name}", "default:".dimmed());
            raw_name.clone()
        }
    };

    let bundle_id = match bundle_id {
        Some(b) => b,
        None if interactive => Text::new("Bundle identifier:")
            .with_default(&fallback_bundle)
            .with_help_message("Reverse-DNS id, e.g. com.company.appname")
            .prompt()?,
        None => {
            println!("  {} bundle id = {fallback_bundle}", "default:".dimmed());
            fallback_bundle
        }
    };

    let targets = match targets {
        Some(spec) => parse_targets(&spec)?,
        None if interactive => prompt_platforms(&[])?,
        None => bail!(
            "No --targets given and stdin is not a terminal.\
             \n  Pass e.g. `--targets macos,ios,android`."
        ),
    };

    let target_dir = match path {
        Some(p) => p,
        None => env::current_dir()?.join(&project_name),
    };

    if target_dir.exists() {
        let mut entries = fs::read_dir(&target_dir)?.filter_map(|e| e.ok());
        if entries.next().is_some() {
            bail!(
                "Target directory '{}' already exists and is not empty.\n\
                 Use `gpui init --add` inside the project to add platforms instead.",
                target_dir.display()
            );
        }
    }

    let config = ProjectConfig {
        name: project_name.clone(),
        title,
        bundle_id,
        ui_framework: UiFramework::GpuiKit,
        targets,
    };

    println!("\n📦 Scaffolding project files...");
    scaffold(&target_dir, &config)?;

    report_success(&config, &target_dir, &project_name);
    Ok(())
}

/// Re-runs the platform picker against an existing project, adding whatever is
/// missing (desktop entry point, iOS and/or Android host directories).
fn add_to_existing(path: Option<PathBuf>, targets: Option<String>) -> Result<()> {
    let root = match path {
        Some(p) => p,
        None => env::current_dir()?,
    };
    let manifest_path = root.join("gpui.toml");
    if !manifest_path.exists() {
        bail!(
            "No gpui.toml in '{}'. `gpui init --add` must run at the root of a generated project.",
            root.display()
        );
    }

    let manifest = fs::read_to_string(&manifest_path)?;
    let name = read_toml_string(&manifest, "name")
        .context("gpui.toml is missing `name` in the [app] section")?;
    let title = read_toml_string(&manifest, "title").unwrap_or_else(|| name.clone());
    let bundle_id =
        read_toml_string(&manifest, "bundle_id").unwrap_or_else(|| default_bundle_id(&name));
    let existing = read_toml_targets(&manifest);

    println!(
        "{}",
        format!("\n➕ Adding platforms to '{}'\n", name)
            .bold()
            .cyan()
    );
    if !existing.is_empty() {
        println!(
            "  currently: {}",
            existing
                .iter()
                .map(|p| p.display_name())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let targets = match targets {
        // `--targets` is additive: keep what the project already had.
        Some(spec) => {
            let mut merged = existing.clone();
            for platform in parse_targets(&spec)? {
                if !merged.contains(&platform) {
                    merged.push(platform);
                }
            }
            merged
        }
        None if interactive => prompt_platforms(&existing)?,
        None => bail!(
            "No --targets given and stdin is not a terminal.\
             \n  Pass e.g. `--targets ios,android`."
        ),
    };

    let added: Vec<Platform> = targets
        .iter()
        .copied()
        .filter(|t| !existing.contains(t))
        .collect();

    if added.is_empty() {
        println!("\n{}", "Nothing to add.".yellow());
        return Ok(());
    }

    let config = ProjectConfig {
        name: name.clone(),
        title,
        bundle_id,
        ui_framework: UiFramework::GpuiKit,
        targets,
    };

    let previous = ProjectConfig {
        targets: existing,
        ..config.clone()
    };
    println!("\n📦 Adding platform files...");
    add_platforms(&root, &previous, &config)?;

    println!(
        "{}",
        format!(
            "\n✨ Added: {}\n",
            added
                .iter()
                .map(|p| p.display_name())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .bold()
        .green()
    );
    Ok(())
}

fn prompt_platforms(preselect: &[Platform]) -> Result<Vec<Platform>> {
    let options: Vec<String> = PLATFORM_CHOICES
        .iter()
        .map(|(label, _)| (*label).to_string())
        .collect();

    let defaults: Vec<usize> = PLATFORM_CHOICES
        .iter()
        .enumerate()
        .filter(|(_, (_, platform))| {
            if preselect.is_empty() {
                // Default to the desktop platforms on a fresh project.
                platform.is_desktop()
            } else {
                preselect.contains(platform)
            }
        })
        .map(|(i, _)| i)
        .collect();

    let selected = MultiSelect::new("Select target platforms:", options)
        .with_default(&defaults)
        .with_help_message("space toggles a platform, enter confirms")
        .prompt()?;

    let targets: Vec<Platform> = PLATFORM_CHOICES
        .iter()
        .filter(|(label, _)| selected.iter().any(|s| s == label))
        .map(|(_, platform)| *platform)
        .collect();

    if targets.is_empty() {
        bail!("At least one target platform must be selected.");
    }
    Ok(targets)
}

fn report_success(config: &ProjectConfig, target_dir: &Path, project_name: &str) {
    println!("{}", "\n✨ Project created successfully!\n".bold().green());

    let in_place = target_dir == env::current_dir().unwrap_or_default();
    println!("Next steps:");
    if !in_place {
        println!("  cd {}", project_name.cyan());
    }
    println!("  cargo fetch          # resolves the pinned GPUI revisions");
    if config.has_desktop() {
        println!("  gpui run desktop");
    }
    if config.has_ios() {
        println!("  gpui run ios         # needs XcodeGen + iOS Rust targets");
    }
    if config.has_android() {
        println!("  gpui run android     # needs cargo-ndk + Android SDK/NDK");
    }
    println!("\n  gpui doctor          # verify your toolchains first\n");
}

/// Lowercases and replaces invalid characters, then trims separators.
fn sanitize_crate_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true; // avoid a leading dash
    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn default_bundle_id(project_name: &str) -> String {
    let leaf = project_name.replace('-', "");
    format!("com.example.{leaf}")
}

/// Minimal `key = "value"` reader for the `[app]` section of `gpui.toml`.
fn read_toml_string(contents: &str, key: &str) -> Option<String> {
    let section = contents.split("[app]").nth(1)?;
    let section = section.split("\n[").next()?;
    for line in section.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn read_toml_targets(contents: &str) -> Vec<Platform> {
    let Some(section) = contents.split("[app]").nth(1) else {
        return Vec::new();
    };
    let Some(section) = section.split("\n[").next() else {
        return Vec::new();
    };
    for line in section.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("targets")
            && let Some(rest) = rest.trim_start().strip_prefix('=')
        {
            return rest
                .split(['[', ']', ',', '"'])
                .filter_map(|part| {
                    let part = part.trim();
                    if part.is_empty() {
                        None
                    } else {
                        Platform::parse(part)
                    }
                })
                .collect();
        }
    }
    Vec::new()
}
