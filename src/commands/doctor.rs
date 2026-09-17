use anyhow::Result;
use colored::*;
use std::process::Command;

/// Reports whether each toolchain `gpui run`/`gpui build` depends on is usable.
pub fn handle_doctor() -> Result<()> {
    println!("{}", "\n🩺 GPUI toolchain check\n".bold().cyan());

    // Anything required-but-absent, reported together at the end.
    let mut missing: Vec<String> = Vec::new();

    section("Rust");
    check_command("rustc", &["--version"], false, &mut missing);
    check_command("cargo", &["--version"], false, &mut missing);
    check_command("rustup", &["--version"], false, &mut missing);

    let host = host_target();
    println!("  host target: {}", host.bold());
    check_rust_target(&host, false, &mut missing);

    section("Desktop");
    // GPUI links against a C toolchain for its font/text stack.
    check_command("cc", &["--version"], true, &mut missing);

    section("iOS");
    if cfg!(target_os = "macos") {
        check_command("xcodebuild", &["-version"], true, &mut missing);
        check_command("xcodegen", &["--version"], true, &mut missing);
        check_rust_target("aarch64-apple-ios", true, &mut missing);
        check_rust_target("aarch64-apple-ios-sim", true, &mut missing);
        // Device discovery and launching go through these.
        check_command("xcrun", &["--version"], true, &mut missing);
        check_xcrun_subcommand("simctl", &mut missing);
        check_xcrun_subcommand("devicectl", &mut missing);
    } else {
        println!("  {} not available off macOS", "–".dimmed());
    }

    section("Android");
    check_command("cargo-ndk", &["--version"], true, &mut missing);
    check_rust_target("aarch64-linux-android", true, &mut missing);

    let sdk = android_sdk();
    match &sdk {
        Some(path) => println!("  {} ANDROID_HOME: {}", "✓".green(), path),
        None => {
            println!("  {} ANDROID_HOME / ANDROID_SDK_ROOT is not set", "✗".red());
            missing.push("ANDROID_HOME / ANDROID_SDK_ROOT".into());
        }
    }
    match android_ndk() {
        Some(path) => println!("  {} ANDROID_NDK_HOME: {}", "✓".green(), path),
        None => {
            println!(
                "  {} ANDROID_NDK_HOME / NDK_HOME is not set (required by cargo-ndk)",
                "✗".red()
            );
            missing.push("ANDROID_NDK_HOME / NDK_HOME".into());
        }
    }
    check_command("adb", &["version"], true, &mut missing);

    // These live inside the SDK and are frequently absent from PATH, so they
    // are resolved by path. They are only needed to manage devices.
    check_sdk_tool(&sdk, "emulator/emulator", "emulator", &mut missing);
    check_sdk_tool(
        &sdk,
        "cmdline-tools/latest/bin/avdmanager",
        "avdmanager",
        &mut missing,
    );
    check_sdk_tool(
        &sdk,
        "cmdline-tools/latest/bin/sdkmanager",
        "sdkmanager",
        &mut missing,
    );
    // The first-party `android` CLI is optional; `gpui device boot` prefers it.
    check_command("android", &["info"], false, &mut missing);

    println!();
    if missing.is_empty() {
        println!(
            "{}",
            "Everything needed for your targets is installed.".green()
        );
    } else {
        println!("{}", "Missing:".yellow().bold());
        for item in &missing {
            println!("  {} {item}", "•".yellow());
        }
    }
    println!(
        "{}",
        "Only the toolchains for the platforms you selected are required.\n".dimmed()
    );

    Ok(())
}

fn section(title: &str) {
    println!("\n{}", title.bold());
}

fn host_target() -> String {
    let output = Command::new("rustc").args(["-vV"]).output();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("host: ") {
                return rest.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

fn android_sdk() -> Option<String> {
    std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .ok()
        .filter(|s| !s.trim().is_empty() && std::path::Path::new(s).exists())
}

fn android_ndk() -> Option<String> {
    std::env::var("ANDROID_NDK_HOME")
        .or_else(|_| std::env::var("NDK_HOME"))
        .ok()
        .filter(|s| !s.trim().is_empty() && std::path::Path::new(s).exists())
}

/// Prints a line for an external command, recording required misses.
fn check_command(cmd: &str, args: &[&str], required: bool, missing: &mut Vec<String>) {
    match which::which(cmd) {
        Ok(path) => {
            let version = Command::new(&path)
                .args(args)
                .output()
                .ok()
                .map(|out| {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let text = if stdout.trim().is_empty() {
                        stderr
                    } else {
                        stdout
                    };
                    text.lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .trim()
                        .to_string()
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| path.display().to_string());
            println!("  {} {}  {}", "✓".green(), cmd.bold(), version.dimmed());
        }
        Err(_) => {
            let tag = if required {
                "✗".red().to_string()
            } else {
                "–".dimmed().to_string()
            };
            println!("  {} {}  not found on PATH", tag, cmd.bold());
            if required {
                missing.push(format!("{cmd} (not on PATH)"));
            }
        }
    }
}

/// Checks an `xcrun` subcommand, which is how `simctl` and `devicectl` are
/// invoked (they are not standalone binaries). `help` is used rather than
/// `--help` because `simctl --help` exits non-zero.
fn check_xcrun_subcommand(subcommand: &str, missing: &mut Vec<String>) {
    let output = Command::new("xcrun").args([subcommand, "help"]).output();
    let ok = output.map(|out| out.status.success()).unwrap_or(false);
    if ok {
        println!("  {} xcrun {}", "✓".green(), subcommand.bold());
    } else {
        println!(
            "  {} xcrun {}  not available (update Xcode)",
            "✗".red(),
            subcommand.bold()
        );
        missing.push(format!("xcrun {subcommand}"));
    }
}

/// Reports an executable that ships inside the Android SDK but is often not on
/// PATH. Optional, since it is only needed to manage devices.
fn check_sdk_tool(sdk: &Option<String>, relative: &str, label: &str, missing: &mut Vec<String>) {
    let found = sdk
        .as_ref()
        .map(|sdk| std::path::Path::new(sdk).join(relative))
        .filter(|path| path.is_file());

    match found {
        Some(path) => println!("  {} {}  {}", "✓".green(), label.bold(), path.display()),
        None if which::which(label).is_ok() => {
            println!("  {} {}  found on PATH", "✓".green(), label.bold())
        }
        None => {
            println!(
                "  {} {}  not found in the SDK (device management unavailable)",
                "–".dimmed(),
                label.bold()
            );
            if sdk.is_none() {
                missing.push(format!("{label} (set ANDROID_HOME)"));
            }
        }
    }
}

fn check_rust_target(target: &str, required: bool, missing: &mut Vec<String>) {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output();
    let installed = output
        .ok()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l.trim() == target)
        })
        .unwrap_or(false);

    if installed {
        println!("  {} target {}", "✓".green(), target);
    } else {
        let hint = format!("rustup target add {target}");
        if required {
            println!("  {} target {}  ({})", "✗".red(), target, hint);
            missing.push(format!("Rust target {target}"));
        } else {
            println!("  {} target {}  ({})", "–".dimmed(), target, hint);
        }
    }
}
