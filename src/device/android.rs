//! Android device discovery via AVD metadata, `avdmanager` and `adb`.
//!
//! `emulator`, `avdmanager` and `sdkmanager` are commonly absent from PATH, so
//! they are resolved from `ANDROID_HOME` (see `android_sdk_tool`). AVD
//! metadata is read straight from `~/.android/avd/<name>.avd/config.ini`, which
//! is both the most complete source and the only one that works without the
//! command line tools installed.

use anyhow::{bail, Context, Result};
use colored::*;
use std::path::{Path, PathBuf};

use super::{adb, emulator_binary, try_capture, Device, Kind, Platform, State};

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
            if let Some(image) = &avd.image {
                if let Some(tag) = image.trim_end_matches('/').split('/').nth(2) {
                    detail.push(tag.to_string());
                }
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
            if let Some(name) = &avd_name {
                if avd_names.contains(name) {
                    return None;
                }
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
        if let Some(rest) = line.strip_prefix("id:") {
            if let Some(id) = rest.split('"').nth(1) {
                out.push(id.to_string());
            }
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
    if image.is_none() {
        if let Some(profile) = profile {
            let cli = super::android_cli().context(
                "the `android` CLI is not installed; pass --image to use avdmanager, or install it",
            )?;
            return super::run(
                &cli,
                &["emulator", "create", &format!("--profile={profile}")],
            );
        }
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
    let adb = adb().context("`adb` was not found")?;
    let apk = apk.to_string_lossy().into_owned();
    super::run(&adb, &["-s", serial, "install", "-r", &apk])?;
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
