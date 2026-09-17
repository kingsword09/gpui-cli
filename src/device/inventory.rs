//! Turning a set of flags, defaults and environment variables into one device.
//!
//! The resolution order is fixed and documented so that `gpui run ios` is
//! reproducible: CLI flags, then `gpui.toml` `[run]`, then environment
//! variables, then an interactive prompt, then a documented auto-pick.

use anyhow::{bail, Context, Result};
use colored::*;

use super::{android, ios, Device, DeviceFlags, Kind, Platform, State};

/// Per-project defaults from the `[run]` section of `gpui.toml`.
#[derive(Debug, Clone, Default)]
pub struct Defaults {
    pub ios_simulator: Option<String>,
    pub android_avd: Option<String>,
}

impl Defaults {
    pub fn from_manifest(manifest: &str) -> Self {
        Self {
            ios_simulator: read_run_value(manifest, "ios_simulator"),
            android_avd: read_run_value(manifest, "android_avd"),
        }
    }
}

/// Reads `key = "value"` from the `[run]` section only.
fn read_run_value(manifest: &str, key: &str) -> Option<String> {
    let section = manifest.split("[run]").nth(1)?;
    let section = section.split("\n[").next()?;
    for line in section.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.trim_start().strip_prefix('=') {
                let value = toml_value(rest);
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Extracts a string value, dropping a trailing inline comment. The generated
/// template ships inline comments on these keys, so `iPhone 16e@18.4   # ...`
/// has to parse as just the device spec.
fn toml_value(raw: &str) -> String {
    let raw = raw.trim();
    let raw = match raw.strip_prefix('"') {
        // Quoted: take up to the closing quote, so a `#` inside the value is
        // preserved and anything after it is a comment.
        Some(rest) => rest.split('"').next().unwrap_or(""),
        // Unquoted: a `#` starts a comment.
        None => raw.split('#').next().unwrap_or(""),
    };
    raw.trim().to_string()
}

// ── Spec matching ───────────────────────────────────────────────────────────

/// A CoreSimulator UDID: hex digits and dashes.
fn looks_like_udid(spec: &str) -> bool {
    spec.len() >= 8 && spec.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Splits `iPhone 17 Pro@26.2` into a name and an optional runtime.
fn split_runtime(spec: &str) -> (String, Option<String>) {
    match spec.split_once('@') {
        Some((name, runtime)) => (name.trim().to_string(), Some(runtime.trim().to_string())),
        None => (spec.trim().to_string(), None),
    }
}

fn runtime_matches(device: &Device, wanted: &str) -> bool {
    let Some(runtime) = device.runtime.as_deref() else {
        return false;
    };
    let wanted = wanted.trim().trim_start_matches("iOS").trim();
    runtime == wanted
        || runtime.ends_with(&format!(" {wanted}"))
        || runtime.trim_start_matches("iOS ") == wanted
}

fn ios_matches(device: &Device, name: &str, runtime: Option<&str>) -> bool {
    let by_id = device.id.eq_ignore_ascii_case(name);
    let by_name = device.name == name || device.name.eq_ignore_ascii_case(name);
    if !(by_id || by_name) {
        return false;
    }
    match runtime {
        Some(runtime) => runtime_matches(device, runtime),
        None => true,
    }
}

fn android_matches(device: &Device, spec: &str) -> bool {
    device.id == spec
        || device.serial() == Some(spec)
        || device.name == spec
        || device.name.eq_ignore_ascii_case(spec)
}

// ── Resolution ──────────────────────────────────────────────────────────────

/// Resolves the device to use for `platform`.
pub fn resolve_device(
    platform: Platform,
    flags: &DeviceFlags,
    defaults: &Defaults,
    project_default: Option<&str>,
) -> Result<Device> {
    match platform {
        Platform::Ios => resolve_ios(flags, defaults, project_default),
        Platform::Android => resolve_android(flags, defaults),
    }
}

fn resolve_ios(
    flags: &DeviceFlags,
    defaults: &Defaults,
    project_default: Option<&str>,
) -> Result<Device> {
    let physical_id = std::env::var("GPUI_IOS_DEVICE_ID")
        .ok()
        .filter(|s| !s.trim().is_empty());

    // Explicit physical-device requests.
    if flags.device_only || physical_id.is_some() {
        let wanted = flags.device.clone().or(physical_id);
        return pick_physical(wanted);
    }

    // A `--device` that names a physical device wins over simulator specs, and
    // is reported with its own reason when it cannot be used rather than
    // falling through to a confusing "no simulator" error.
    if let Some(spec) = flags.device.as_deref() {
        let (name, runtime) = split_runtime(spec);
        if !looks_like_udid(&name) {
            let physical = ios::physical_devices();
            if let Some(device) = physical
                .iter()
                .find(|d| ios_matches(d, &name, runtime.as_deref()))
            {
                if !device.launchable() {
                    bail!(
                        "iOS device `{}` is not usable: {}\n  \
                         Unlock it, re-pair it, or check its connection in Xcode.\n  \
                         Run `gpui device list --all` to see all known devices.",
                        device.label(),
                        device.state_label()
                    );
                }
                return Ok(device.clone());
            }
        }
        return pick_simulator(Some(spec));
    }

    if let Some(spec) = flags.sim.as_deref() {
        return pick_simulator(Some(spec));
    }

    // Project default, then the long-standing environment variable.
    if let Some(spec) = defaults.ios_simulator.as_deref() {
        return pick_simulator(Some(spec));
    }
    if let Ok(spec) = std::env::var("GPUI_IOS_DEVICE") {
        if !spec.trim().is_empty() {
            return pick_simulator(Some(spec.trim()));
        }
    }
    if let Some(spec) = project_default {
        return pick_simulator(Some(spec));
    }

    if let Some(device) = prompt(Platform::Ios)? {
        return Ok(device);
    }
    ios::default_simulator()
}

fn resolve_android(flags: &DeviceFlags, defaults: &Defaults) -> Result<Device> {
    if let Some(spec) = flags.avd.as_deref() {
        return pick_android_emulator(spec);
    }
    if let Some(spec) = flags.device.as_deref() {
        // A serial selects a connected device; anything else is an AVD name.
        let devices = android::all();
        if let Some(device) = devices
            .into_iter()
            .find(|d| d.serial() == Some(spec) && d.launchable())
        {
            return Ok(device);
        }
        return pick_android_emulator(spec);
    }
    if let Some(spec) = defaults.android_avd.as_deref() {
        return pick_android_emulator(spec);
    }

    if let Some(device) = prompt(Platform::Android)? {
        return Ok(device);
    }

    // Nothing chosen: prefer something already running, else the first AVD.
    let devices: Vec<Device> = android::all()
        .into_iter()
        .filter(Device::launchable)
        .collect();
    if let Some(running) = devices.iter().find(|d| d.is_running()) {
        return Ok(running.clone());
    }
    if let Some(avd) = devices.iter().find(|d| d.kind == Kind::Emulator) {
        return Ok(avd.clone());
    }
    devices.into_iter().next().context(
        "No Android device found. Connect a device, or create an emulator with \
         `gpui device create --platform android`.",
    )
}

fn pick_simulator(spec: Option<&str>) -> Result<Device> {
    let devices = ios::simulators()?;

    let Some(spec) = spec else {
        return ios::default_simulator();
    };
    let (name, runtime) = split_runtime(spec);

    // A UDID is unambiguous, so it is matched first.
    if looks_like_udid(&name) {
        if let Some(device) = devices.iter().find(|d| d.id.eq_ignore_ascii_case(&name)) {
            if !device.launchable() {
                bail!(
                    "Simulator `{}` is not usable ({})",
                    device.label(),
                    device.state_label()
                );
            }
            return Ok(device.clone());
        }
        bail!("No iOS simulator with UDID `{name}`. List them with `gpui device list`.");
    }

    let candidates: Vec<&Device> = devices
        .iter()
        .filter(|d| ios_matches(d, &name, runtime.as_deref()))
        .collect();

    if candidates.is_empty() {
        let available = devices
            .iter()
            .filter(|d| d.launchable())
            .map(Device::label)
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "No iOS simulator matching `{spec}`.\n  Available: {available}\n  \
             Run `gpui device list` to see installed simulators."
        );
    }
    if let Some(device) = candidates.iter().find(|d| d.launchable()) {
        return Ok((*device).clone());
    }
    bail!("Simulator `{spec}` exists but is not usable.")
}

fn pick_android_emulator(spec: &str) -> Result<Device> {
    let devices = android::all();

    if let Some(device) = devices
        .iter()
        .find(|d| android_matches(d, spec) && d.launchable())
    {
        return Ok(device.clone());
    }
    if let Some(device) = devices.iter().find(|d| android_matches(d, spec)) {
        bail!(
            "`{}` is not usable: {}",
            device.label(),
            device.state_label()
        );
    }

    let available = devices
        .iter()
        .filter(|d| d.launchable())
        .map(Device::label)
        .collect::<Vec<_>>();
    if available.is_empty() {
        bail!(
            "No Android device named `{spec}`, and none are available.\n  \
             Create one with `gpui device create --platform android --name {spec}`."
        );
    }
    bail!(
        "No Android device named `{spec}`.\n  Available: {}\n  \
         Run `gpui device list` to see installed AVDs.",
        available.join(", ")
    );
}

fn pick_physical(wanted: Option<String>) -> Result<Device> {
    let devices: Vec<Device> = ios::physical_devices()
        .into_iter()
        .filter(|d| d.kind == Kind::Physical)
        .collect();

    if devices.is_empty() {
        bail!(
            "No iOS physical device is known to CoreDevice.\n  \
             Connect and trust a device, then check it with `gpui device list --all`."
        );
    }

    if let Some(wanted) = wanted {
        let wanted = wanted.trim();
        if let Some(device) = devices
            .iter()
            .find(|d| d.id.eq_ignore_ascii_case(wanted) || d.name.eq_ignore_ascii_case(wanted))
        {
            if !device.launchable() {
                bail!(
                    "Device `{}` is not usable: {}\n  \
                     Unlock it, re-pair it, or check its connection in Xcode.",
                    device.label(),
                    device.state_label()
                );
            }
            return Ok(device.clone());
        }
        bail!("No iOS physical device matching `{wanted}`. Run `gpui device list --all`.");
    }

    if let Some(device) = devices.iter().find(|d| d.launchable()) {
        return Ok(device.clone());
    }

    let reasons = devices
        .iter()
        .map(|d| format!("{} ({})", d.label(), d.state_label()))
        .collect::<Vec<_>>()
        .join("; ");
    bail!("No usable iOS physical device. Known devices: {reasons}")
}

// ── Booting ─────────────────────────────────────────────────────────────────

/// Boots the device if needed and returns it with a resolved serial.
pub fn ensure_running(device: Device) -> Result<Device> {
    match (device.platform, &device.state) {
        (_, State::Running { .. }) => Ok(device),

        (Platform::Ios, State::Stopped) => {
            println!("  {} booting simulator {}", "→".blue(), device.label());
            ios::boot(&device.id)?;
            Ok(Device {
                state: State::Running { serial: None },
                ..device
            })
        }

        (Platform::Android, State::Stopped) if device.kind == Kind::Emulator => {
            let serial = android::boot_avd(&device.id)?;
            Ok(Device {
                state: State::Running {
                    serial: Some(serial),
                },
                ..device
            })
        }

        (_, State::Unavailable { reason }) => {
            bail!("`{}` cannot be used: {reason}", device.label())
        }

        // A stopped physical device is not something this tool can start.
        (Platform::Android, State::Stopped) => {
            bail!("Android device `{}` is not connected.", device.label())
        }
    }
}

// ── Interactive picker ──────────────────────────────────────────────────────

/// Prompts for a device, or returns `None` when prompting is not appropriate.
fn prompt(platform: Platform) -> Result<Option<Device>> {
    if !super::is_interactive() {
        return Ok(None);
    }

    let devices: Vec<Device> = match platform {
        Platform::Ios => ios::all(),
        Platform::Android => android::all(),
    };
    let devices: Vec<Device> = devices.into_iter().filter(Device::launchable).collect();
    if devices.is_empty() {
        return Ok(None);
    }

    let options: Vec<String> = devices.iter().map(picker_label).collect();
    let answer = match inquire::Select::new(
        &format!("Select a {} device:", platform.display_name()),
        options,
    )
    .prompt()
    {
        Ok(answer) => answer,
        // A cancelled prompt falls through to the auto-pick rather than failing.
        Err(_) => return Ok(None),
    };

    let index = devices
        .iter()
        .position(|d| picker_label(d) == answer)
        .context("could not map the selected entry back to a device")?;

    Ok(Some(devices[index].clone()))
}

/// One line per candidate in the interactive picker.
fn picker_label(device: &Device) -> String {
    let mut line = format!(
        "{}  [{}]  {}",
        device.label(),
        device.kind.label(),
        device.state_label()
    );
    if !device.detail.is_empty() {
        line.push_str(&format!("  {}", device.detail));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::State;

    fn device(name: &str, runtime: &str) -> Device {
        Device {
            platform: Platform::Ios,
            kind: Kind::Emulator,
            id: format!("UDID-{name}"),
            name: name.to_string(),
            runtime: Some(runtime.to_string()),
            arch: None,
            state: State::Stopped,
            detail: String::new(),
            last_used: None,
        }
    }

    #[test]
    fn a_name_matches_only_within_its_runtime() {
        // The whole point of `name@runtime`: iPhone 16e exists on both runtimes.
        let old = device("iPhone 16e", "iOS 18.4");
        let new = device("iPhone 16e", "iOS 26.2");

        assert!(ios_matches(&old, "iPhone 16e", Some("18.4")));
        assert!(!ios_matches(&new, "iPhone 16e", Some("18.4")));
        assert!(ios_matches(&new, "iPhone 16e", Some("26.2")));
        // Without a runtime, both match (the caller reports ambiguity by taking
        // the first launchable one).
        assert!(ios_matches(&old, "iPhone 16e", None));
        assert!(ios_matches(&new, "iPhone 16e", None));
    }

    #[test]
    fn runtime_specs_accept_the_ios_prefix() {
        let d = device("iPhone 17 Pro", "iOS 26.2");
        assert!(runtime_matches(&d, "26.2"));
        assert!(runtime_matches(&d, "iOS 26.2"));
        assert!(runtime_matches(&d, "iOS26.2"));
        assert!(!runtime_matches(&d, "18.4"));
    }

    #[test]
    fn udid_detection_does_not_confuse_a_device_name() {
        assert!(looks_like_udid("864C6391-D64A-4073-B18D-FC7F2B155F94"));
        assert!(looks_like_udid("00008101-000250100252001E"));
        assert!(!looks_like_udid("iPhone 16 Pro"));
        assert!(!looks_like_udid("Pixel_9_Pro"));
        // Too short to be a UDID.
        assert!(!looks_like_udid("abc"));
    }

    #[test]
    fn splits_a_name_at_the_runtime_separator() {
        assert_eq!(
            split_runtime("iPhone 17 Pro@26.2"),
            ("iPhone 17 Pro".into(), Some("26.2".into()))
        );
        assert_eq!(
            split_runtime("iPhone 17 Pro"),
            ("iPhone 17 Pro".into(), None)
        );
        // An empty runtime is still `Some`, and simply will not match.
        assert_eq!(split_runtime("X@"), ("X".into(), Some(String::new())));
    }

    #[test]
    fn reads_run_defaults_from_the_manifest() {
        let manifest = "\
[app]
name = \"demo\"

[run]
ios_simulator = \"iPhone 17 Pro@26.2\"
android_avd = \"Pixel_9_Pro\"
";
        let defaults = Defaults::from_manifest(manifest);
        assert_eq!(
            defaults.ios_simulator.as_deref(),
            Some("iPhone 17 Pro@26.2")
        );
        assert_eq!(defaults.android_avd.as_deref(), Some("Pixel_9_Pro"));
    }

    #[test]
    fn commented_out_run_defaults_are_ignored() {
        // The generated template ships both keys commented out, so a fresh
        // project must not pick up a device that does not exist.
        let manifest = "\
[app]
name = \"demo\"

[run]
# ios_simulator = \"iPhone 17 Pro@26.2\"
# android_avd = \"Pixel_9_Pro\"
";
        let defaults = Defaults::from_manifest(manifest);
        assert_eq!(defaults.ios_simulator, None);
        assert_eq!(defaults.android_avd, None);
    }

    #[test]
    fn run_defaults_are_not_read_from_other_sections() {
        // `ios_simulator` under [app] must not leak into the run defaults.
        let manifest = "\
[app]
name = \"demo\"
ios_simulator = \"Wrong\"
";
        assert_eq!(Defaults::from_manifest(manifest).ios_simulator, None);
    }

    #[test]
    fn empty_run_values_are_treated_as_absent() {
        let manifest = "[run]\nios_simulator = \"\"\n";
        assert_eq!(Defaults::from_manifest(manifest).ios_simulator, None);
    }

    #[test]
    fn inline_comments_do_not_become_part_of_the_value() {
        // The generated template puts its hint on the same line as the key, so
        // uncommenting it must not fold the comment into the device spec.
        let manifest = "\
[run]
ios_simulator = \"iPhone 16e@18.4\"   # name@runtime; runtime optional
android_avd = \"Pixel_9a\"  # trailing
";
        let defaults = Defaults::from_manifest(manifest);
        assert_eq!(defaults.ios_simulator.as_deref(), Some("iPhone 16e@18.4"));
        assert_eq!(defaults.android_avd.as_deref(), Some("Pixel_9a"));
    }

    #[test]
    fn unquoted_values_still_parse() {
        let manifest = "[run]\nandroid_avd = Pixel_9a # note\n";
        assert_eq!(
            Defaults::from_manifest(manifest).android_avd.as_deref(),
            Some("Pixel_9a")
        );
    }
}
