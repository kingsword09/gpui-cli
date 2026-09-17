use anyhow::{bail, Result};
use colored::*;

use super::run::{build_android_apk, build_desktop, build_ios_app, resolve_ios_target, Project};
use crate::device::DeviceFlags;

/// Dispatches `gpui build <target>`.
pub fn handle_build(target: Option<String>, release: bool, flags: DeviceFlags) -> Result<()> {
    let project = Project::load(None)?;
    let target = target
        .unwrap_or_else(|| "all".to_string())
        .to_ascii_lowercase();

    match target.as_str() {
        "all" => build_all(&project, release),
        "desktop" | "macos" | "windows" | "linux" => {
            banner(&project.title, "desktop");
            build_desktop(&project, release)
        }
        "ios" => {
            banner(&project.title, "ios");
            // The destination determines which Rust target is compiled, so the
            // device has to be resolved even for a build-only run.
            let ios_target = resolve_ios_target(&project, &flags)?;
            println!("  {} targeting {}", "→".blue(), ios_target.label());
            let app = build_ios_app(&project, &ios_target, release)?;
            println!("  {} bundle: {}", "✓".green(), app.display());
            Ok(())
        }
        "android" => {
            banner(&project.title, "android");
            let apk = build_android_apk(&project, release)?;
            println!("  {} apk: {}", "✓".green(), apk.display());
            Ok(())
        }
        other => bail!("Unknown target '{other}'. Valid targets: all, desktop, ios, android"),
    }
}

/// Builds every platform the project actually targets.
fn build_all(project: &Project, release: bool) -> Result<()> {
    let mut built = 0usize;

    if project.has_desktop() {
        banner(&project.title, "desktop");
        build_desktop(project, release)?;
        built += 1;
    }

    if project.ios_dir().exists() {
        banner(&project.title, "ios (simulator)");
        let ios_target = resolve_ios_target(project, &DeviceFlags::default())?;
        let app = build_ios_app(project, &ios_target, release)?;
        println!("  {} {}", "✓".green(), app.display());
        built += 1;
    }

    if project.android_gradle_dir().exists() {
        banner(&project.title, "android");
        let apk = build_android_apk(project, release)?;
        println!("  {} {}", "✓".green(), apk.display());
        built += 1;
    }

    if built == 0 {
        bail!("This project has no buildable target. Add one with `gpui init --add`.");
    }

    println!(
        "\n{}",
        format!("✅ Built {built} target(s) successfully.")
            .bold()
            .green()
    );
    Ok(())
}

fn banner(title: &str, target: &str) {
    println!(
        "{}",
        format!("\n🔨 Building '{title}' for {target}")
            .bold()
            .cyan()
    );
}
