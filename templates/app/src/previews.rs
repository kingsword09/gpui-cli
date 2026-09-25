//! Explicit preview registry and scenario lifecycle for generated apps.
//!
//! The registry is deliberately metadata-first. A component is previewable
//! only when the application registers it explicitly; the runtime never
//! reflects over arbitrary GPUI `Render` values. The first generated app
//! registers its Counter surface, while applications can add their own
//! descriptors beside this file.

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
    let mut guard = state().lock().unwrap_or_else(|error| error.into_inner());
    let current = guard
        .as_mut()
        .ok_or_else(|| "no preview scenario is active".to_string())?;
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
    let guard = state().lock().unwrap_or_else(|error| error.into_inner());
    let fixture = guard.as_ref()?.fixture.get("initial_value")?.as_u64()?;
    usize::try_from(fixture).ok()
}
