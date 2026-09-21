//! Turning a set of flags, defaults and environment variables into one device.
//!
//! The resolution order is fixed and documented so that `gpui run ios` is
//! reproducible: CLI flags, then `gpui.toml` `[run]`, then environment
//! variables, then an interactive prompt, then a documented auto-pick.

use anyhow::{Context, Result, bail};
use colored::*;

use super::{Device, DeviceFlags, Kind, Platform, State, android, ios};

/// Per-project defaults from the `[run]` section of `gpui.toml`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Defaults {
    #[serde(default, deserialize_with = "optional_device")]
    pub ios_simulator: Option<String>,
    #[serde(default, deserialize_with = "optional_device")]
    pub android_avd: Option<String>,
}

fn optional_device<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    use serde::Deserialize;
    Ok(Option::<String>::deserialize(deserializer)?.and_then(|value| nonempty(Some(&value))))
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

impl Defaults {
    pub fn from_manifest(manifest: &str) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct RunManifest {
            #[serde(default)]
            run: Defaults,
        }
        Ok(toml::from_str::<RunManifest>(manifest)
            .context("invalid gpui.toml")?
            .run)
    }
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

#[derive(Default)]
struct Environment {
    ios_device_id: Option<String>,
    ios_simulator: Option<String>,
    android_serial: Option<String>,
}

impl Environment {
    fn read() -> Self {
        let get = |key| nonempty(std::env::var(key).ok().as_deref());
        Self {
            ios_device_id: get("GPUI_IOS_DEVICE_ID"),
            ios_simulator: get("GPUI_IOS_DEVICE"),
            android_serial: get("ANDROID_SERIAL"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Selection {
    Device(String),
    Physical(Option<String>),
    Simulator(String),
    Avd(String),
    Serial(String),
    Automatic,
}

fn ios_selection(
    flags: &DeviceFlags,
    defaults: &Defaults,
    project_default: Option<&str>,
    env: &Environment,
) -> Result<Selection> {
    if flags.avd.is_some() {
        bail!("--avd is only supported for Android");
    }
    if flags.sim.is_some() && (flags.device.is_some() || flags.device_only) {
        bail!("--sim cannot be combined with --device or --device-only");
    }
    if flags.device_only {
        return Ok(Selection::Physical(
            flags.device.clone().or_else(|| env.ios_device_id.clone()),
        ));
    }
    if let Some(spec) = &flags.device {
        return Ok(Selection::Device(spec.clone()));
    }
    if let Some(spec) = &flags.sim {
        return Ok(Selection::Simulator(spec.clone()));
    }
    if let Some(spec) = &defaults.ios_simulator {
        return Ok(Selection::Simulator(spec.clone()));
    }
    if let Some(spec) = &env.ios_device_id {
        return Ok(Selection::Physical(Some(spec.clone())));
    }
    if let Some(spec) = env.ios_simulator.as_deref().or(project_default) {
        return Ok(Selection::Simulator(spec.into()));
    }
    Ok(Selection::Automatic)
}

fn android_selection(
    flags: &DeviceFlags,
    defaults: &Defaults,
    env: &Environment,
) -> Result<Selection> {
    if flags.sim.is_some() || flags.device_only {
        bail!("--sim and --device-only are only supported for iOS");
    }
    if flags.avd.is_some() && flags.device.is_some() {
        bail!("--avd cannot be combined with --device");
    }
    if let Some(spec) = &flags.avd {
        return Ok(Selection::Avd(spec.clone()));
    }
    if let Some(spec) = &flags.device {
        return Ok(Selection::Device(spec.clone()));
    }
    if let Some(spec) = &defaults.android_avd {
        return Ok(Selection::Avd(spec.clone()));
    }
    if let Some(spec) = &env.android_serial {
        return Ok(Selection::Serial(spec.clone()));
    }
    Ok(Selection::Automatic)
}

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
    match ios_selection(flags, defaults, project_default, &Environment::read())? {
        Selection::Physical(wanted) => return pick_physical(wanted),
        Selection::Simulator(spec) => return pick_simulator(Some(&spec)),
        Selection::Device(spec) => {
            let (name, runtime) = split_runtime(&spec);
            // CoreDevice identifiers and simulator UDIDs have similar shapes.
            // Check physical devices by ID as well as by name before fallback.
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
            return pick_simulator(Some(&spec));
        }
        _ => {}
    }

    if let Some(device) = prompt(Platform::Ios)? {
        return Ok(device);
    }
    ios::default_simulator()
}

fn resolve_android(flags: &DeviceFlags, defaults: &Defaults) -> Result<Device> {
    let selection = android_selection(flags, defaults, &Environment::read())?;
    if selection != Selection::Automatic {
        let devices = android::all();
        let (spec, device) = match &selection {
            Selection::Avd(spec) => (
                spec,
                devices
                    .iter()
                    .find(|d| d.kind == Kind::Emulator && android_matches(d, spec)),
            ),
            Selection::Serial(spec) => (
                spec,
                devices
                    .iter()
                    .find(|d| d.serial() == Some(spec) || d.id == *spec),
            ),
            Selection::Device(spec) => (spec, devices.iter().find(|d| android_matches(d, spec))),
            _ => unreachable!("Android selection"),
        };
        if let Some(device) = device {
            if !device.launchable() {
                bail!(
                    "`{}` is not usable: {}",
                    device.label(),
                    device.state_label()
                );
            }
            return Ok(device.clone());
        }
        bail!(
            "No Android device matching `{spec}`. Run `gpui device list --all` to check the selected device."
        );
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
        let defaults = Defaults::from_manifest(manifest).unwrap();
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
        let defaults = Defaults::from_manifest(manifest).unwrap();
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
        assert_eq!(
            Defaults::from_manifest(manifest).unwrap().ios_simulator,
            None
        );
    }

    #[test]
    fn empty_run_values_are_treated_as_absent() {
        let manifest = "[run]\nios_simulator = \"\"\n";
        assert_eq!(
            Defaults::from_manifest(manifest).unwrap().ios_simulator,
            None
        );
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
        let defaults = Defaults::from_manifest(manifest).unwrap();
        assert_eq!(defaults.ios_simulator.as_deref(), Some("iPhone 16e@18.4"));
        assert_eq!(defaults.android_avd.as_deref(), Some("Pixel_9a"));
    }

    #[test]
    fn invalid_toml_is_reported_instead_of_silently_misparsed() {
        let manifest = "[run]\nandroid_avd = Pixel_9a # note\n";
        assert!(Defaults::from_manifest(manifest).is_err());
    }

    #[test]
    fn explicit_ios_selection_and_project_defaults_override_environment() {
        let env = Environment {
            ios_device_id: Some("physical-id".into()),
            ios_simulator: Some("env-sim".into()),
            ..Default::default()
        };
        let defaults = Defaults {
            ios_simulator: Some("project-sim".into()),
            ..Default::default()
        };
        let flags = DeviceFlags {
            sim: Some("cli-sim".into()),
            ..Default::default()
        };
        assert_eq!(
            ios_selection(&flags, &defaults, None, &env).unwrap(),
            Selection::Simulator("cli-sim".into())
        );
        assert_eq!(
            ios_selection(&DeviceFlags::default(), &defaults, None, &env).unwrap(),
            Selection::Simulator("project-sim".into())
        );
        let flags = DeviceFlags {
            device: Some("00008101-000250100252001E".into()),
            ..Default::default()
        };
        assert_eq!(
            ios_selection(&flags, &defaults, None, &env).unwrap(),
            Selection::Device("00008101-000250100252001E".into())
        );
        assert_eq!(
            ios_selection(&DeviceFlags::default(), &Defaults::default(), None, &env).unwrap(),
            Selection::Physical(Some("physical-id".into()))
        );
    }

    #[test]
    fn android_serial_is_used_after_cli_and_project_defaults() {
        let env = Environment {
            android_serial: Some("serial-b".into()),
            ..Default::default()
        };
        let defaults = Defaults {
            android_avd: Some("project-avd".into()),
            ..Default::default()
        };
        let flags = DeviceFlags {
            device: Some("serial-a".into()),
            ..Default::default()
        };
        assert_eq!(
            android_selection(&flags, &defaults, &env).unwrap(),
            Selection::Device("serial-a".into())
        );
        assert_eq!(
            android_selection(&DeviceFlags::default(), &defaults, &env).unwrap(),
            Selection::Avd("project-avd".into())
        );
        assert_eq!(
            android_selection(&DeviceFlags::default(), &Defaults::default(), &env).unwrap(),
            Selection::Serial("serial-b".into())
        );
    }

    #[test]
    fn conflicting_device_flags_are_rejected_before_discovery() {
        let flags = DeviceFlags {
            device_only: true,
            sim: Some("sim".into()),
            ..Default::default()
        };
        assert!(
            ios_selection(&flags, &Defaults::default(), None, &Environment::default()).is_err()
        );
        let flags = DeviceFlags {
            device: Some("serial".into()),
            avd: Some("avd".into()),
            ..Default::default()
        };
        assert!(android_selection(&flags, &Defaults::default(), &Environment::default()).is_err());
    }
}
