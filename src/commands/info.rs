use anyhow::Result;
use colored::*;

use super::run::Project;

/// Prints the metadata `gpui init` recorded, plus which host projects exist.
pub fn handle_info() -> Result<()> {
    let project = Project::load(None)?;
    let manifest = std::fs::read_to_string(project.root.join("gpui.toml"))?;

    println!("{}", format!("\n📋 {}", project.title).bold().cyan());
    println!("{manifest}");

    println!("{}", "Detected targets:".bold());

    let desktop = project.has_desktop();
    let ios = project.ios_dir().exists();
    let android = project.android_gradle_dir().exists();

    let mark = |present: bool| {
        if present {
            "✓".green().to_string()
        } else {
            "✗".red().to_string()
        }
    };

    println!(
        "  {} desktop  (crates/desktop -> {})",
        mark(desktop),
        project.desktop_crate()
    );
    println!("  {} ios      (mobile/ios)", mark(ios));
    println!("  {} android  (mobile/android/gradle)", mark(android));

    if !desktop && !ios && !android {
        println!(
            "\n{}",
            "No host projects found. Run `gpui init --add`.".yellow()
        );
    }
    println!();

    Ok(())
}
