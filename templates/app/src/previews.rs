//! Explicit preview registry and scenario lifecycle for generated apps.
//!
//! The registry is deliberately metadata-first. A component is previewable
//! only when the application registers it explicitly; the runtime never
//! reflects over arbitrary GPUI `Render` values. The generated app ships
//! three deterministic preview surfaces whose data comes from the selected
//! fixture; applications can add their own descriptors beside this file.

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct PreviewDescriptor {
    pub name: &'static str,
    pub version: &'static str,
    pub fixture_schema: &'static str,
    pub supports_reset: bool,
    pub ready_ids: &'static [&'static str],
    pub logical_ids: &'static [&'static str],
    pub environments: &'static [&'static str],
}

#[derive(Clone, Debug, Default)]
pub struct PreviewRegistry {
    descriptors: Vec<PreviewDescriptor>,
}

impl PreviewRegistry {
    pub fn register(&mut self, descriptor: PreviewDescriptor) -> Result<(), String> {
        if descriptor.name.trim().is_empty()
            || descriptor.version.trim().is_empty()
            || descriptor.fixture_schema.trim().is_empty()
        {
            return Err("preview descriptor name, version and fixture_schema are required".into());
        }
        if self
            .descriptors
            .iter()
            .any(|candidate| candidate.name == descriptor.name)
        {
            return Err(format!(
                "preview component `{}` is registered more than once",
                descriptor.name
            ));
        }
        self.descriptors.push(descriptor);
        Ok(())
    }

    pub fn descriptors(&self) -> &[PreviewDescriptor] {
        &self.descriptors
    }

    pub fn find(&self, name: &str) -> Option<&PreviewDescriptor> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.name == name)
    }

    pub fn manifest(&self) -> Value {
        let mut components = self
            .descriptors
            .iter()
            .map(|descriptor| {
                json!({
                    "name": descriptor.name,
                    "version": descriptor.version,
                    "fixture_schema": descriptor.fixture_schema,
                    "supports_reset": descriptor.supports_reset,
                    "ready_ids": descriptor.ready_ids,
                    "logical_ids": descriptor.logical_ids,
                    "environments": descriptor.environments,
                })
            })
            .collect::<Vec<_>>();
        components.sort_by(|left, right| {
            left["name"]
                .as_str()
                .cmp(&right["name"].as_str())
        });
        json!({
            "schema_version": REGISTRY_SCHEMA_VERSION,
            "components": components,
        })
    }
}

pub fn default_registry() -> PreviewRegistry {
    let mut registry = PreviewRegistry::default();
    registry
        .register(PreviewDescriptor {
            name: "Counter",
            version: "0.1",
            fixture_schema: "Counter",
            supports_reset: true,
            ready_ids: &["counter.value"],
            logical_ids: &["counter.value", "counter.increment"],
            environments: &["desktop"],
        })
        .expect("the built-in Counter preview descriptor is valid");
    registry
        .register(PreviewDescriptor {
            name: "LoginForm",
            version: "0.1",
            fixture_schema: "LoginForm",
            supports_reset: true,
            ready_ids: &["login.password"],
            logical_ids: &[
                "login.username",
                "login.password",
                "login.submit",
                "login.error",
            ],
            environments: &["desktop"],
        })
        .expect("the built-in LoginForm preview descriptor is valid");
    registry
        .register(PreviewDescriptor {
            name: "VirtualList",
            version: "0.1",
            fixture_schema: "VirtualList",
            supports_reset: true,
            ready_ids: &["list.viewport"],
            logical_ids: &["list.viewport", "list.item.<stable-key>"],
            environments: &["desktop"],
        })
        .expect("the built-in VirtualList preview descriptor is valid");
    registry
}

pub fn write_registry_manifest(root: &Path, registry: &PreviewRegistry) -> Result<PathBuf, String> {
    let path = root.join(".gpui/registry-manifest.json");
    let parent = path
        .parent()
        .ok_or_else(|| "registry manifest has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("creating registry directory: {error}"))?;
    let mut bytes = serde_json::to_vec_pretty(&registry.manifest())
        .map_err(|error| format!("serializing registry manifest: {error}"))?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|error| format!("writing registry manifest: {error}"))?;
    Ok(path)
}

#[derive(Clone, Debug)]
struct PreviewState {
    scenario_id: String,
    component: String,
    fixture_hash: String,
    fixture_path: PathBuf,
    fixture: Value,
    theme: String,
    locale: String,
    clock: String,
    clock_at: Option<String>,
    random_seed: Option<String>,
    data_dir: PathBuf,
    uncontrolled_inputs: Vec<String>,
    reset_generation: u64,
}

static STATE: OnceLock<Mutex<Option<PreviewState>>> = OnceLock::new();

fn state() -> &'static Mutex<Option<PreviewState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

fn project_root() -> PathBuf {
    std::env::var_os("GPUI_PREVIEW_PROJECT_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_required(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("preview environment variable {name} is missing"))
}

fn env_optional(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn uncontrolled_inputs() -> Vec<String> {
    std::env::var("GPUI_PREVIEW_UNCONTROLLED_INPUTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn persist_state(state: &PreviewState) -> Result<(), String> {
    let path = state.data_dir.join("runtime.json");
    let body = json!({
        "scenario_id": state.scenario_id,
        "component": state.component,
        "fixture_hash": state.fixture_hash,
        "reset_generation": state.reset_generation,
        "data_dir": state.data_dir,
    });
    let mut bytes = serde_json::to_vec_pretty(&body)
        .map_err(|error| format!("serializing preview runtime state: {error}"))?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|error| format!("writing preview runtime state: {error}"))
}

fn ready_environment(state: &PreviewState) -> String {
    json!({
        "theme": state.theme,
        "locale": state.locale,
        "clock": state.clock,
        "clock_at": state.clock_at,
        "random_seed": state.random_seed,
    })
    .to_string()
}

fn report_ready(state: &PreviewState) {
    let uncontrolled = serde_json::to_string(&state.uncontrolled_inputs)
        .unwrap_or_else(|_| "[]".to_string());
    crate::report_scenario_ready(
        &state.scenario_id,
        &state.component,
        &state.fixture_hash,
        &ready_environment(state),
        state.reset_generation,
        &state.data_dir.to_string_lossy(),
        &uncontrolled,
    );
}

/// Initializes the explicit preview registry and, when launched by
/// `gpui preview`, creates the first isolated scenario state.
pub fn initialize() {
    let root = project_root();
    let registry = default_registry();
    if let Err(error) = write_registry_manifest(&root, &registry) {
        log::warn!("could not write preview registry manifest: {error}");
    }

    let Some(scenario_id) = env_optional("GPUI_PREVIEW_SCENARIO_ID") else {
        return;
    };
    let result = (|| {
        let component = env_required("GPUI_PREVIEW_COMPONENT")?;
        let descriptor = registry
            .find(&component)
            .ok_or_else(|| format!("component `{component}` is not in the preview registry"))?;
        if !descriptor
            .environments
            .iter()
            .any(|environment| *environment == "desktop")
        {
            return Err(format!(
                "component `{component}` does not support the desktop preview environment"
            ));
        }
        let fixture_path = PathBuf::from(env_required("GPUI_PREVIEW_FIXTURE")?);
        let fixture_text = fs::read_to_string(&fixture_path)
            .map_err(|error| format!("reading preview fixture {}: {error}", fixture_path.display()))?;
        let fixture: Value = serde_json::from_str(&fixture_text)
            .map_err(|error| format!("parsing preview fixture {}: {error}", fixture_path.display()))?;
        if fixture.get("component").and_then(Value::as_str) != Some(component.as_str()) {
            return Err(format!(
                "preview fixture component does not match `{component}`"
            ));
        }
        let data_dir = PathBuf::from(env_required("GPUI_PREVIEW_DATA_DIR")?);
        fs::create_dir_all(&data_dir)
            .map_err(|error| format!("creating preview data directory: {error}"))?;
        let preview_state = PreviewState {
            scenario_id,
            component,
            fixture_hash: env_required("GPUI_PREVIEW_FIXTURE_HASH")?,
            fixture_path: fixture_path.clone(),
            fixture,
            theme: env_optional("GPUI_PREVIEW_THEME").unwrap_or_else(|| "light".into()),
            locale: env_optional("GPUI_PREVIEW_LOCALE").unwrap_or_else(|| "en-US".into()),
            clock: env_optional("GPUI_PREVIEW_CLOCK").unwrap_or_else(|| "real".into()),
            clock_at: env_optional("GPUI_PREVIEW_CLOCK_AT"),
            random_seed: env_optional("GPUI_PREVIEW_RANDOM_SEED"),
            data_dir,
            uncontrolled_inputs: uncontrolled_inputs(),
            reset_generation: 1,
        };
        persist_state(&preview_state)?;
        *state()
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(preview_state.clone());
        report_ready(&preview_state);
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        log::error!("preview initialization failed: {error}");
    }
}

/// Resets the registered scenario to its fixture generation. The application
/// owns the actual component state; it must call this hook before rebuilding
/// its view so stale async work cannot cross the new generation.
pub fn reset_generation() -> Result<u64, String> {
    reset_generation_inner(None)
}

/// Resets only the scenario named by the supervisor request. A stale or
/// misrouted request cannot reset a different preview in the same process.
pub fn reset_generation_for(scenario_id: &str) -> Result<u64, String> {
    reset_generation_inner(Some(scenario_id))
}

/// Returns the component selected by the active preview, if any.
pub fn active_component() -> Option<String> {
    state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .map(|preview| preview.component.clone())
}

/// Returns the active preview generation so a generated view can discard its
/// local interaction state when the supervisor completes a reset.
pub fn active_reset_generation() -> Option<u64> {
    state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .map(|preview| preview.reset_generation)
}

fn reset_generation_inner(expected_scenario_id: Option<&str>) -> Result<u64, String> {
    let mut guard = state().lock().unwrap_or_else(|error| error.into_inner());
    let current = guard
        .as_mut()
        .ok_or_else(|| "no preview scenario is active".to_string())?;
    if expected_scenario_id.is_some_and(|scenario_id| scenario_id != current.scenario_id) {
        return Err("scenario_id does not match the active preview".into());
    }
    let fixture_text = fs::read_to_string(&current.fixture_path).map_err(|error| {
        format!(
            "reading preview fixture {}: {error}",
            current.fixture_path.display()
        )
    })?;
    let fixture: Value = serde_json::from_str(&fixture_text)
        .map_err(|error| format!("parsing preview fixture: {error}"))?;
    if fixture.get("component").and_then(Value::as_str) != Some(current.component.as_str()) {
        return Err(format!(
            "preview fixture component does not match `{}`",
            current.component
        ));
    }
    current.fixture = fixture;
    current.reset_generation = current
        .reset_generation
        .checked_add(1)
        .ok_or_else(|| "preview reset_generation overflowed".to_string())?;
    persist_state(current)?;
    let generation = current.reset_generation;
    report_ready(current);
    Ok(generation)
}

/// Returns the Counter fixture's initial value, ignoring Live snapshots while
/// a preview scenario is active.
pub fn counter_initial_value() -> Option<usize> {
    fixture_u64(&["initial_value"]).and_then(|value| usize::try_from(value).ok())
}

/// Returns the fixture increment for the generated Counter surface.
pub fn counter_increment_by() -> Option<usize> {
    fixture_u64(&["increment_by"]).and_then(|value| usize::try_from(value).ok())
}

/// Returns whether the generated Counter surface is disabled by its fixture.
pub fn counter_disabled() -> Option<bool> {
    fixture_bool(&["disabled"])
}

/// Returns the LoginForm fixture's initial username.
pub fn login_initial_username() -> Option<String> {
    fixture_string(&["initial_username"])
}

/// Returns the LoginForm fixture's initial password.
pub fn login_initial_password() -> Option<String> {
    fixture_string(&["initial_password"])
}

/// Returns the deterministic LoginForm response message.
pub fn login_error_message() -> Option<String> {
    fixture_string(&["network", "response", "message"])
}

/// Returns the number of rows declared by the VirtualList fixture.
pub fn virtual_list_item_count() -> Option<usize> {
    fixture_u64(&["items", "count"]).and_then(|value| usize::try_from(value).ok())
}

/// Returns the stable key prefix declared by the VirtualList fixture.
pub fn virtual_list_stable_key_prefix() -> Option<String> {
    fixture_string(&["items", "stable_key_prefix"])
}

/// Returns the number of digits used by VirtualList stable keys.
pub fn virtual_list_stable_key_digits() -> Option<usize> {
    fixture_u64(&["items", "stable_key_digits"]).and_then(|value| usize::try_from(value).ok())
}

/// Returns the label prefix declared by the VirtualList fixture.
pub fn virtual_list_label_prefix() -> Option<String> {
    fixture_string(&["items", "label_prefix"])
}

/// Returns the fixed row height declared by the VirtualList fixture.
pub fn virtual_list_row_height() -> Option<u32> {
    fixture_u64(&["row_height"]).and_then(|value| u32::try_from(value).ok())
}

/// Returns the initial scroll offset declared by the VirtualList fixture.
pub fn virtual_list_initial_scroll_y() -> Option<u32> {
    fixture_u64(&["initial_scroll_y"]).and_then(|value| u32::try_from(value).ok())
}

/// Returns the stable key for a VirtualList row.
pub fn virtual_list_stable_key(index: usize) -> Option<String> {
    let count = virtual_list_item_count()?;
    if index >= count {
        return None;
    }
    let prefix = virtual_list_stable_key_prefix()?;
    let digits = virtual_list_stable_key_digits()?;
    Some(format!("{prefix}{index:0digits$}"))
}

fn fixture_value(path: &[&str]) -> Option<Value> {
    let guard = state().lock().unwrap_or_else(|error| error.into_inner());
    let mut value = guard.as_ref()?.fixture.clone();
    for key in path {
        value = value.get(*key)?.clone();
    }
    Some(value)
}

fn fixture_string(path: &[&str]) -> Option<String> {
    fixture_value(path)?.as_str().map(str::to_owned)
}

fn fixture_bool(path: &[&str]) -> Option<bool> {
    fixture_value(path)?.as_bool()
}

fn fixture_u64(path: &[&str]) -> Option<u64> {
    fixture_value(path)?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_exposes_all_generated_preview_surfaces() {
        let registry = default_registry();
        let names = registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["Counter", "LoginForm", "VirtualList"]);
        assert_eq!(
            registry.find("LoginForm").unwrap().ready_ids,
            ["login.password"]
        );
        assert_eq!(
            registry.find("VirtualList").unwrap().logical_ids,
            ["list.viewport", "list.item.<stable-key>"]
        );
        assert_eq!(registry.manifest()["components"].as_array().unwrap().len(), 3);
    }
}
