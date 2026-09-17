use anyhow::{bail, Context, Result};
use colored::*;

use crate::device::{self, android, ios, Device, Kind, Platform};
use crate::DeviceCommands;

pub fn handle_device(command: DeviceCommands) -> Result<()> {
    match command {
        DeviceCommands::List {
            platform,
            all,
            json,
        } => list(platform, all, json),
        DeviceCommands::Create {
            platform,
            name,
            image,
            device,
            profile,
            device_type,
            runtime,
        } => create(platform, name, image, device, profile, device_type, runtime),
        DeviceCommands::Boot { id, last } => boot(id, last),
        DeviceCommands::Shutdown { id, all } => shutdown(id, all),
        DeviceCommands::Remove { id, yes } => remove(id, yes),
    }
}

// ── list ─────────────────────────────────────────────────────────────────────

fn collect(platform: Option<Platform>) -> Vec<Device> {
    match platform {
        Some(Platform::Ios) => ios::all(),
        Some(Platform::Android) => android::all(),
        None => {
            let mut devices = ios::all();
            devices.extend(android::all());
            devices
        }
    }
}

fn list(platform: Option<String>, all: bool, json: bool) -> Result<()> {
    let platform = match platform.as_deref() {
        Some(value) => Some(
            Platform::parse(value)
                .with_context(|| format!("unknown platform `{value}`; expected ios or android"))?,
        ),
        None => None,
    };

    let devices = collect(platform);

    if json {
        println!("{}", serde_json::to_string_pretty(&devices)?);
        return Ok(());
    }

    device::print_devices(&devices, all);
    Ok(())
}

// ── create ───────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn create(
    platform: String,
    name: String,
    image: Option<String>,
    device: Option<String>,
    profile: Option<String>,
    device_type: Option<String>,
    runtime: Option<String>,
) -> Result<()> {
    let platform = Platform::parse(&platform)
        .with_context(|| format!("unknown platform `{platform}`; expected ios or android"))?;

    match platform {
        Platform::Ios => create_ios(&name, device_type, runtime),
        Platform::Android => {
            println!("{}", format!("Creating AVD '{name}'...").bold().cyan());
            android::create_avd(
                &name,
                image.as_deref(),
                device.as_deref(),
                profile.as_deref(),
            )?;
            println!("\n{}", format!("✓ Created AVD '{name}'.").green());
            println!(
                "{}",
                format!("  Boot it with `gpui device boot {name}`.").dimmed()
            );
            Ok(())
        }
    }
}

fn create_ios(name: &str, device_type: Option<String>, runtime: Option<String>) -> Result<()> {
    // With no runtime requested, offer the newest installed one. `simctl create`
    // would default to that anyway, and filtering by it keeps the candidate list
    // to models that actually exist on a current runtime instead of every legacy
    // device type CoreSimulator knows about.
    let runtime_spec = match runtime.as_deref() {
        Some(spec) => Some(
            ios::find_runtime(spec)
                .with_context(|| format!("no installed iOS runtime matching `{spec}`"))?,
        ),
        None => ios::runtimes()?.into_iter().next_back(),
    };

    let types = ios::device_types(runtime_spec.as_ref())?;
    let device_type = match device_type {
        Some(device_type) => device_type,
        None => {
            if types.is_empty() {
                bail!("No iOS device types are available for the requested runtime.");
            }
            if !device::is_interactive() {
                let names = types
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "`--type` is required to create a simulator non-interactively.\n  \
                     Available: {names}"
                );
            }
            let names: Vec<String> = types.iter().map(|t| t.name.clone()).collect();
            inquire::Select::new("Select an iOS device type:", names).prompt()?
        }
    };

    // Accept either a display name or a full identifier.
    let identifier = match types
        .into_iter()
        .find(|t| t.name == device_type || t.identifier == device_type)
    {
        Some(device_type) => device_type.identifier,
        None => device_type.clone(),
    };

    println!(
        "{}",
        format!("Creating simulator '{name}' ({device_type})...")
            .bold()
            .cyan()
    );
    let udid = ios::create(name, &identifier, runtime_spec.as_ref())?;
    println!("\n{}", format!("✓ Created simulator '{name}'.").green());
    println!("  {} {}", "udid:".dimmed(), udid);
    println!(
        "{}",
        format!("  Boot it with `gpui device boot {udid}`.").dimmed()
    );
    Ok(())
}

// ── boot / shutdown / remove ────────────────────────────────────────────────

/// Finds a device across both platforms by id, name or serial.
fn find_any(spec: &str) -> Result<Device> {
    let mut devices = ios::all();
    devices.extend(android::all());

    if let Some(device) = devices
        .iter()
        .find(|d| d.id.eq_ignore_ascii_case(spec) || d.name.eq_ignore_ascii_case(spec))
    {
        return Ok(device.clone());
    }
    if let Some(device) = devices
        .iter()
        .find(|d| d.serial() == Some(spec) || d.name.eq_ignore_ascii_case(spec))
    {
        return Ok(device.clone());
    }

    bail!("No device matching `{spec}`. Run `gpui device list --all` to see what is installed.")
}

/// Chooses a device to boot when nothing was named.
fn pick_for_boot(explicit_last: bool) -> Result<Device> {
    let all = collect(None);

    // `--last` means the most recently used device, by the recency signal each
    // backend can supply (`lastBootedAt`, `lastConnectionDate`, AVD mtime).
    if explicit_last {
        let launchable: Vec<Device> = all.into_iter().filter(Device::launchable).collect();
        let mut with_time: Vec<Device> = launchable
            .iter()
            .filter(|d| d.last_used.is_some())
            .cloned()
            .collect();
        with_time.sort_by_key(|d| std::cmp::Reverse(d.last_used.unwrap_or(0)));
        if let Some(device) = with_time.first() {
            return Ok(device.clone());
        }
        // No timestamps available falls back to the ordinary pick.
        return pick_from(launchable);
    }

    let running: Vec<Device> = all
        .iter()
        .filter(|d| d.is_running() && d.launchable())
        .cloned()
        .collect();
    if let Some(device) = running.first() {
        return Ok(device.clone());
    }

    pick_from(all.into_iter().filter(Device::launchable).collect())
}

fn pick_from(candidates: Vec<Device>) -> Result<Device> {
    if candidates.is_empty() {
        bail!("No device to boot. Create one with `gpui device create --platform <ios|android>`.");
    }

    if !device::is_interactive() {
        bail!(
            "No device specified and nothing is running.\n  \
             Candidates: {}\n  \
             Pass an id, or run `gpui device list` first.",
            candidates
                .iter()
                .map(Device::label)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let label = |d: &Device| format!("{}  [{}]", d.label(), d.kind.label());
    let options: Vec<String> = candidates.iter().map(label).collect();
    let answer = inquire::Select::new("Boot which device?", options).prompt()?;
    let index = candidates
        .iter()
        .position(|d| label(d) == answer)
        .context("could not map the selected entry back to a device")?;
    Ok(candidates[index].clone())
}

fn boot(id: Option<String>, last: bool) -> Result<()> {
    let device = match id {
        Some(id) => find_any(&id)?,
        None => pick_for_boot(last)?,
    };

    if !device.launchable() {
        bail!(
            "`{}` cannot be booted: {}",
            device.label(),
            device.state_label()
        );
    }
    if device.is_running() {
        println!(
            "{}",
            format!("✓ '{}' is already running.", device.label()).green()
        );
        return Ok(());
    }

    let booted = device::inventory::ensure_running(device)?;
    println!("\n{}", format!("✓ Booted '{}'.", booted.label()).green());
    if let Some(serial) = booted.serial() {
        println!("  {} {}", "serial:".dimmed(), serial);
    }
    Ok(())
}

fn shutdown(id: Option<String>, all: bool) -> Result<()> {
    if all {
        return shutdown_all();
    }

    let Some(id) = id else {
        bail!("Provide a device id, or pass --all to shut everything down.");
    };
    let device = find_any(&id)?;

    if !device.is_running() {
        println!(
            "{}",
            format!("'{}' is not running.", device.label()).dimmed()
        );
        return Ok(());
    }

    match device.platform {
        Platform::Ios => ios::shutdown(&device.id)?,
        Platform::Android if device.kind == Kind::Emulator => android::shutdown_avd(&device.id)?,
        Platform::Android => bail!(
            "`{}` is a physical device; disconnect it instead.",
            device.label()
        ),
    }
    println!("\n{}", format!("✓ Shut down '{}'.", device.label()).green());
    Ok(())
}

fn shutdown_all() -> Result<()> {
    let devices = collect(None);
    let mut count = 0usize;

    for device in devices.iter().filter(|d| d.is_running()) {
        let result = match (device.platform, device.kind) {
            (Platform::Ios, _) => ios::shutdown(&device.id),
            (Platform::Android, Kind::Emulator) => android::shutdown_avd(&device.id),
            // Physical devices are left alone.
            (Platform::Android, Kind::Physical) => continue,
        };
        match result {
            Ok(()) => {
                println!("  {} {}", "✓".green(), device.label());
                count += 1;
            }
            Err(error) => println!("  {} {}: {error}", "✗".red(), device.label()),
        }
    }

    if count == 0 {
        println!("{}", "Nothing was running.".dimmed());
    } else {
        println!("\n{}", format!("✓ Shut down {count} device(s).").green());
    }
    Ok(())
}

fn remove(id: String, yes: bool) -> Result<()> {
    let device = find_any(&id)?;

    if device.is_running() {
        bail!(
            "`{}` is running. Shut it down first with `gpui device shutdown {}`.",
            device.label(),
            device.id
        );
    }
    if !device.launchable() {
        println!(
            "{}",
            format!(
                "Note: '{}' is already unusable ({}).",
                device.label(),
                device.state_label()
            )
            .yellow()
        );
    }

    let description = match device.kind {
        Kind::Emulator => "its local data",
        Kind::Physical => "nothing on the device",
    };

    // Deletion is irreversible: confirm interactively, and require an explicit
    // flag when there is no one to ask.
    if !yes {
        if !device::is_interactive() {
            bail!(
                "Refusing to delete '{}' without confirmation.\n  \
                 Re-run with `--yes` to delete it (removes {description}).",
                device.label()
            );
        }
        let confirmed = inquire::Confirm::new(&format!(
            "Delete '{}' ({})? This removes {description}.",
            device.label(),
            device.id,
        ))
        .with_default(false)
        .prompt()
        .unwrap_or(false);
        if !confirmed {
            println!("{}", "Aborted.".dimmed());
            return Ok(());
        }
    }

    match device.platform {
        Platform::Ios => ios::delete(&device.id)?,
        Platform::Android if device.kind == Kind::Emulator => android::remove_avd(&device.id)?,
        Platform::Android => bail!("Refusing to remove a physical device."),
    }
    println!("\n{}", format!("✓ Removed '{}'.", device.label()).green());
    Ok(())
}
