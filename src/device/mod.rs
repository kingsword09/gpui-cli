//! Device discovery, selection and lifecycle for iOS and Android.
//!
//! This replaces the text-scraping helpers that used to live in
//! `commands/run.rs` with machine-readable sources: `simctl --json`,
//! `devicectl --json-output`, `~/.android/avd/*/config.ini` and
//! `adb devices -l`.

use anyhow::{bail, Context, Result};
use colored::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod android;
pub mod inventory;
pub mod ios;

pub use inventory::{resolve_device, Defaults};

// ── Model ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Ios,
    Android,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Ios => "ios",
            Platform::Android => "android",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Platform::Ios => "iOS",
            Platform::Android => "Android",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ios" => Some(Platform::Ios),
            "android" => Some(Platform::Android),
            _ => None,
        }
    }
}

/// Whether a device is real hardware or a simulator/emulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Physical,
    Emulator,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Physical => "device",
            Kind::Emulator => "emulator",
        }
    }
}

/// A device that can be listed is not necessarily a device that can be used:
/// an iPhone can be paired while its tunnel is down, and an AVD can reference a
/// system image that is no longer installed. Modelling that explicitly keeps the
/// failure out of the boot/install path, where it would otherwise surface as an
/// opaque `devicectl`/`adb` error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum State {
    Running {
        #[serde(skip_serializing_if = "Option::is_none")]
        serial: Option<String>,
    },
    Stopped,
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub platform: Platform,
    pub kind: Kind,
    /// Stable primary key: a simulator UDID, an Android serial, or an AVD name.
    pub id: String,
    pub name: String,
    /// Display runtime, e.g. `iOS 26.2` or `android-36`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// e.g. `arm64-v8a`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    pub state: State,
    /// Factual extras shown in listings (hardware model, system image, ...).
    pub detail: String,
    /// Last time this device was seen in use, as epoch seconds. Sourced from
    /// `simctl`'s `lastBootedAt`, `devicectl`'s `lastConnectionDate` and the
    /// AVD directory mtime. Backs `gpui device boot --last`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
}

impl Device {
    /// Only launchable devices may be offered as candidates.
    pub fn launchable(&self) -> bool {
        !matches!(self.state, State::Unavailable { .. })
    }

    pub fn serial(&self) -> Option<&str> {
        match &self.state {
            State::Running { serial: Some(s) } => Some(s),
            _ => None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, State::Running { .. })
    }

    /// Name plus runtime, which is what disambiguates same-named devices across
    /// runtimes (`iPhone 16e` exists on both iOS 18.4 and iOS 26.2).
    pub fn label(&self) -> String {
        match &self.runtime {
            Some(rt) => format!("{} · {}", self.name, rt),
            None => self.name.clone(),
        }
    }

    pub fn state_label(&self) -> String {
        match &self.state {
            State::Running { serial: Some(s) } => format!("running ({s})"),
            State::Running { serial: None } => "running".to_string(),
            State::Stopped => "stopped".to_string(),
            State::Unavailable { reason } => format!("unavailable: {reason}"),
        }
    }
}

/// Both `simctl` and `devicectl` report timestamps as RFC 3339 UTC. Only that
/// one shape is needed, so this avoids a date/time dependency.
pub fn parse_rfc3339_utc(text: &str) -> Option<u64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num =
        |range: std::ops::Range<usize>| -> Option<i64> { text.get(range)?.parse::<i64>().ok() };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);

    // Howard Hinnant's days-from-civil algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    u64::try_from(seconds).ok()
}

/// Directory mtime as epoch seconds, the only recency signal AVDs expose.
pub fn path_mtime(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// ── Toolchain paths ──────────────────────────────────────────────────────────

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// `ANDROID_HOME` / `ANDROID_SDK_ROOT`, verified to exist.
pub fn android_sdk() -> Option<PathBuf> {
    std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// An executable inside the SDK. `emulator`, `avdmanager` and `sdkmanager` are
/// frequently absent from PATH, so they are resolved from `ANDROID_HOME`.
pub fn android_sdk_tool(relative: &str) -> Option<PathBuf> {
    let path = android_sdk()?.join(relative);
    path.is_file().then_some(path)
}

pub fn android_sdk_tool_any(candidates: &[&str]) -> Option<PathBuf> {
    candidates.iter().find_map(|c| android_sdk_tool(c))
}

pub fn avdmanager() -> Option<PathBuf> {
    android_sdk_tool_any(&[
        &format!("cmdline-tools/latest/bin/{}", exe("avdmanager")),
        &format!("tools/bin/{}", exe("avdmanager")),
    ])
}

pub fn sdkmanager() -> Option<PathBuf> {
    android_sdk_tool_any(&[
        &format!("cmdline-tools/latest/bin/{}", exe("sdkmanager")),
        &format!("tools/bin/{}", exe("sdkmanager")),
    ])
}

pub fn emulator_binary() -> Option<PathBuf> {
    android_sdk_tool(&format!("emulator/{}", exe("emulator")))
}

pub fn adb() -> Option<PathBuf> {
    if let Ok(path) = which::which(exe("adb")) {
        return Some(path);
    }
    android_sdk_tool(&format!("platform-tools/{}", exe("adb")))
}

/// The first-party `android` CLI, when installed. Optional: everything it does
/// has a fallback, but `emulator start` blocks until the device is ready, which
/// saves hand-rolling the wait.
pub fn android_cli() -> Option<PathBuf> {
    which::which(exe("android")).ok()
}

pub fn xcrun() -> Option<PathBuf> {
    which::which("xcrun").ok()
}

// ── Process helpers ──────────────────────────────────────────────────────────

pub fn display_cmd(program: &Path, args: &[&str]) -> String {
    let name = program
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| program.to_str().unwrap_or("?"));
    if args.is_empty() {
        name.to_string()
    } else {
        format!("{name} {}", args.join(" "))
    }
}

/// Runs a command and returns stdout, failing on a non-zero exit status.
pub fn capture(program: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{}`", display_cmd(program, args)))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        bail!("`{}` failed: {}", display_cmd(program, args), detail.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs a command, returning stdout only on success. Used where a tool is
/// optional (older Xcode without `devicectl`, for instance).
pub fn try_capture(program: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs a command with inherited stdio, failing on a non-zero exit status.
pub fn run(program: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to spawn `{}`", program.display()))?;
    if !status.success() {
        bail!("`{}` failed", display_cmd(program, args));
    }
    Ok(())
}

/// Both stdin and stdout must be a terminal before prompting; otherwise a CI run
/// would block forever waiting for input that will never arrive.
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

// ── Selection flags ──────────────────────────────────────────────────────────

/// Device selection shared by `gpui run` and `gpui build`.
#[derive(Debug, Clone, Default)]
pub struct DeviceFlags {
    pub device: Option<String>,
    pub sim: Option<String>,
    pub avd: Option<String>,
    pub device_only: bool,
}

// ── Listing ─────────────────────────────────────────────────────────────────

pub fn print_devices(devices: &[Device], show_all: bool) {
    let visible: Vec<&Device> = devices
        .iter()
        .filter(|d| show_all || d.launchable())
        .collect();

    if visible.is_empty() {
        println!(
            "{}",
            "No devices found. Create one with `gpui device create --platform <ios|android>`."
                .dimmed()
        );
        return;
    }

    for platform in [Platform::Ios, Platform::Android] {
        let group: Vec<&&Device> = visible.iter().filter(|d| d.platform == platform).collect();
        if group.is_empty() {
            continue;
        }
        println!("\n{}", platform.display_name().bold());

        for device in group {
            let bullet = match device.state {
                State::Running { .. } => "●".green(),
                State::Stopped => "○".dimmed(),
                State::Unavailable { .. } => "⚠".yellow(),
            };
            let mut line = format!(
                "  {} {}  {}  {}",
                bullet,
                device.label().bold(),
                device.kind.label().dimmed(),
                device.state_label()
            );
            if !device.detail.is_empty() {
                line.push_str(&format!("  {}", device.detail.dimmed()));
            }
            println!("{line}");
            println!("      {}", device.id.dimmed());
        }
    }

    let hidden = devices.len() - visible.len();
    if hidden > 0 {
        println!(
            "\n{}",
            format!("{hidden} unusable device(s) hidden; pass --all to show them.").dimmed()
        );
    }
    println!();
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_utc_into_epoch_seconds() {
        // 2026-09-17T00:47:19Z, the value simctl reports for a booted device.
        let seconds = parse_rfc3339_utc("2026-09-17T00:47:19Z").unwrap();
        assert_eq!(seconds, 1_789_606_039);

        // Epoch itself.
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        // A leap day.
        assert_eq!(
            parse_rfc3339_utc("2024-02-29T00:00:00Z"),
            parse_rfc3339_utc("2024-03-01T00:00:00Z").map(|s| s - 86_400)
        );
    }

    #[test]
    fn parses_devicectl_timestamps_with_fractional_seconds() {
        // The trailing `.018Z` must not break parsing.
        let with_fraction = parse_rfc3339_utc("2025-04-30T08:06:45.018Z");
        assert_eq!(with_fraction, parse_rfc3339_utc("2025-04-30T08:06:45Z"));
    }

    #[test]
    fn rejects_unparseable_timestamps_instead_of_panicking() {
        assert_eq!(parse_rfc3339_utc(""), None);
        assert_eq!(parse_rfc3339_utc("not a date"), None);
        assert_eq!(parse_rfc3339_utc("2026-09"), None);
    }

    #[test]
    fn ordering_by_last_used_is_meaningful() {
        assert!(
            parse_rfc3339_utc("2026-09-17T00:47:19Z").unwrap()
                > parse_rfc3339_utc("2025-08-18T06:35:38Z").unwrap()
        );
    }
}
