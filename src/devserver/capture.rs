//! Platform capture providers used by observation operations.

use anyhow::bail;
#[cfg(target_os = "macos")]
use anyhow::{Context, anyhow};
#[cfg(target_os = "macos")]
use std::process::{Command, Output, Stdio};
#[cfg(target_os = "macos")]
use std::thread;
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct WindowCapture {
    pub window_number: u32,
    pub bounds: WindowBounds,
    pub bytes: Vec<u8>,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct WindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn window_capture_available() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(target_os = "macos")]
pub fn capture_window(pid: u32, title: &str, timeout: Duration) -> anyhow::Result<WindowCapture> {
    use std::io::Read;

    const ENUMERATE_WINDOW: &str = r#"
import AppKit
import CoreGraphics
import Foundation

let environment = ProcessInfo.processInfo.environment
guard let pid = environment["GPUI_CAPTURE_PID"].flatMap(Int.init) else { exit(2) }
let expectedTitle = environment["GPUI_CAPTURE_TITLE"] ?? ""
let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows {
    guard (window[kCGWindowOwnerPID as String] as? NSNumber)?.intValue == pid,
          (window[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
          let number = (window[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
          let name = window[kCGWindowName as String] as? String,
          name == expectedTitle,
          let bounds = window[kCGWindowBounds as String] as? [String: Any],
          let x = (bounds["X"] as? NSNumber)?.intValue,
          let y = (bounds["Y"] as? NSNumber)?.intValue,
          let width = (bounds["Width"] as? NSNumber)?.uint32Value,
          let height = (bounds["Height"] as? NSNumber)?.uint32Value else { continue }
    print("\(number)\t\(x)\t\(y)\t\(width)\t\(height)")
    exit(0)
}
exit(3)
"#;

    let started_at_ms = epoch_ms();
    let started = Instant::now();
    let enumeration = run_bounded(
        Command::new("/usr/bin/swift")
            .arg("-e")
            .arg(ENUMERATE_WINDOW)
            .env("GPUI_CAPTURE_PID", pid.to_string())
            .env("GPUI_CAPTURE_TITLE", title)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        timeout,
    )
    .context("enumerating the target macOS window")?;
    if enumeration.status.code() == Some(3) {
        bail!("no visible macOS window matched the app PID and title");
    }
    if !enumeration.status.success() {
        let detail = String::from_utf8_lossy(&enumeration.stderr);
        bail!("macOS window enumeration failed: {}", detail.trim());
    }
    let line = String::from_utf8_lossy(&enumeration.stdout)
        .lines()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("no visible macOS window matched the app PID and title"))?;
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 5 {
        bail!("macOS window enumeration returned malformed bounds");
    }
    let window_number = fields[0].parse::<u32>()?;
    let bounds = WindowBounds {
        x: fields[1].parse::<i32>()?,
        y: fields[2].parse::<i32>()?,
        width: fields[3].parse::<u32>()?,
        height: fields[4].parse::<u32>()?,
    };
    if bounds.width == 0 || bounds.height == 0 {
        bail!("the matched macOS window has empty bounds");
    }

    let output_dir = tempfile::tempdir().context("creating a private screenshot directory")?;
    let png_path = output_dir.path().join("window.png");
    let remaining = timeout.saturating_sub(started.elapsed());
    let output = run_bounded(
        Command::new("/usr/sbin/screencapture")
            .args(["-x", "-o", "-l"])
            .arg(window_number.to_string())
            .arg(&png_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
        remaining,
    )
    .context("capturing the selected macOS window")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("macOS window screenshot failed: {}", detail.trim());
    }
    let mut file =
        std::fs::File::open(&png_path).context("screencapture did not create the requested PNG")?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .context("reading the macOS window screenshot")?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("macOS window screenshot exceeds the 16 MiB artifact limit");
    }
    if bytes.is_empty() {
        bail!("macOS window screenshot is empty");
    }
    Ok(WindowCapture {
        window_number,
        bounds,
        bytes,
        started_at_ms,
        finished_at_ms: epoch_ms(),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn capture_window(
    _pid: u32,
    _title: &str,
    _timeout: Duration,
) -> anyhow::Result<WindowCapture> {
    bail!("macOS window capture is unavailable on this platform")
}

#[cfg(target_os = "macos")]
fn run_bounded(command: &mut Command, timeout: Duration) -> anyhow::Result<Output> {
    if timeout.is_zero() {
        bail!("window capture deadline elapsed");
    }
    let mut child = command.spawn().context("starting capture helper")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("waiting for capture helper")? {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                std::io::Read::read_to_end(&mut pipe, &mut stdout)?;
            }
            if let Some(mut pipe) = child.stderr.take() {
                std::io::Read::read_to_end(&mut pipe, &mut stderr)?;
            }
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("capture helper exceeded its deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_window_enumerator_compiles_and_rejects_an_unmatched_title() {
        let error = capture_window(
            std::process::id(),
            "gpui-cli-capture-test-window-that-does-not-exist",
            Duration::from_secs(30),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no visible macOS window matched"),
            "unexpected capture helper error: {error:#}"
        );
    }
}
