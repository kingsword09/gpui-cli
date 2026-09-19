//! iOS device discovery via `xcrun simctl` and `xcrun devicectl`.
//!
//! Both tools emit JSON, so nothing here parses human-readable tables. That
//! matters: simulator names repeat across runtimes (`iPhone 16e` exists on both
//! iOS 18.4 and iOS 26.2), so a name match is genuinely ambiguous and only the
//! UDID is a sound key.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

use super::{capture, try_capture, Device, Kind, Platform, State};

fn xcrun() -> Result<PathBuf> {
    super::xcrun().context("`xcrun` was not found; install Xcode or the command line tools")
}

fn json(args: &[&str]) -> Result<Value> {
    let xcrun = xcrun()?;
    let text = capture(&xcrun, args)?;
    serde_json::from_str(&text)
        .with_context(|| format!("could not parse JSON from `xcrun {}`", args.join(" ")))
}

/// Runs `devicectl`, which — unlike simctl — only writes JSON to a file, never
/// to stdout. Hence the temp file.
fn devicectl_json(args: &[&str]) -> Option<Value> {
    let xcrun = super::xcrun()?;
    let file = tempfile::NamedTempFile::new().ok()?;
    let path = file.path().to_string_lossy().into_owned();

    let mut full: Vec<&str> = vec!["devicectl"];
    full.extend_from_slice(args);
    full.push("--json-output");
    full.push(&path);

    // Grouped devices need a moment to enumerate; a failure here just means
    // physical devices are omitted from the listing.
    try_capture(&xcrun, &full)?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

// ── Runtimes ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Runtime {
    pub identifier: String,
    pub name: String,
    pub version: String,
    pub supported_device_types: Vec<String>,
}

impl Runtime {
    /// `18.4` -> `(18, 4)` for ordering.
    pub fn sort_key(&self) -> (u64, u64) {
        parse_version(&self.version)
    }
}

fn parse_version(version: &str) -> (u64, u64) {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or(0);
    (major, minor)
}

pub fn runtimes() -> Result<Vec<Runtime>> {
    let value = json(&["simctl", "list", "runtimes", "--json"])?;
    let entries = value
        .get("runtimes")
        .and_then(Value::as_array)
        .context("`simctl list runtimes` returned no `runtimes` array")?;

    let mut out = Vec::new();
    for entry in entries {
        // Only iOS runtimes can host a GPUI app.
        if entry.get("platform").and_then(Value::as_str) != Some("iOS") {
            continue;
        }
        if entry.get("isAvailable").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let supported_device_types = entry
            .get("supportedDeviceTypes")
            .and_then(Value::as_array)
            .map(|types| {
                types
                    .iter()
                    .filter_map(|t| t.get("identifier").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        out.push(Runtime {
            identifier: string_field(entry, "identifier").unwrap_or_default(),
            name: string_field(entry, "name").unwrap_or_default(),
            version: string_field(entry, "version").unwrap_or_default(),
            supported_device_types,
        });
    }
    out.sort_by_key(|r| r.sort_key());
    Ok(out)
}

// ── Simulators ───────────────────────────────────────────────────────────────

pub fn simulators() -> Result<Vec<Device>> {
    // `devices` is keyed by runtime identifier.
    let value = json(&["simctl", "list", "devices", "--json"])?;
    let groups = value
        .get("devices")
        .and_then(Value::as_object)
        .context("`simctl list devices` returned no `devices` object")?;

    let runtime_names: Vec<(String, Runtime)> = runtimes()
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.identifier.clone(), r))
        .collect();

    let mut out = Vec::new();
    for (runtime_id, devices) in groups {
        let runtime = runtime_names
            .iter()
            .find(|(id, _)| id == runtime_id)
            .map(|(_, r)| r);
        let Some(runtime) = runtime else { continue };

        let Some(devices) = devices.as_array() else {
            continue;
        };
        for device in devices {
            let Some(udid) = string_field(device, "udid") else {
                continue;
            };
            let name = string_field(device, "name").unwrap_or_else(|| udid.clone());
            let available = device
                .get("isAvailable")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let booted = string_field(device, "state").as_deref() == Some("Booted");

            let state = if !available {
                State::Unavailable {
                    reason: "runtime reports it unavailable".to_string(),
                }
            } else if booted {
                State::Running { serial: None }
            } else {
                State::Stopped
            };

            out.push(Device {
                platform: Platform::Ios,
                kind: Kind::Emulator,
                id: udid,
                name,
                runtime: Some(runtime.name.clone()),
                arch: None,
                state,
                // `name` and `runtime` already identify a simulator; the device
                // type identifier would only restate the name.
                detail: String::new(),
                last_used: string_field(device, "lastBootedAt")
                    .and_then(|t| super::parse_rfc3339_utc(&t)),
            });
        }
    }

    // Running first, then newest runtime, then iPhone before iPad with the
    // higher model number first.
    out.sort_by(|a, b| {
        let key = |d: &Device| {
            let version = d
                .runtime
                .as_deref()
                .and_then(|name| name.rsplit(' ').next())
                .map(parse_version)
                .unwrap_or((0, 0));
            let (family, model) = model_rank(&d.name);
            (
                !d.is_running(),
                std::cmp::Reverse(version),
                family,
                std::cmp::Reverse(model),
                d.name.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(out)
}

/// Ranks a simulator name for ordering and default selection: iPhone before
/// iPad, and within iPhones the higher model number first.
fn model_rank(name: &str) -> (u8, u64) {
    if let Some(rest) = name.strip_prefix("iPhone") {
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (0, digits.parse().unwrap_or(0))
    } else if name.starts_with("iPad") {
        (1, 0)
    } else {
        (2, 0)
    }
}

/// The simulator to use when the user expressed no preference: a running one if
/// any, else an iPhone on the newest runtime.
pub fn default_simulator() -> Result<Device> {
    let devices = simulators()?;
    let launchable: Vec<Device> = devices.into_iter().filter(Device::launchable).collect();

    if let Some(running) = launchable.iter().find(|d| d.is_running()) {
        return Ok(running.clone());
    }
    if let Some(iphone) = launchable.iter().find(|d| d.name.starts_with("iPhone")) {
        return Ok(iphone.clone());
    }
    launchable.into_iter().next().context(
        "No available iOS simulator found. Create one with `gpui device create --platform ios`.",
    )
}

// ── Physical devices ────────────────────────────────────────────────────────

pub fn physical_devices() -> Vec<Device> {
    let Some(value) = devicectl_json(&["list", "devices"]) else {
        return Vec::new();
    };
    let Some(devices) = value
        .get("result")
        .and_then(|r| r.get("devices"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for device in devices {
        let hardware = device.get("hardwareProperties");
        // `reality` distinguishes real hardware from the paired Mac itself.
        if hardware
            .and_then(|h| h.get("reality"))
            .and_then(Value::as_str)
            != Some("physical")
        {
            continue;
        }
        let platform_name = hardware
            .and_then(|h| h.get("platform"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !platform_name.starts_with("iOS") {
            continue;
        }

        // The identifier CoreDevice uses for install/launch is not the UDID;
        // `hardwareProperties.udid` is.
        let udid = hardware
            .and_then(|h| h.get("udid"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| string_field(device, "identifier"));
        let Some(udid) = udid else { continue };

        let properties = device.get("deviceProperties");
        let name = properties
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("iOS device")
            .to_string();
        let os_version = properties
            .and_then(|p| p.get("osVersionNumber"))
            .and_then(Value::as_str)
            .map(str::to_string);

        let connection = device.get("connectionProperties");
        let pairing = connection
            .and_then(|c| c.get("pairingState"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let tunnel = connection
            .and_then(|c| c.get("tunnelState"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let ddi = properties
            .and_then(|p| p.get("ddiServicesAvailable"))
            .and_then(Value::as_bool);

        // A paired device with a down tunnel or no developer disk image shows up
        // in listings but cannot accept an install.
        let unreachable = if pairing != "paired" {
            Some(if pairing.is_empty() {
                "not paired".to_string()
            } else {
                format!("pairing state: {pairing}")
            })
        } else if !tunnel.is_empty() && tunnel != "available" {
            Some(format!("tunnel {tunnel}"))
        } else if ddi == Some(false) {
            Some("developer disk image unavailable".to_string())
        } else {
            None
        };

        let state = match unreachable {
            Some(reason) => State::Unavailable { reason },
            None => State::Running {
                serial: Some(udid.clone()),
            },
        };

        let detail = hardware
            .and_then(|h| h.get("marketingName"))
            .and_then(Value::as_str)
            .or_else(|| {
                hardware
                    .and_then(|h| h.get("productType"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .to_string();

        out.push(Device {
            platform: Platform::Ios,
            kind: Kind::Physical,
            id: udid,
            name,
            runtime: os_version.map(|v| format!("iOS {v}")),
            arch: None,
            state,
            detail,
            last_used: connection
                .and_then(|c| c.get("lastConnectionDate"))
                .and_then(Value::as_str)
                .and_then(super::parse_rfc3339_utc),
        });
    }
    out
}

pub fn all() -> Vec<Device> {
    let mut devices = simulators().unwrap_or_default();
    devices.extend(physical_devices());
    devices
}

// ── Device type templates (for `create`) ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DeviceType {
    pub name: String,
    pub identifier: String,
    pub product_family: String,
}

pub fn device_types(runtime: Option<&Runtime>) -> Result<Vec<DeviceType>> {
    let value = json(&["simctl", "list", "devicetypes", "--json"])?;
    let entries = value
        .get("devicetypes")
        .and_then(Value::as_array)
        .context("`simctl list devicetypes` returned no `devicetypes` array")?;

    let mut out = Vec::new();
    for entry in entries {
        let family = string_field(entry, "productFamily").unwrap_or_default();
        if family != "iPhone" && family != "iPad" {
            continue;
        }
        let identifier = string_field(entry, "identifier").unwrap_or_default();
        if let Some(runtime) = runtime {
            if !runtime.supported_device_types.contains(&identifier) {
                continue;
            }
        }
        out.push(DeviceType {
            name: string_field(entry, "name").unwrap_or_default(),
            identifier,
            product_family: family,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn find_runtime(spec: &str) -> Option<Runtime> {
    let wanted = spec.trim().trim_start_matches("iOS").trim();
    runtimes()
        .ok()?
        .into_iter()
        .find(|r| r.version == wanted || r.name == spec.trim() || r.identifier == spec.trim())
}

// ─ Lifecycle ────────────────────────────────────────────────────────────────

/// Creates a simulator and returns its UDID.
pub fn create(name: &str, device_type: &str, runtime: Option<&Runtime>) -> Result<String> {
    let xcrun = xcrun()?;
    let mut args = vec!["simctl", "create", name, device_type];
    if let Some(runtime) = runtime {
        args.push(&runtime.identifier);
    }
    let udid = capture(&xcrun, &args)?.trim().to_string();
    if udid.is_empty() {
        bail!("`simctl create` did not report a new device UDID");
    }
    Ok(udid)
}

pub fn boot(udid: &str) -> Result<()> {
    let xcrun = xcrun()?;
    // Booting an already-booted simulator reports an error; that is not a
    // failure for our purposes.
    let _ = try_capture(&xcrun, &["simctl", "boot", udid]);
    let _ = std::process::Command::new("open")
        .args(["-a", "Simulator"])
        .status();
    super::run(&xcrun, &["simctl", "bootstatus", udid, "-b"])
}

pub fn shutdown(udid: &str) -> Result<()> {
    let xcrun = xcrun()?;
    super::run(&xcrun, &["simctl", "shutdown", udid])
}

pub fn delete(udid: &str) -> Result<()> {
    let xcrun = xcrun()?;
    super::run(&xcrun, &["simctl", "delete", udid])
}

/// Installs and launches an app bundle on a simulator.
pub fn install_and_launch(udid: &str, app: &std::path::Path, bundle_id: &str) -> Result<()> {
    install_and_launch_with_env(udid, app, bundle_id, &[])
}

/// Same as `install_and_launch`, but passes `env` into the launched process.
///
/// `simctl launch` forwards the calling process's environment to the app when
/// the variable is prefixed with `SIMCTL_CHILD_`; this is how live mode hands
/// the app its dev-channel credentials on the simulator.
pub fn install_and_launch_with_env(
    udid: &str,
    app: &std::path::Path,
    bundle_id: &str,
    env: &[(String, String)],
) -> Result<()> {
    let xcrun = xcrun()?;
    let app = app.to_string_lossy().into_owned();
    super::run(&xcrun, &["simctl", "install", udid, &app])?;
    let mut launch = Command::new(&xcrun);
    launch.args([
        "simctl",
        "launch",
        "--terminate-running-process",
        udid,
        bundle_id,
    ]);
    for (key, value) in env {
        launch.env(format!("SIMCTL_CHILD_{key}"), value);
    }
    let status = launch
        .status()
        .with_context(|| "failed to spawn `simctl launch`")?;
    if !status.success() {
        bail!("`simctl launch` failed");
    }
    Ok(())
}

/// Installs and launches an app bundle on a physical device.
pub fn install_and_launch_device(udid: &str, app: &std::path::Path, bundle_id: &str) -> Result<()> {
    let xcrun = xcrun()?;
    let app = app.to_string_lossy().into_owned();
    super::run(
        &xcrun,
        &[
            "devicectl",
            "device",
            "install",
            "app",
            &app,
            "--device",
            udid,
        ],
    )?;
    super::run(
        &xcrun,
        &[
            "devicectl",
            "device",
            "process",
            "launch",
            "--device",
            udid,
            bundle_id,
        ],
    )
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
