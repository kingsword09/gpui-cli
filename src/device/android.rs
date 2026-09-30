//! Android device discovery via AVD metadata, `avdmanager` and `adb`.
//!
//! `emulator`, `avdmanager` and `sdkmanager` are commonly absent from PATH, so
//! they are resolved from `ANDROID_HOME` (see `android_sdk_tool`). AVD
//! metadata is read straight from `~/.android/avd/<name>.avd/config.ini`, which
//! is both the most complete source and the only one that works without the
//! command line tools installed.

use anyhow::{Context, Result, bail};
use colored::*;
use std::fs::OpenOptions;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{Device, Kind, Platform, State, adb, emulator_binary, try_capture};

// ── config.ini ───────────────────────────────────────────────────────────────

/// The AVD directory, honouring `ANDROID_AVD_HOME` then `ANDROID_USER_HOME`.
pub fn avd_home() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ANDROID_AVD_HOME") {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return Some(path);
        }
    }
    if let Ok(home) = std::env::var("ANDROID_USER_HOME") {
        let path = PathBuf::from(home).join("avd");
        if path.is_dir() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".android/avd");
    path.is_dir().then_some(path)
}

/// Minimal `key = value` reader for an AVD's `config.ini`.
fn read_ini(path: &Path) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    Some(out)
}

fn ini_get<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A virtual device as declared on disk.
#[derive(Debug, Clone)]
pub struct Avd {
    pub name: String,
    /// e.g. `arm64-v8a`
    pub abi: Option<String>,
    /// e.g. `android-36`
    pub api: Option<String>,
    /// e.g. `system-images/android-36/google_apis_playstore_ps16k/arm64-v8a/`
    pub image: Option<String>,
    /// e.g. `pixel_9_pro`
    pub device: Option<String>,
    pub play_store: bool,
    /// The `<name>.avd` directory, which carries the only recency signal.
    pub dir: Option<PathBuf>,
}

impl Avd {
    /// `android-36` -> `36`
    pub fn api_level(&self) -> Option<u32> {
        self.api.as_deref()?.rsplit('-').next()?.parse().ok()
    }

    /// The `sdkmanager` package path for the image this AVD boots from.
    pub fn image_package(&self) -> Option<String> {
        let image = self.image.as_deref()?;
        let trimmed = image.trim_end_matches('/');
        // `system-images/android-36/google_apis_playstore_ps16k/arm64-v8a`
        let mut parts: Vec<&str> = trimmed.split('/').collect();
        if parts.first() != Some(&"system-images") {
            return None;
        }
        if parts.len() < 4 {
            return None;
        }
        let abi = parts.pop()?;
        let tag = parts.pop()?;
        let api = parts.pop()?;
        Some(format!("system-images;{api};{tag};{abi}"))
    }
}

pub fn avds() -> Vec<Avd> {
    let Some(home) = avd_home() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&home) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("avd") {
            continue;
        }
        let Some(ini) = read_ini(&path.join("config.ini")) else {
            continue;
        };
        let fallback = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        let image = ini_get(&ini, "image.sysdir.1").map(str::to_string);
        // The API level is in the image path, not a dedicated key.
        let api = image
            .as_deref()
            .and_then(|image| image.split('/').nth(1))
            .map(str::to_string);

        out.push(Avd {
            name: ini_get(&ini, "AvdId").unwrap_or(&fallback).to_string(),
            abi: ini_get(&ini, "abi.type").map(str::to_string),
            api,
            image,
            device: ini_get(&ini, "hw.device.name").map(str::to_string),
            play_store: ini_get(&ini, "PlayStore.enabled") == Some("true"),
            dir: Some(path.clone()),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Fallback AVD listing, for when `config.ini` is unreadable.
fn avd_names_from_manager() -> Vec<String> {
    let Some(manager) = super::avdmanager() else {
        return Vec::new();
    };
    try_capture(&manager, &["list", "avd", "-c"])
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with("Available") && !l.starts_with("---"))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ── Connected devices ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AdbDevice {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}

pub fn adb_devices() -> Vec<AdbDevice> {
    let Some(adb) = adb() else {
        return Vec::new();
    };
    let Some(text) = try_capture(&adb, &["devices", "-l"]) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        let state = parts.next().unwrap_or("").to_string();
        let model = parts
            .filter_map(|p| p.strip_prefix("model:"))
            .next()
            .map(str::to_string);
        out.push(AdbDevice {
            serial: serial.to_string(),
            state,
            model,
        });
    }
    out
}

/// Reads an Android system property from a specific device. Every adb call past
/// discovery is scoped with `-s`, so multiple connected devices are safe.
fn getprop(adb: &Path, serial: &str, prop: &str) -> Option<String> {
    let text = try_capture(adb, &["-s", serial, "shell", "getprop", prop])?;
    let value = text.trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// `android-36` stays as-is; a bare `36` becomes `android-36`.
fn api_to_runtime(api: &str) -> String {
    if api.starts_with("android-") {
        api.to_string()
    } else {
        format!("android-{api}")
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The AVD name an emulator serial belongs to. The property name differs across
/// emulator versions, so both are tried.
fn emulator_avd_name(adb: &Path, serial: &str) -> Option<String> {
    getprop(adb, serial, "ro.boot.qemu.avd_name")
        .or_else(|| getprop(adb, serial, "ro.kernel.qemu.avd_name"))
}

pub fn all() -> Vec<Device> {
    let mut devices = emulator_devices();
    devices.extend(connected_devices());
    devices
}

fn emulator_devices() -> Vec<Device> {
    let mut declared = avds();
    let declared_names: Vec<String> = declared.iter().map(|a| a.name.clone()).collect();
    for name in avd_names_from_manager() {
        if !declared_names.contains(&name) {
            declared.push(Avd {
                name,
                abi: None,
                api: None,
                image: None,
                device: None,
                play_store: false,
                dir: None,
            });
        }
    }

    let running = adb_devices();

    declared
        .into_iter()
        .map(|avd| {
            // A running AVD appears in `adb devices` as `emulator-<port>`; the
            // AVD name is recovered from the emulator's own properties.
            let serial = adb().and_then(|adb| {
                running
                    .iter()
                    .find(|d| {
                        d.state == "device"
                            && d.serial.starts_with("emulator-")
                            && emulator_avd_name(&adb, &d.serial).as_deref()
                                == Some(avd.name.as_str())
                    })
                    .map(|d| d.serial.clone())
            });

            // An AVD whose system image was since uninstalled cannot boot.
            let unavailable = match &avd.image {
                Some(image) => {
                    let image_dir =
                        super::android_sdk().map(|sdk| sdk.join(image.trim_end_matches('/')));
                    image_dir
                        .filter(|dir| !dir.is_dir())
                        .map(|dir| format!("system image missing ({})", dir.display()))
                }
                None => None,
            };

            let state = if let Some(reason) = unavailable {
                State::Unavailable { reason }
            } else if let Some(serial) = serial {
                State::Running {
                    serial: Some(serial),
                }
            } else {
                State::Stopped
            };

            let mut detail: Vec<String> = Vec::new();
            if let Some(device) = &avd.device {
                detail.push(device.replace('_', " "));
            }
            if let Some(image) = &avd.image
                && let Some(tag) = image.trim_end_matches('/').split('/').nth(2)
            {
                detail.push(tag.to_string());
            }
            if avd.play_store {
                detail.push("play store".to_string());
            }

            Device {
                platform: Platform::Android,
                kind: Kind::Emulator,
                id: avd.name.clone(),
                name: avd.name,
                runtime: avd.api.as_deref().map(api_to_runtime),
                arch: avd.abi.clone(),
                state,
                detail: detail.join(", "),
                last_used: avd.dir.as_deref().and_then(super::path_mtime),
            }
        })
        .collect()
}

fn connected_devices() -> Vec<Device> {
    let Some(adb) = adb() else {
        return Vec::new();
    };
    let avd_names: Vec<String> = avds().into_iter().map(|a| a.name).collect();

    adb_devices()
        .into_iter()
        .filter_map(|device| {
            let is_emulator = device.serial.starts_with("emulator-");
            let avd_name = is_emulator
                .then(|| emulator_avd_name(&adb, &device.serial))
                .flatten();

            // Emulators are reported by `emulator_devices` under their AVD name,
            // so skip them here unless the AVD is unknown to us.
            if let Some(name) = &avd_name
                && avd_names.contains(name)
            {
                return None;
            }

            let api = getprop(&adb, &device.serial, "ro.build.version.sdk");
            let abi = getprop(&adb, &device.serial, "ro.product.cpu.abi");
            let model = getprop(&adb, &device.serial, "ro.product.model").or_else(|| {
                device
                    .model
                    .as_deref()
                    .map(|m| m.replace('_', " ").trim().to_string())
            });
            let booted =
                getprop(&adb, &device.serial, "sys.boot_completed").as_deref() == Some("1");

            let state = match device.state.as_str() {
                "device" if booted || !is_emulator => State::Running {
                    serial: Some(device.serial.clone()),
                },
                "device" => State::Unavailable {
                    reason: "still booting".to_string(),
                },
                "unauthorized" => State::Unavailable {
                    reason: "USB debugging not authorized on the device".to_string(),
                },
                "offline" => State::Unavailable {
                    reason: "offline".to_string(),
                },
                other => State::Unavailable {
                    reason: other.to_string(),
                },
            };

            let name = if is_emulator {
                avd_name.unwrap_or_else(|| device.serial.clone())
            } else {
                model.clone().unwrap_or_else(|| device.serial.clone())
            };

            Some(Device {
                platform: Platform::Android,
                kind: if is_emulator {
                    Kind::Emulator
                } else {
                    Kind::Physical
                },
                id: device.serial.clone(),
                name,
                runtime: api.as_deref().map(api_to_runtime),
                arch: abi,
                state,
                detail: format!("serial {}", device.serial),
                // A connected device is in use right now.
                last_used: Some(now_epoch()),
            })
        })
        .collect()
}

// ── System images (for `create`) ─────────────────────────────────────────────

pub fn installed_system_images() -> Vec<String> {
    let Some(sdk) = super::android_sdk() else {
        return Vec::new();
    };
    let root = sdk.join("system-images");
    let Ok(apis) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for api in apis.flatten() {
        let Ok(tags) = std::fs::read_dir(api.path()) else {
            continue;
        };
        for tag in tags.flatten() {
            let Ok(abis) = std::fs::read_dir(tag.path()) else {
                continue;
            };
            for abi in abis.flatten() {
                let path = abi.path();
                let Ok(rel) = path.strip_prefix(&root) else {
                    continue;
                };
                out.push(format!(
                    "system-images;{}",
                    rel.to_string_lossy().replace('/', ";")
                ));
            }
        }
    }
    out.sort();
    out
}

/// Device profile ids accepted by `avdmanager create avd -d`.
pub fn device_profiles() -> Vec<String> {
    let Some(manager) = super::avdmanager() else {
        return Vec::new();
    };
    let Some(text) = try_capture(&manager, &["list", "device"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // `id: 12 or "pixel_9_pro"`
        if let Some(rest) = line.strip_prefix("id:")
            && let Some(id) = rest.split('"').nth(1)
        {
            out.push(id.to_string());
        }
    }
    out
}

// ── Lifecycle ────────────────────────────────────────────────────────────────

/// Creates an AVD via `avdmanager`, falling back to the first-party `android`
/// CLI's profile-based creation when only that is available.
pub fn create_avd(
    name: &str,
    image: Option<&str>,
    device: Option<&str>,
    profile: Option<&str>,
) -> Result<()> {
    // The `android` CLI cannot select an image; use it only when asked to.
    if image.is_none()
        && let Some(profile) = profile
    {
        let cli = super::android_cli().context(
            "the `android` CLI is not installed; pass --image to use avdmanager, or install it",
        )?;
        return super::run(
            &cli,
            &["emulator", "create", &format!("--profile={profile}")],
        );
    }

    let manager = super::avdmanager().context(
        "`avdmanager` was not found. Install the SDK command line tools, or set ANDROID_HOME.",
    )?;
    let image = image.context("--image is required when creating an AVD with avdmanager")?;

    // The target image must be present locally before an AVD can reference it.
    if !installed_system_images().iter().any(|i| i == image) {
        bail!(
            "system image `{image}` is not installed.\n  \
             Install it with: sdkmanager \"{image}\"\n  \
             Then re-run this command."
        );
    }

    let mut args = vec!["create", "avd", "-n", name, "-k", image];
    if let Some(device) = device {
        args.push("-d");
        args.push(device);
    }
    // `avdmanager` prompts for a hardware profile when `-d` is omitted; with
    // `-d` supplied it still asks for custom hardware, which stdin answers.
    super::run(&manager, &args)
}

/// Boots an AVD, preferring the blocking `android` CLI.
pub fn boot_avd(name: &str) -> Result<String> {
    if let Some(cli) = super::android_cli() {
        println!("  {} android emulator start {name}", "→".blue());
        super::run(&cli, &["emulator", "start", name])?;
    } else {
        let emulator = emulator_binary()
            .context("neither the `android` CLI nor `$ANDROID_HOME/emulator/emulator` was found")?;
        println!("  {} emulator -avd {name}", "→".blue());
        // Without the `android` CLI there is nothing to block on, so the
        // emulator is spawned and the wait happens below.
        std::process::Command::new(&emulator)
            .args(["-avd", name, "-gpu", "host"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("failed to start the Android emulator")?;
    }

    wait_for_emulator(name)
}

/// Waits for an AVD to appear in `adb devices` and finish booting.
pub fn wait_for_emulator(name: &str) -> Result<String> {
    let adb = adb().context("`adb` was not found; install the Android platform tools")?;

    // Give the emulator a moment to register before polling, so a fresh boot
    // does not race the first `wait-for-device`.
    std::thread::sleep(std::time::Duration::from_secs(2));

    for _ in 0..150 {
        for device in adb_devices() {
            if device.state != "device" || !device.serial.starts_with("emulator-") {
                continue;
            }
            let avd_name = emulator_avd_name(&adb, &device.serial);
            if avd_name.as_deref() != Some(name) {
                continue;
            }
            if getprop(&adb, &device.serial, "sys.boot_completed").as_deref() == Some("1") {
                return Ok(device.serial);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    bail!("timed out waiting for emulator `{name}` to finish booting")
}

pub fn shutdown_avd(name: &str) -> Result<()> {
    if let Some(cli) = super::android_cli() {
        return super::run(&cli, &["emulator", "stop", name]);
    }
    let adb = adb().context("`adb` was not found")?;
    // Resolve the AVD name to its serial, then ask the emulator to quit.
    for device in adb_devices() {
        if !device.serial.starts_with("emulator-") {
            continue;
        }
        let avd_name = emulator_avd_name(&adb, &device.serial);
        if avd_name.as_deref() == Some(name) {
            return super::run(&adb, &["-s", &device.serial, "emu", "kill"]);
        }
    }
    bail!("emulator `{name}` is not running")
}

pub fn remove_avd(name: &str) -> Result<()> {
    if let Some(cli) = super::android_cli() {
        return super::run(&cli, &["emulator", "remove", name]);
    }
    let manager = super::avdmanager().context("`avdmanager` was not found")?;
    super::run(&manager, &["delete", "avd", "-n", name])
}

/// Installs and launches an APK on a specific device.
pub fn install_and_launch(serial: &str, apk: &Path, bundle_id: &str) -> Result<()> {
    install_apk(serial, apk)?;
    launch_app(serial, bundle_id)
}

/// Installs an APK, replacing an existing build.
pub fn install_apk(serial: &str, apk: &Path) -> Result<()> {
    let adb = adb().context("`adb` was not found")?;
    let apk = apk.to_string_lossy().into_owned();
    super::run(&adb, &["-s", serial, "install", "-r", &apk])
}

/// Kills the app process so the next `am start` is a cold launch. Without
/// this, reinstalling under live mode would leave the old process running
/// with stale dev-channel credentials.
pub fn force_stop(serial: &str, bundle_id: &str) -> Result<()> {
    let adb = adb().context("`adb` was not found")?;
    super::run(
        &adb,
        &["-s", serial, "shell", "am", "force-stop", bundle_id],
    )
}

/// Starts the app's activity.
pub fn launch_app(serial: &str, bundle_id: &str) -> Result<()> {
    let adb = adb().context("`adb` was not found")?;
    super::run(
        &adb,
        &[
            "-s",
            serial,
            "shell",
            "am",
            "start",
            "-n",
            &format!("{bundle_id}/dev.gpui.mobile.GpuiActivity"),
        ],
    )
}

/// Returns the current PID for an installed package, if it is running.
pub fn app_pid(serial: &str, bundle_id: &str) -> Result<Option<u32>> {
    let adb = adb().context("`adb` was not found")?;
    let Some(output) = try_capture(&adb, &["-s", serial, "shell", "pidof", "-s", bundle_id]) else {
        return Ok(None);
    };
    Ok(output
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u32>().ok()))
}

/// Identity for one observed package process. A PID alone is not stable: the
/// kernel may reuse it after an early crash. The proc start tick is stable for
/// a process lifetime, while boot_id prevents the same tick value from being
/// reused after a device reboot.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AppProcessIdentity {
    pub pid: u32,
    pub start_time_ticks: u64,
    pub boot_id: Option<String>,
}

impl AppProcessIdentity {
    /// Returns an internal, non-serialized identity input for evidence hashing.
    pub fn start_token(&self) -> String {
        format!(
            "android-process|pid={}|start_time_ticks={}|boot_id={}",
            self.pid,
            self.start_time_ticks,
            self.boot_id.as_deref().unwrap_or("unknown")
        )
    }
}

/// Maximum time spent waiting for the process created by `am start` to appear.
pub const APP_PROCESS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Reads a package PID and its kernel process-start identity from one serial.
/// A process that exits between the PID and `/proc` reads is reported as
/// unavailable, so early native crashes remain observable without turning the
/// launch command itself into a false failure.
pub fn app_process_identity(serial: &str, bundle_id: &str) -> Result<Option<AppProcessIdentity>> {
    let Some(pid) = app_pid(serial, bundle_id)? else {
        return Ok(None);
    };
    let adb = adb().context("`adb` was not found")?;
    let stat_path = format!("/proc/{pid}/stat");
    let Some(stat) = try_capture(&adb, &["-s", serial, "shell", "cat", &stat_path]) else {
        return Ok(None);
    };
    let start_time_ticks = parse_proc_stat_start_time(&stat)
        .with_context(|| format!("parsing Android process stat for pid {pid}"))?;
    let boot_id = try_capture(
        &adb,
        &[
            "-s",
            serial,
            "shell",
            "cat",
            "/proc/sys/kernel/random/boot_id",
        ],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());
    Ok(Some(AppProcessIdentity {
        pid,
        start_time_ticks,
        boot_id,
    }))
}

/// Waits briefly for the process spawned by `am start`, then returns its
/// identity. The bounded wait covers normal launch scheduling without hiding
/// an early crash or a channel that never establishes.
pub fn wait_for_app_process(serial: &str, bundle_id: &str) -> Result<Option<AppProcessIdentity>> {
    let deadline = Instant::now() + APP_PROCESS_PROBE_TIMEOUT;
    loop {
        if let Some(identity) = app_process_identity(serial, bundle_id)? {
            return Ok(Some(identity));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct DisplayEvidence {
    pub logical_width: Option<u32>,
    pub logical_height: Option<u32>,
    pub scale_milli: Option<u32>,
    pub orientation: Option<String>,
    pub foreground_app: Option<String>,
}

/// Probes display metadata independently from screenshot bytes. Missing or
/// vendor-specific shell output remains an unknown field instead of being
/// converted into a guessed viewport.
pub fn display_evidence(serial: &str, bundle_id: &str) -> Result<DisplayEvidence> {
    let adb = adb().context("`adb` was not found")?;
    let size = try_capture(&adb, &["-s", serial, "shell", "wm", "size"])
        .as_deref()
        .and_then(parse_display_size);
    let density = try_capture(&adb, &["-s", serial, "shell", "wm", "density"])
        .as_deref()
        .and_then(parse_display_density);
    let orientation = try_capture(&adb, &["-s", serial, "shell", "dumpsys", "input"])
        .as_deref()
        .and_then(parse_surface_orientation);
    let foreground_app = try_capture(
        &adb,
        &["-s", serial, "shell", "dumpsys", "activity", "activities"],
    )
    .and_then(|output| parse_foreground_app(&output, bundle_id));

    let scale_milli = density.and_then(|value| u32::try_from(u64::from(value) * 1000 / 160).ok());
    let (logical_width, logical_height) = match (size, density) {
        (Some((width, height)), Some(density)) if density > 0 => (
            u32::try_from(u64::from(width) * 160 / u64::from(density)).ok(),
            u32::try_from(u64::from(height) * 160 / u64::from(density)).ok(),
        ),
        _ => (None, None),
    };
    Ok(DisplayEvidence {
        logical_width,
        logical_height,
        scale_milli,
        orientation,
        foreground_app,
    })
}

fn parse_display_size(output: &str) -> Option<(u32, u32)> {
    output.lines().rev().find_map(|line| {
        let value = line.split_once(':')?.1.trim();
        let (width, height) = value.split_once('x')?;
        let width = width.parse().ok()?;
        let height = height.parse().ok()?;
        (width > 0 && height > 0).then_some((width, height))
    })
}

fn parse_display_density(output: &str) -> Option<u32> {
    output.lines().rev().find_map(|line| {
        let value = line.split_once(':')?.1.trim();
        let density = value.parse().ok()?;
        (density > 0).then_some(density)
    })
}

fn parse_foreground_app(output: &str, bundle_id: &str) -> Option<String> {
    const FOREGROUND_MARKERS: [&str; 3] = ["mResumedActivity:", "mFocusedApp=", "mCurrentFocus="];
    output
        .lines()
        .map(str::trim_start)
        .find(|line| {
            FOREGROUND_MARKERS
                .iter()
                .any(|marker| line.starts_with(marker))
                && line.contains(bundle_id)
        })
        .map(|_| bundle_id.to_owned())
}

fn parse_surface_orientation(output: &str) -> Option<String> {
    let value = output.lines().find_map(|line| {
        let (_, value) = line.split_once("SurfaceOrientation:")?;
        value.trim().parse::<u8>().ok()
    })?;
    Some(
        match value {
            0 => "portrait",
            1 => "landscape",
            2 => "reverse_portrait",
            3 => "reverse_landscape",
            _ => return None,
        }
        .into(),
    )
}

fn parse_proc_stat_start_time(stat: &str) -> Result<u64> {
    let open = stat.find('(').context("process stat has no command name")?;
    let close = stat
        .rfind(')')
        .filter(|close| *close > open)
        .context("process stat has no command terminator")?;
    stat[..open]
        .trim()
        .parse::<u32>()
        .context("process stat has an invalid pid")?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let start_time = fields
        .get(19)
        .context("process stat has no start-time field")?;
    start_time
        .parse::<u64>()
        .context("process stat has an invalid start-time field")
}

/// Captures the device framebuffer as a binary PNG. `exec-out` avoids the
/// shell and never subjects the image bytes to text framing or line endings.
pub fn capture_screenshot(serial: &str, output: &Path) -> Result<()> {
    match std::fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "refusing to write screenshot through symbolic link: {}",
                output.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let adb = adb().context("`adb` was not found")?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(output)
        .with_context(|| format!("opening screenshot output {}", output.display()))?;
    let status = Command::new(&adb)
        .args(["-s", serial, "exec-out", "screencap", "-p"])
        .stdout(Stdio::from(file))
        .status()
        .with_context(|| format!("failed to run `adb -s {serial} exec-out screencap -p`"))?;
    if !status.success() {
        let _ = std::fs::remove_file(output);
        bail!("`adb -s {serial} exec-out screencap -p` failed");
    }
    Ok(())
}

pub const MAX_LOGCAT_BYTES: u64 = 8 * 1024 * 1024;

/// Collects a bounded logcat snapshot. When no verified PID is available the
/// caller may omit the filter, but must preserve that uncertainty in evidence.
pub fn collect_logcat(serial: &str, output: &Path, pid: Option<u32>) -> Result<(u64, bool)> {
    match std::fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "refusing to write logcat through symbolic link: {}",
                output.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = output
        .parent()
        .context("logcat output has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating logcat directory {}", parent.display()))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(output)
        .with_context(|| format!("opening logcat output {}", output.display()))?;
    let adb = adb().context("`adb` was not found")?;
    let mut args = vec!["-s", serial, "logcat", "-d", "-v", "threadtime"];
    let pid_text = pid.map(|value| value.to_string());
    if let Some(pid_text) = pid_text.as_deref() {
        args.extend(["--pid", pid_text]);
    }
    let mut child = Command::new(&adb)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting logcat collection for {serial}"))?;
    let mut stdout = child.stdout.take().context("capturing logcat output")?;
    let mut file = file;
    let mut buffer = [0_u8; 16 * 1024];
    let mut written = 0_u64;
    let mut truncated = false;
    loop {
        let read = stdout.read(&mut buffer).context("reading logcat output")?;
        if read == 0 {
            break;
        }
        let remaining = MAX_LOGCAT_BYTES.saturating_sub(written);
        let to_write = (read as u64).min(remaining) as usize;
        if to_write > 0 {
            file.write_all(&buffer[..to_write])
                .context("writing logcat output")?;
            written += to_write as u64;
        }
        if to_write < read {
            truncated = true;
        }
    }
    let status = child.wait().context("waiting for logcat collection")?;
    if !status.success() {
        let _ = std::fs::remove_file(output);
        bail!("logcat collection failed for {serial}");
    }
    Ok((written, truncated))
}

/// Maps a device-side port onto the same host port over adb, so the app can
/// reach the live dev server at `127.0.0.1:<port>` (works on emulators and
/// devices connected over USB alike).
pub fn reverse_port(serial: &str, port: u16) -> Result<()> {
    let adb = adb().context("`adb` was not found")?;
    let spec = format!("tcp:{port}");
    super::run(&adb, &["-s", serial, "reverse", &spec, &spec])
}

/// Writes a file into the app's internal files dir (subdirectories allowed).
///
/// Android apps have no host environment to inherit, so live mode delivers
/// its connection credentials (and hot-reloadable assets) as files staged
/// through `/data/local/tmp` and copied in with `run-as` (debug builds only,
/// which live mode requires).
pub fn write_device_config(serial: &str, package: &str, name: &str, content: &[u8]) -> Result<()> {
    let adb = adb().context("`adb` was not found")?;
    let staged = format!("/data/local/tmp/{name}");
    // Sub-path assets need the staging directory to exist first.
    if let Some((dir, _)) = name.rsplit_once('/') {
        super::run(
            &adb,
            &[
                "-s",
                serial,
                "shell",
                "mkdir",
                "-p",
                &format!("/data/local/tmp/{dir}"),
            ],
        )?;
    }

    let mut stage = Command::new(&adb)
        .args([
            "-s",
            serial,
            "shell",
            "sh",
            "-c",
            &format!("cat > {staged}"),
        ])
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to stage {staged}"))?;
    if let Some(mut stdin) = stage.stdin.take() {
        stdin
            .write_all(content)
            .with_context(|| format!("failed to write {staged}"))?;
    }
    let status = stage.wait().with_context(|| format!("staging {staged}"))?;
    if !status.success() {
        bail!("staging {staged} failed");
    }

    let target = format!("files/{name}");
    let parent = match target.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => "files".to_string(),
    };
    super::run(
        &adb,
        &[
            "-s", serial, "shell", "run-as", package, "mkdir", "-p", &parent,
        ],
    )?;
    super::run(
        &adb,
        &[
            "-s", serial, "shell", "run-as", package, "cp", &staged, &target,
        ],
    )
}

/// Removes a file from the app's private files dir. The name is passed as an
/// adb argument rather than through a shell so asset paths cannot escape that
/// directory or alter the command being run.
pub fn remove_device_file(serial: &str, package: &str, name: &str) -> Result<()> {
    validate_device_file_name(name)?;
    let adb = adb().context("`adb` was not found")?;
    let target = format!("files/{name}");
    super::run(
        &adb,
        &[
            "-s", serial, "shell", "run-as", package, "rm", "-f", &target,
        ],
    )
}

fn validate_device_file_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("invalid app-private file name '{name}'")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ini(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reads_the_api_level_from_the_image_path() {
        let entries = ini(&[
            ("AvdId", "Pixel_9_Pro"),
            (
                "image.sysdir.1",
                "system-images/android-36/google_apis_playstore_ps16k/arm64-v8a/",
            ),
            ("abi.type", "arm64-v8a"),
            ("PlayStore.enabled", "true"),
        ]);
        assert_eq!(ini_get(&entries, "AvdId"), Some("Pixel_9_Pro"));
        assert_eq!(ini_get(&entries, "abi.type"), Some("arm64-v8a"));
        assert_eq!(
            ini_get(&entries, "image.sysdir.1").and_then(|i| i.split('/').nth(1)),
            Some("android-36")
        );
        assert!(ini_get(&entries, "PlayStore.enabled") == Some("true"));
    }

    #[test]
    fn rebuilding_the_image_package_round_trips() {
        // The trailing slash in config.ini must not leak into the package path,
        // which sdkmanager would reject.
        let avd = Avd {
            name: "Pixel_9_Pro".into(),
            abi: Some("arm64-v8a".into()),
            api: Some("android-36".into()),
            image: Some("system-images/android-36/google_apis_playstore_ps16k/arm64-v8a/".into()),
            device: Some("pixel_9_pro".into()),
            play_store: true,
            dir: None,
        };
        assert_eq!(
            avd.image_package().as_deref(),
            Some("system-images;android-36;google_apis_playstore_ps16k;arm64-v8a")
        );
        assert_eq!(avd.api_level(), Some(36));
    }

    #[test]
    fn image_package_rejects_paths_outside_system_images() {
        let avd = Avd {
            name: "odd".into(),
            abi: None,
            api: None,
            image: Some("platforms/android-36".into()),
            device: None,
            play_store: false,
            dir: None,
        };
        assert_eq!(avd.image_package(), None);
    }

    #[test]
    fn runtime_does_not_double_the_android_prefix() {
        assert_eq!(api_to_runtime("android-36"), "android-36");
        assert_eq!(api_to_runtime("36"), "android-36");
    }

    #[test]
    fn device_file_names_cannot_escape_private_files() {
        assert!(validate_device_file_name("assets/icons/logo.png").is_ok());
        assert!(validate_device_file_name("../outside").is_err());
        assert!(validate_device_file_name("assets/../outside").is_err());
        assert!(validate_device_file_name("/absolute").is_err());
        assert!(validate_device_file_name("assets\\logo.png").is_err());
    }

    #[test]
    fn proc_stat_parser_handles_parentheses_in_the_command_name() {
        let mut fields = vec!["0".to_string(); 20];
        fields[0] = "S".into();
        fields[19] = "987654".into();
        let stat = format!("123 (gpui app (debug)) {}", fields.join(" "));
        assert_eq!(parse_proc_stat_start_time(&stat).unwrap(), 987654);
    }

    #[test]
    fn process_start_token_binds_pid_boot_and_start_time() {
        let process = AppProcessIdentity {
            pid: 123,
            start_time_ticks: 987654,
            boot_id: Some("boot-a".into()),
        };
        assert!(process.start_token().contains("pid=123"));
        assert!(process.start_token().contains("start_time_ticks=987654"));
        assert!(process.start_token().contains("boot_id=boot-a"));

        let reused_pid = AppProcessIdentity {
            start_time_ticks: 987655,
            ..process.clone()
        };
        assert_ne!(process.start_token(), reused_pid.start_token());
    }

    #[test]
    fn display_metadata_parsers_keep_unknown_vendor_output_unset() {
        assert_eq!(
            parse_display_size("Physical size: 1080x2400\n"),
            Some((1080, 2400))
        );
        assert_eq!(
            parse_display_size("Override size: 720x1280\n"),
            Some((720, 1280))
        );
        assert_eq!(parse_display_density("Physical density: 420\n"), Some(420));
        assert_eq!(
            parse_surface_orientation("SurfaceOrientation: 1\n"),
            Some("landscape".into())
        );
        assert_eq!(parse_surface_orientation("rotation=unknown\n"), None);
        assert_eq!(parse_display_size("Physical size: 0x0\n"), None);
        assert_eq!(parse_display_density("Override density: 0\n"), None);
        assert_eq!(
            parse_foreground_app(
                "mResumedActivity: ActivityRecord{u0 com.example.app/.MainActivity}\n",
                "com.example.app"
            ),
            Some("com.example.app".into())
        );
        assert_eq!(
            parse_foreground_app(
                "mResumedActivity: ActivityRecord{u0 com.other/.MainActivity}\nHistory: com.example.app/.OldActivity\n",
                "com.example.app"
            ),
            None
        );
    }

    #[test]
    fn ini_reader_skips_comments_and_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.ini");
        std::fs::write(
            &path,
            "# a comment\n\nAvdId = Pixel_9a\nabi.type=arm64-v8a\n",
        )
        .unwrap();

        let entries = read_ini(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(ini_get(&entries, "AvdId"), Some("Pixel_9a"));
        // Whitespace around the separator is not significant.
        assert_eq!(ini_get(&entries, "abi.type"), Some("arm64-v8a"));
    }
}
