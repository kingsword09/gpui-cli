//! Dev-channel client for `gpui run --live` (debug builds only).
//!
//! Connects back to the CLI's loopback dev server using credentials injected
//! at launch (environment variables, or a config file on platforms with no
//! environment to inherit). Forwards logs and panics to the CLI and receives
//! asset/snapshot instructions from it.
//!
//! Everything here is hand-rolled on `std` on purpose: generated projects get
//! no new dependencies, and release builds compile this module to nothing.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use serde_json::Value;

const PROTO_VERSION: u32 = 2;
const RUNTIME_VERSION: &str = "agent-native-dev-runtime-v1";
const GPUI_VERSION: &str = "{{GPUI_PRE_VERSION}}";
/// Mirrors the CLI's `MAX_FRAME_LEN`.
const MAX_FRAME_LEN: u32 = 1024 * 1024;
/// Outbound queue bound: when the CLI is gone or slow, drop logs instead of
/// stalling the app (panics and logs are best-effort by design).
const OUTBOUND_BOUND: usize = 256;

struct LiveConfig {
    addr: String,
    token: String,
    project: String,
    /// Snapshot session to restore, injected by the CLI on relaunch.
    session: Option<String>,
    /// Where the CLI stored that session's snapshot bytes.
    state_file: Option<std::path::PathBuf>,
}

/// Latest snapshot the view published (`publish_state`), shipped to the CLI
/// when it asks the app to prepare for a restart.
static PUBLISHED_STATE: Mutex<Option<String>> = Mutex::new(None);

/// Snapshot bytes from the previous process, consumed by the view once via
/// `take_restored_state`.
static PENDING_STATE: Mutex<Option<String>> = Mutex::new(None);

/// Explicit developer declarations that bridge a GPUI debug `element_id` to
/// the stable logical id exported to agent tooling. The bridge is deliberately
/// opt-in: `.id()` and the debug tree's `element_id` are never promoted on
/// their own.
static DECLARED_LOGICAL_IDS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Sender half of the current connection; `None` while disconnected.
static OUTBOUND: Mutex<Option<mpsc::SyncSender<String>>> = Mutex::new(None);

/// Asset paths the CLI reported as changed, drained by the UI loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetEvent {
    pub path: String,
    pub transfer_id: String,
    pub asset_revision: u64,
    pub removed: bool,
    pub failed: bool,
    pub error: Option<String>,
}

static ASSET_EVENTS: Mutex<Vec<AssetEvent>> = Mutex::new(Vec::new());

struct PendingAssetBatch {
    transfer_id: String,
    asset_revision: u64,
    events: Vec<AssetEvent>,
}

/// Asset instructions received between `assets_begin` and `assets_commit`.
/// They are deliberately withheld from the UI loop so a partial transaction
/// cannot invalidate only part of a resource set.
static PENDING_ASSET_BATCH: Mutex<Option<PendingAssetBatch>> = Mutex::new(None);

/// The latest manifest the supervisor declared for this connection's run.
static DESIRED_ASSETS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
static DESIRED_TRANSFER_ID: Mutex<Option<String>> = Mutex::new(None);
static DESIRED_ASSET_REVISION: Mutex<u64> = Mutex::new(0);
/// Hashes of assets whose UI-thread cache invalidation has completed.
static APPLIED_ASSETS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
/// Assets explicitly declared by the app as necessary for its current scene.
static REQUIRED_ASSETS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// Required assets successfully read through the live asset source.
static LOADED_REQUIRED_ASSETS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static FAILED_REQUIRED_ASSETS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static LAST_REQUIRED_LOADED_REPORT: Mutex<Option<String>> = Mutex::new(None);
#[cfg(feature = "gpui-dev")]
static LAST_SCENE_COMPLETION_REPORT: Mutex<Option<String>> = Mutex::new(None);

/// Probe requests waiting for the UI thread. The app consumes these from its
/// render/event loop and answers with `respond_ui_probe`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiProbeRequest {
    pub request_id: String,
    pub window_id: String,
    /// When the network thread accepted the probe. The UI adapter can report
    /// this as queue latency without waiting on the network thread.
    pub queued_at: std::time::Instant,
}

/// Semantics requests are accepted by the network thread but always executed
/// by the foreground GPUI context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticsReadRequest {
    pub request_id: String,
    pub window_id: String,
    pub max_bytes: usize,
}

/// Preview reset requests are accepted by the network thread but executed by
/// the foreground GPUI context, just like semantics reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewResetRequest {
    pub request_id: String,
    pub scenario_id: String,
}

static UI_PROBES: Mutex<Vec<UiProbeRequest>> = Mutex::new(Vec::new());
static SEMANTICS_READS: Mutex<Vec<SemanticsReadRequest>> = Mutex::new(Vec::new());
static PREVIEW_RESETS: Mutex<Vec<PreviewResetRequest>> = Mutex::new(Vec::new());
static PENDING_CONTROL: Mutex<Vec<String>> = Mutex::new(Vec::new());
static REGISTERED_WINDOWS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static REGISTERED_WINDOW_HANDLES: Mutex<BTreeMap<String, gpui::AnyWindowHandle>> =
    Mutex::new(BTreeMap::new());
const MAX_SEMANTICS_TREE_BYTES: usize = 256 * 1024;
const PENDING_CONTROL_BOUND: usize = 16;

/// Set by `dev_asset_source()`: only then does the hello advertise the
/// asset-reload capability, so the CLI can skip rebuilds for asset changes.
static ASSET_SOURCE_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Resolves the dev-server credentials, or `None` when not run under
/// `gpui run --live`.
fn resolve_config(config_file: Option<&Path>) -> Option<LiveConfig> {
    let project = std::env::var("GPUI_LIVE_PROJECT").unwrap_or_default();
    let session = std::env::var("GPUI_LIVE_SESSION").ok().filter(|s| !s.is_empty());
    let state_file = std::env::var("GPUI_LIVE_STATE_FILE")
        .ok()
        .map(std::path::PathBuf::from);
    if let (Ok(addr), Ok(token)) = (
        std::env::var("GPUI_LIVE_ADDR"),
        std::env::var("GPUI_LIVE_TOKEN"),
    ) {
        return Some(LiveConfig {
            addr,
            token,
            project,
            session,
            state_file,
        });
    }

    // Android has no inheritable environment: the CLI stages a config file
    // into the app's internal files dir before launch, alongside `gpui_state`
    // when a snapshot is pending.
    let content = std::fs::read_to_string(config_file?).ok()?;
    let mut addr = None;
    let mut token = None;
    let mut file_project = None;
    let mut file_session = None;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("addr=") {
            addr = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("token=") {
            token = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("project=") {
            file_project = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("session=") {
            file_session = Some(value.trim().to_string());
        }
    }
    Some(LiveConfig {
        addr: addr?,
        token: token?,
        project: file_project.unwrap_or(project),
        session: file_session.filter(|s| !s.is_empty()),
        state_file: config_file
            .and_then(Path::parent)
            .map(|dir| dir.join("gpui_state")),
    })
}

/// Installs the panic forwarding hook (chained on top of any existing one) and
/// the log forwarder, then spawns the connection thread.
pub fn init(config_file: Option<&Path>) {
    let Some(config) = resolve_config(config_file) else {
        return;
    };

    // A snapshot from the previous process may be waiting; stage it for the
    // view to pick up. Unreadable or version-mismatched snapshots just mean a
    // cold start — they must never block the app from booting.
    if let (Some(session), Some(state_file)) = (&config.session, &config.state_file) {
        match std::fs::read_to_string(state_file) {
            Ok(data) => {
                *PENDING_STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(data);
            }
            Err(err) => {
                eprintln!("[live] could not restore session {session}: {err}; starting cold");
            }
        }
    }

    // On iOS the CLI pushes asset bytes over the channel (see image_source);
    // advertise that capability up front, before the first render.
    #[cfg(all(debug_assertions, target_os = "ios"))]
    ASSET_SOURCE_INSTALLED.store(true, std::sync::atomic::Ordering::SeqCst);

    install_panic_hook();
    install_log_forwarder();

    let platform = if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    let _ = std::thread::Builder::new()
        .name("gpui-live".into())
        .spawn(move || client_loop(config, platform));
}

/// Resolves the gpui image source for an asset under the dev assets dir.
///
/// Desktop/Android keep the embedded-resource key served by
/// `dev_asset_source`. The iOS runner constructs `Application` internally, so
/// there is no app-level asset source to install; instead, on a debug iOS
/// build we hand gpui a filesystem path (`Resource::Path` loads via
/// `fs::read`), which the simulator resolves on the shared host filesystem
/// through `GPUI_LIVE_ASSETS`. `pump_live_assets` evicts both key styles.
pub fn image_source(name: &str) -> gpui::ImageSource {
    #[cfg(all(debug_assertions, target_os = "ios"))]
    {
        // The iOS runner constructs `Application` internally, so there is no
        // app-level asset source to install. Instead the CLI pushes asset
        // bytes over the dev channel; they land in the app's own tmp dir,
        // which gpui can read via `Resource::Path` (`fs::read`).
        let path = std::env::temp_dir()
            .join("gpui-assets")
            .join(name);
        if path.exists() {
            return gpui::ImageSource::Resource(gpui::Resource::Path(path.into()));
        }
        gpui::ImageSource::Resource(gpui::Resource::Embedded(name.to_string().into()))
    }
    #[cfg(not(all(debug_assertions, target_os = "ios")))]
    {
        gpui::ImageSource::Resource(gpui::Resource::Embedded(name.to_string().into()))
    }
}

/// Declares the stable logical id for one explicitly named GPUI element.
///
/// `element_id` is only a bridge key used within the debug semantics adapter;
/// it is not exported as the logical id. A logical id and an element id may
/// each be declared only once so duplicate mappings fail before a query can
/// pretend they are stable.
pub fn declare_logical_id(element_id: &str, logical_id: &str) -> Result<(), &'static str> {
    if element_id.is_empty() || element_id.len() > 256 || element_id.contains('\0') {
        return Err("invalid_element_id");
    }
    if logical_id.is_empty() || logical_id.len() > 256 || logical_id.contains('\0') {
        return Err("invalid_logical_id");
    }
    let mut declarations = DECLARED_LOGICAL_IDS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if declarations
        .get(element_id)
        .is_some_and(|declared| declared != logical_id)
    {
        return Err("element_id_already_declared");
    }
    if declarations
        .iter()
        .any(|(declared_element, declared_id)| {
            declared_id == logical_id && declared_element != element_id
        })
    {
        return Err("logical_id_already_declared");
    }
    declarations.insert(element_id.to_owned(), logical_id.to_owned());
    Ok(())
}

/// Clears explicit logical-id declarations, for a process-local scenario
/// reset. It does not infer or retain ids from a previous run.
pub fn clear_declared_logical_ids() {
    DECLARED_LOGICAL_IDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

fn enrich_semantics_tree(tree: &str) -> Result<String, &'static str> {
    let declarations = DECLARED_LOGICAL_IDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if declarations.is_empty() {
        return Ok(tree.to_owned());
    }
    let mut value: Value = serde_json::from_str(tree).map_err(|_| "semantics_invalid_json")?;
    let nodes = value
        .get_mut("nodes")
        .and_then(Value::as_object_mut)
        .ok_or("semantics_nodes_missing")?;
    let mut matched = BTreeMap::<String, String>::new();
    for (node_ref, node) in nodes.iter_mut() {
        let Some(element_id) = node.get("element_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(logical_id) = declarations.get(element_id) else {
            continue;
        };
        if let Some(existing) = node.get("logical_id").and_then(Value::as_str) {
            if existing != logical_id {
                return Err("logical_id_conflict_in_tree");
            }
            continue;
        }
        if matched
            .insert(logical_id.clone(), node_ref.clone())
            .is_some()
        {
            return Err("logical_id_duplicate_in_tree");
        }
        node.as_object_mut().expect("node is an object").insert(
            "logical_id".into(),
            Value::String(logical_id.clone()),
        );
    }
    serde_json::to_string(&value).map_err(|_| "semantics_serialization_failed")
}

/// Asset paths reported changed by the CLI since the last call.
pub fn take_asset_events() -> Vec<AssetEvent> {
    std::mem::take(&mut ASSET_EVENTS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Declares an asset that must be successfully read before the current scene
/// can be considered resource-ready. The declaration is intentionally
/// explicit: an asset being transferred does not imply that the app uses it.
pub fn require_asset(path: &str) {
    REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_owned());
    *LAST_REQUIRED_LOADED_REPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn clear_required_assets() {
    REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    LOADED_REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    FAILED_REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *LAST_REQUIRED_LOADED_REPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

fn record_asset_bytes(path: &str, bytes: &[u8]) {
    if !REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(path)
    {
        return;
    }
    let transfer_id = DESIRED_TRANSFER_ID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let loaded = expected_asset_hash(&transfer_id, path)
        .is_some_and(|expected| sha256_hex(bytes) == expected);
    if loaded {
        LOADED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_owned());
        FAILED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
    } else {
        FAILED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_owned());
        LOADED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
    }
    *LAST_REQUIRED_LOADED_REPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

fn record_asset_failure(path: &str) {
    if REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(path)
    {
        FAILED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_owned());
        LOADED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
        *LAST_REQUIRED_LOADED_REPORT
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Sends a deduplicated required-loaded status update. Returns true when a
/// new status was queued for the supervisor.
#[cfg(feature = "gpui-dev")]
pub fn report_required_assets_loaded(window_id: &str, scene_epoch: u64) -> bool {
    let Some(transfer_id) = DESIRED_TRANSFER_ID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return false;
    };
    let asset_revision = *DESIRED_ASSET_REVISION
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let required = REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    if required.is_empty() {
        return false;
    }
    let loaded_set = LOADED_REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let failed_set = FAILED_REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let loaded = required
        .iter()
        .filter(|path| loaded_set.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    let failed = required
        .iter()
        .filter(|path| failed_set.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    let fingerprint = format!(
        "{transfer_id}:{asset_revision}:{window_id}:{scene_epoch}:{required:?}:{loaded:?}:{failed:?}"
    );
    let mut last = LAST_REQUIRED_LOADED_REPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(fingerprint.as_str()) {
        return false;
    }
    *last = Some(fingerprint);
    drop(last);
    let strings = |values: &[String]| {
        values
            .iter()
            .take(128)
            .map(|value| format!("\"{}\"", json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    };
    queue_control(format!(
        "{{\"type\":\"assets_required_loaded\",\"transfer_id\":\"{}\",\"asset_revision\":{asset_revision},\"window_id\":\"{}\",\"scene_epoch\":{scene_epoch},\"required\":[{}],\"loaded\":[{}],\"failed\":[{}]}}",
        json_escape(&transfer_id),
        json_escape(window_id),
        strings(&required),
        strings(&loaded),
        strings(&failed),
    ));
    true
}

/// Reports a scene that the UI adapter has completed for a specific content
/// revision. A presented frame id is optional because most backends do not yet
/// expose a verified present completion callback.
#[cfg(feature = "gpui-dev")]
pub fn report_scene_completed(
    window_id: &str,
    scene_epoch: u64,
    source_revision: u64,
    asset_revision: u64,
    presented_frame_id: Option<&str>,
) -> bool {
    if scene_epoch == 0 {
        return false;
    }
    let fingerprint = format!(
        "{window_id}:{scene_epoch}:{source_revision}:{asset_revision}:{presented_frame_id:?}"
    );
    let mut last = LAST_SCENE_COMPLETION_REPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(fingerprint.as_str()) {
        return false;
    }
    *last = Some(fingerprint);
    drop(last);
    let presented = presented_frame_id
        .map(|value| format!(",\"presented_frame_id\":\"{}\"", json_escape(value)))
        .unwrap_or_default();
    queue_control(format!(
        "{{\"type\":\"scene_completed\",\"window_id\":\"{}\",\"scene_epoch\":{scene_epoch},\"source_revision\":{source_revision},\"asset_revision\":{asset_revision}{presented}}}",
        json_escape(window_id),
    ));
    true
}

fn begin_asset_batch(
    transfer_id: String,
    asset_revision: u64,
    entries: Vec<(String, String)>,
    removed: Vec<String>,
) {
    let touched = entries
        .iter()
        .map(|(path, _)| path.clone())
        .chain(removed.iter().cloned())
        .collect::<BTreeSet<_>>();
    {
        let mut desired = DESIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (path, hash) in entries {
            desired.insert(path, hash);
        }
        for path in removed {
            desired.remove(&path);
        }
    }
    *DESIRED_TRANSFER_ID
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(transfer_id.clone());
    *DESIRED_ASSET_REVISION
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = asset_revision;
    LOADED_REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|path| !touched.contains(path));
    FAILED_REQUIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|path| !touched.contains(path));
    *LAST_REQUIRED_LOADED_REPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    let mut pending = PENDING_ASSET_BATCH
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if pending.as_ref().is_some_and(|batch| {
        batch.transfer_id == transfer_id && batch.asset_revision == asset_revision
    }) {
        return;
    }
    *pending = Some(PendingAssetBatch {
        transfer_id,
        asset_revision,
        events: Vec::new(),
    });
}

fn commit_asset_batch(transfer_id: &str, asset_revision: u64) {
    let events = {
        let mut pending = PENDING_ASSET_BATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(batch) = pending.as_ref() else {
            return;
        };
        if batch.transfer_id != transfer_id || batch.asset_revision != asset_revision {
            return;
        }
        pending.take().map(|batch| batch.events).unwrap_or_default()
    };
    ASSET_EVENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(events);
}

fn discard_pending_asset_batch() {
    PENDING_ASSET_BATCH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
}

fn expected_asset_hash(transfer_id: &str, path: &str) -> Option<String> {
    if DESIRED_TRANSFER_ID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        != Some(transfer_id)
    {
        return None;
    }
    DESIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .cloned()
}

fn queue_asset_event(event: AssetEvent) {
    let mut pending = PENDING_ASSET_BATCH
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(batch) = pending.as_mut().filter(|batch| {
        batch.transfer_id == event.transfer_id && batch.asset_revision == event.asset_revision
    }) {
        batch.events.push(event);
        return;
    }
    drop(pending);
    ASSET_EVENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(event);
}

#[cfg(feature = "gpui-dev")]
pub(crate) fn mark_asset_applied(transfer_id: &str, path: &str, removed: bool) {
    if DESIRED_TRANSFER_ID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        != Some(transfer_id)
    {
        return;
    }
    let mut applied = APPLIED_ASSETS.lock().unwrap_or_else(|e| e.into_inner());
    if removed {
        applied.remove(path);
        return;
    }
    let desired = DESIRED_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(hash) = desired.get(path) {
        applied.insert(path.to_owned(), hash.clone());
    }
}

fn report_assets_reconciled(
    transfer_id: &str,
    asset_revision: u64,
    present: &[String],
    missing: &[String],
    stale: &[String],
    removed: &[String],
) {
    let strings = |values: &[String]| {
        values
            .iter()
            .take(256)
            .map(|value| format!("\"{}\"", json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    };
    queue_control(format!(
        "{{\"type\":\"assets_reconciled\",\"transfer_id\":\"{}\",\"asset_revision\":{asset_revision},\"present\":[{}],\"missing\":[{}],\"stale\":[{}],\"removed\":[{}]}}",
        json_escape(transfer_id),
        strings(present),
        strings(missing),
        strings(stale),
        strings(removed),
    ));
}

/// Publishes the current app state as an opaque snapshot (usually JSON).
///
/// Call this from `render` (or wherever state changes): whatever was published
/// last is what the CLI receives when it asks the app to prepare for a
/// restart, and what the new process restores from. Keep it small — the CLI
/// rejects snapshots above its size limit.
pub fn publish_state(json: &str) {
    *PUBLISHED_STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(json.to_string());
}

/// Announces a generated window to the current supervisor. Registration is
/// retained briefly if the connection is still handshaking.
pub fn register_window(
    window_id: &str,
    title: &str,
    width: u32,
    height: u32,
    scale_milli: u32,
    foreground: bool,
) {
    register_window_internal(
        window_id,
        title,
        width,
        height,
        scale_milli,
        foreground,
        None,
    );
}

/// Registers a window and retains its GPUI handle for UI-thread semantics
/// reads. The handle is opaque to the network thread and is only used through
/// `AnyWindowHandle::update` on the app context.
pub fn register_window_with_handle(
    window_id: &str,
    title: &str,
    width: u32,
    height: u32,
    scale_milli: u32,
    foreground: bool,
    handle: gpui::AnyWindowHandle,
) {
    register_window_internal(
        window_id,
        title,
        width,
        height,
        scale_milli,
        foreground,
        Some(handle),
    );
}

fn register_window_internal(
    window_id: &str,
    title: &str,
    width: u32,
    height: u32,
    scale_milli: u32,
    foreground: bool,
    handle: Option<gpui::AnyWindowHandle>,
) {
    remember_window(window_id);
    let mut handles = REGISTERED_WINDOW_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = handle {
        handles.insert(window_id.to_owned(), handle);
    } else {
        handles.remove(window_id);
    }
    drop(handles);
    queue_control(format!(
        "{{\"type\":\"window_registered\",\"window_id\":\"{}\",\"title\":\"{}\",\"width\":{},\"height\":{},\"scale_milli\":{},\"foreground\":{}}}",
        json_escape(window_id),
        json_escape(title),
        width,
        height,
        scale_milli,
        foreground,
    ));
}

/// Announces a generated window closure to the supervisor.
pub fn close_window(window_id: &str, reason: Option<&str>) {
    forget_window(window_id);
    let reason = reason
        .map(|value| format!(",\"reason\":\"{}\"", json_escape(value)))
        .unwrap_or_default();
    queue_control(format!(
        "{{\"type\":\"window_closed\",\"window_id\":\"{}\"{reason}}}",
        json_escape(window_id),
    ));
}

/// Returns whether the runtime still considers a window registered.
pub fn window_is_registered(window_id: &str) -> bool {
    REGISTERED_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|registered| registered == window_id)
}

/// Drains probe requests for execution on the UI thread.
pub fn take_ui_probe_requests() -> Vec<UiProbeRequest> {
    std::mem::take(&mut UI_PROBES.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Drains semantics requests for execution on the UI thread.
pub fn take_semantics_read_requests() -> Vec<SemanticsReadRequest> {
    std::mem::take(&mut SEMANTICS_READS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Drains bounded preview reset requests for execution on the UI thread.
pub fn take_preview_reset_requests() -> Vec<PreviewResetRequest> {
    std::mem::take(&mut PREVIEW_RESETS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Sends the result of a UI-thread probe back to the supervisor.
pub fn respond_ui_probe(
    request_id: &str,
    window_id: &str,
    responsive: bool,
    latency_ms: Option<u64>,
) {
    let latency = latency_ms
        .map(|value| format!(",\"latency_ms\":{value}"))
        .unwrap_or_default();
    queue_control(format!(
        "{{\"type\":\"ui_probe_result\",\"request_id\":\"{}\",\"window_id\":\"{}\",\"responsive\":{responsive}{latency}}}",
        json_escape(request_id),
        json_escape(window_id),
    ));
}

/// Reports one bounded semantics read. A tree is sent as an escaped JSON
/// string so the app-channel envelope remains unambiguous and bounded.
pub fn respond_semantics_read(
    request_id: &str,
    window_id: &str,
    status: &str,
    a11y_active: bool,
    tree_json: Option<&str>,
    reason: Option<&str>,
    captured_at_ms: u64,
) {
    let tree = tree_json
        .map(|tree| format!(",\"tree_json\":\"{}\"", json_escape(tree)))
        .unwrap_or_default();
    let reason = reason
        .map(|reason| format!(",\"reason\":\"{}\"", json_escape(reason)))
        .unwrap_or_default();
    queue_control(format!(
        "{{\"type\":\"semantics_result\",\"request_id\":\"{}\",\"window_id\":\"{}\",\"status\":\"{}\",\"a11y_active\":{a11y_active}{tree}{reason},\"captured_at_ms\":{captured_at_ms}}}",
        json_escape(request_id),
        json_escape(window_id),
        json_escape(status),
    ));
}

/// Executes a semantics query on the GPUI foreground context. No network or
/// file I/O occurs inside the window callback; the immutable tree string is
/// handed back to the channel writer after the short UI read completes.
pub fn answer_semantics_read(cx: &mut gpui::App, request: SemanticsReadRequest) {
    let captured_at_ms = now_ms();
    let handle = REGISTERED_WINDOW_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&request.window_id)
        .copied();
    let Some(handle) = handle else {
        respond_semantics_read(
            &request.request_id,
            &request.window_id,
            "unavailable",
            false,
            None,
            Some("window_handle_unavailable"),
            captured_at_ms,
        );
        return;
    };
    let result = handle.update(cx, |_, window, _| {
        let active = window.is_a11y_active();
        let tree = active.then(|| window.debug_a11y_tree_json()).flatten();
        (active, tree)
    });
    match result {
        Ok((true, Some(tree))) => match enrich_semantics_tree(&tree) {
            Ok(tree) if tree.len() <= request.max_bytes => respond_semantics_read(
                &request.request_id,
                &request.window_id,
                "ready",
                true,
                Some(&tree),
                None,
                captured_at_ms,
            ),
            Ok(_) => respond_semantics_read(
                &request.request_id,
                &request.window_id,
                "too_large",
                true,
                None,
                Some("tree_exceeds_request_limit"),
                captured_at_ms,
            ),
            Err(reason) => respond_semantics_read(
                &request.request_id,
                &request.window_id,
                "unavailable",
                true,
                None,
                Some(reason),
                captured_at_ms,
            ),
        },
        Ok((false, _)) => respond_semantics_read(
            &request.request_id,
            &request.window_id,
            "inactive",
            false,
            None,
            Some("a11y_inactive"),
            captured_at_ms,
        ),
        Ok((true, None)) => respond_semantics_read(
            &request.request_id,
            &request.window_id,
            "unavailable",
            true,
            None,
            Some("tree_unavailable"),
            captured_at_ms,
        ),
        Err(_) => respond_semantics_read(
            &request.request_id,
            &request.window_id,
            "unavailable",
            false,
            None,
            Some("window_closed"),
            captured_at_ms,
        ),
    }
}

/// Reports whether a preview reset was accepted and which generation was
/// created. A successful reset also emits the normal `scenario_ready` event
/// from the preview registry.
pub fn respond_scenario_reset(
    request_id: &str,
    scenario_id: &str,
    accepted: bool,
    reset_generation: u64,
    reason: Option<&str>,
) {
    let reason = reason
        .map(|value| format!(",\"reason\":\"{}\"", json_escape(value)))
        .unwrap_or_default();
    queue_control(format!(
        "{{\"type\":\"scenario_reset_result\",\"request_id\":\"{}\",\"scenario_id\":\"{}\",\"accepted\":{accepted},\"reset_generation\":{reset_generation}{reason}}}",
        json_escape(request_id),
        json_escape(scenario_id),
    ));
}

/// Announces that the preview runtime created a fresh scenario state. The
/// environment and uncontrolled-input values are already serialized JSON
/// fragments produced by `previews.rs`; keeping this helper string-based lets
/// the generated app retain its dependency-light dev channel.
pub fn report_scenario_ready(
    scenario_id: &str,
    component: &str,
    fixture_hash: &str,
    environment_json: &str,
    reset_generation: u64,
    data_dir: &str,
    uncontrolled_inputs_json: &str,
) {
    queue_control(format!(
        "{{\"type\":\"scenario_ready\",\"scenario_id\":\"{}\",\"component\":\"{}\",\"fixture_hash\":\"{}\",\"environment\":{},\"reset_generation\":{},\"data_dir\":\"{}\",\"uncontrolled_inputs\":{}}}",
        json_escape(scenario_id),
        json_escape(component),
        json_escape(fixture_hash),
        environment_json,
        reset_generation,
        json_escape(data_dir),
        uncontrolled_inputs_json,
    ));
}

/// Acknowledges an asset batch after the UI thread has invalidated its cache.
pub fn report_assets_applied(
    transfer_id: &str,
    asset_revision: u64,
    applied: &[String],
    failed: &[String],
    cache_invalidated: bool,
) {
    let strings = |values: &[String]| {
        values
            .iter()
            .take(128)
            .map(|value| format!("\"{}\"", json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    };
    queue_control(format!(
        "{{\"type\":\"assets_applied\",\"transfer_id\":\"{}\",\"asset_revision\":{asset_revision},\"applied\":[{}],\"failed\":[{}],\"cache_invalidated\":{cache_invalidated}}}",
        json_escape(transfer_id),
        strings(applied),
        strings(failed),
    ));
}

fn report_assets_received(
    transfer_id: &str,
    asset_revision: u64,
    received: &[String],
    failed: &[String],
) {
    let strings = |values: &[String]| {
        values
            .iter()
            .take(128)
            .map(|value| format!("\"{}\"", json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    };
    queue_control(format!(
        "{{\"type\":\"assets_received\",\"transfer_id\":\"{}\",\"asset_revision\":{asset_revision},\"received\":[{}],\"failed\":[{}]}}",
        json_escape(transfer_id),
        strings(received),
        strings(failed),
    ));
}

/// Builds a single-number snapshot body, e.g. `"clicks":3` -> `"{"clicks":3}"`.
pub fn snapshot_json_number(key: &str, value: usize) -> String {
    ["{\"".to_owned(), key.to_owned(), "\":".to_owned(), value.to_string(), "}".to_owned()].concat()
}

/// Returns the previous process's snapshot, if one was saved, consuming it.
///
/// Call this when constructing your root view; data that fails to parse means
/// the caller falls back to its defaults (a cold start).
pub fn take_restored_state() -> Option<String> {
    std::mem::take(&mut PENDING_STATE.lock().unwrap_or_else(|e| e.into_inner()))
}

/// A file-backed asset source for live development.
///
/// `img("assets/foo.png")` resolves through this source, reading straight from
/// disk every time so the CLI can evict GPUI's cache entry on change and the
/// next render picks the new bytes up without a rebuild.
///
/// Roots are tried in order: the `GPUI_LIVE_ASSETS` directory the CLI exports
/// (desktop and iOS simulator), the process working directory's `assets/`
/// (desktop), and finally `extra_root` (the app files dir on Android, where
/// the CLI pushes changed assets over adb).
pub struct DevAssetSource {
    roots: Vec<std::path::PathBuf>,
}

pub fn dev_asset_source(extra_root: Option<std::path::PathBuf>) -> DevAssetSource {
    ASSET_SOURCE_INSTALLED.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut roots = Vec::new();
    if let Ok(dir) = std::env::var("GPUI_LIVE_ASSETS") {
        roots.push(std::path::PathBuf::from(dir));
    }
    roots.push(std::path::PathBuf::from("assets"));
    if let Some(root) = extra_root {
        roots.push(root);
    }
    DevAssetSource { roots }
}

/// Asset source used when a debug build does not explicitly enable gpui-dev.
pub fn disabled_asset_source() -> DevAssetSource {
    DevAssetSource { roots: Vec::new() }
}

impl DevAssetSource {
    /// `img("assets/x.png")` keys carry the `assets/` prefix, but every root
    /// already *is* the assets dir — strip it before joining.
    fn relative(path: &str) -> &str {
        path.strip_prefix("assets/").unwrap_or(path)
    }
}

impl gpui::AssetSource for DevAssetSource {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        for root in &self.roots {
            if let Ok(bytes) = std::fs::read(root.join(Self::relative(path))) {
                let expected = DESIRED_TRANSFER_ID
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .and_then(|transfer_id| expected_asset_hash(&transfer_id, path));
                if expected
                    .as_deref()
                    .is_some_and(|expected| sha256_hex(&bytes) != expected)
                {
                    record_asset_failure(path);
                    return Ok(None);
                }
                record_asset_bytes(path, &bytes);
                return Ok(Some(std::borrow::Cow::Owned(bytes)));
            }
        }
        record_asset_failure(path);
        Ok(None)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        let mut out = Vec::new();
        for root in &self.roots {
            let dir = root.join(Self::relative(path));
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    let prefix = if path.is_empty() {
                        String::new()
                    } else {
                        format!("{path}/")
                    };
                    out.push(gpui::SharedString::from(format!("{prefix}{name}")));
                }
            }
            break; // first root that exists wins; list is a dev convenience
        }
        Ok(out)
    }
}

fn client_loop(config: LiveConfig, platform: &'static str) {
    loop {
        run_connection(&config, platform);
        // The CLI may be mid-rebuild; keep trying quietly.
        thread::sleep(Duration::from_secs(2));
    }
}

fn run_connection(config: &LiveConfig, platform: &'static str) {
    let Ok(mut stream) = connect_with_retry(&config.addr) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let Ok(mut reader) = stream.try_clone() else {
        return;
    };

    let hello = format!(
        "{{\"type\":\"hello\",\"proto\":{PROTO_VERSION},\"token\":\"{}\",\"project\":\"{}\",\"pid\":{},\"platform\":\"{platform}\",\"asset_reload\":{},\"runtime_version\":\"{RUNTIME_VERSION}\",\"gpui_version\":\"{GPUI_VERSION}\",\"capabilities\":[\"logs\",\"panic\",\"state\",\"semantics.read\",\"semantics.logical_id\"{}{}]}}",
        json_escape(&config.token),
        json_escape(&config.project),
        std::process::id(),
        ASSET_SOURCE_INSTALLED.load(std::sync::atomic::Ordering::SeqCst),
        if ASSET_SOURCE_INSTALLED.load(std::sync::atomic::Ordering::SeqCst) {
            ",\"asset_reload\",\"asset_manifest\""
        } else {
            ""
        },
        if std::env::var_os("GPUI_PREVIEW_SCENARIO_ID").is_some() {
            ",\"scenario.reset\""
        } else {
            ""
        },
    );
    if write_frame(&mut stream, hello.as_bytes()).is_err() {
        return;
    }
    match read_frame(&mut reader) {
        Ok(ref frame) if find_bytes(frame, b"hello_ok") => {}
        _ => return,
    }

    let (tx, rx) = mpsc::sync_channel::<String>(OUTBOUND_BOUND);
    {
        let mut guard = OUTBOUND.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(tx);
    }
    flush_pending_control();
    let writer_handle = thread::Builder::new()
        .name("gpui-live-write".into())
        .spawn(move || {
            for payload in rx {
                if write_frame(&mut stream, payload.as_bytes()).is_err() {
                    break;
                }
            }
        })
        .ok();

    loop {
        match read_frame(&mut reader) {
            Ok(frame) => dispatch(&frame, config),
            Err(_) => break,
        }
    }

    // Dropping the sender ends the writer thread; the CLI sees the disconnect.
    {
        let mut guard = OUTBOUND.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    if let Some(handle) = writer_handle {
        let _ = handle.join();
    }
}

fn connect_with_retry(addr: &str) -> std::io::Result<TcpStream> {
    // The app can start before `adb reverse` lands; retry briefly.
    for attempt in 0..8 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(400));
        }
        if let Ok(stream) = TcpStream::connect(addr) {
            return Ok(stream);
        }
    }
    TcpStream::connect(addr)
}

/// Handles one server message. Frames come from our own CLI, so a targeted
/// scan for the known fields is enough (no JSON parser).
fn dispatch(frame: &[u8], config: &LiveConfig) {
    if find_bytes(frame, b"\"asset_manifest\"") {
        let Some(transfer_id) = string_field(frame, "transfer_id") else {
            return;
        };
        let Some(asset_revision) = number_field(frame, "asset_revision") else {
            return;
        };
        let entries = object_entries(frame, "entries");
        let desired: BTreeMap<String, String> = entries.into_iter().collect();
        let applied = APPLIED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut present = Vec::new();
        let mut missing = Vec::new();
        let mut stale = Vec::new();
        for (path, hash) in &desired {
            match applied.get(path) {
                Some(previous) if previous == hash => present.push(path.clone()),
                Some(_) => stale.push(path.clone()),
                None => missing.push(path.clone()),
            }
        }
        let removed = applied
            .keys()
            .filter(|path| !desired.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        discard_pending_asset_batch();
        *DESIRED_ASSETS.lock().unwrap_or_else(|e| e.into_inner()) = desired;
        *DESIRED_TRANSFER_ID
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(transfer_id.clone());
        *DESIRED_ASSET_REVISION
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = asset_revision;
        LOADED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        FAILED_REQUIRED_ASSETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *LAST_REQUIRED_LOADED_REPORT
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        report_assets_reconciled(
            &transfer_id,
            asset_revision,
            &present,
            &missing,
            &stale,
            &removed,
        );
        if missing.is_empty() && stale.is_empty() && removed.is_empty() {
            report_assets_applied(&transfer_id, asset_revision, &[], &[], true);
        }
    } else if find_bytes(frame, b"\"assets_begin\"") {
        let (Some(transfer_id), Some(asset_revision)) = (
            string_field(frame, "transfer_id"),
            number_field(frame, "asset_revision"),
        ) else {
            return;
        };
        begin_asset_batch(
            transfer_id,
            asset_revision,
            object_entries(frame, "entries"),
            string_array(frame, "removed"),
        );
    } else if find_bytes(frame, b"\"assets_commit\"") {
        let (Some(transfer_id), Some(asset_revision)) = (
            string_field(frame, "transfer_id"),
            number_field(frame, "asset_revision"),
        ) else {
            return;
        };
        commit_asset_batch(&transfer_id, asset_revision);
    } else if find_bytes(frame, b"\"asset_removed\"") {
        if let Some(path) = string_field(frame, "path") {
            let transfer_id = string_field(frame, "transfer_id").unwrap_or_default();
            let asset_revision = number_field(frame, "asset_revision").unwrap_or(0);
            queue_asset_event(AssetEvent {
                path: path.clone(),
                transfer_id: transfer_id.clone(),
                asset_revision,
                removed: true,
                failed: false,
                error: None,
            });
            report_assets_received(&transfer_id, asset_revision, &[path.clone()], &[]);
        }
    } else if find_bytes(frame, b"\"asset_changed\"") {
        if let Some(path) = string_field(frame, "path") {
            let transfer_id = string_field(frame, "transfer_id").unwrap_or_default();
            let asset_revision = number_field(frame, "asset_revision").unwrap_or(0);
            queue_asset_event(AssetEvent {
                path: path.clone(),
                transfer_id: transfer_id.clone(),
                asset_revision,
                removed: false,
                failed: false,
                error: None,
            });
            report_assets_received(&transfer_id, asset_revision, &[path.clone()], &[]);
        }
    } else if find_bytes(frame, b"\"asset_data\"") {
        if let Some(path) = string_field(frame, "path") {
            let asset_revision = number_field(frame, "asset_revision").unwrap_or(0);
            let transfer_id = string_field(frame, "transfer_id").unwrap_or_default();
            let data = string_field(frame, "data");
            if let Some(bytes) = data.as_deref().and_then(b64_decode) {
                let expected_hash = expected_asset_hash(&transfer_id, &path);
                let target = std::env::temp_dir()
                    .join("gpui-assets")
                    .join(&path);
                let temporary = target.with_extension("gpui-tmp");
                let result = if expected_hash
                    .as_deref()
                    .is_some_and(|expected| sha256_hex(&bytes) == expected)
                {
                    (|| -> Result<(), &'static str> {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).map_err(|_| "write_failed")?;
                        }
                        std::fs::write(&temporary, bytes).map_err(|_| "write_failed")?;
                        std::fs::rename(&temporary, &target).map_err(|_| "write_failed")?;
                        Ok(())
                    })()
                } else {
                    Err("hash_mismatch")
                };
                if result.is_ok() {
                    report_assets_received(&transfer_id, asset_revision, &[path.clone()], &[]);
                    queue_asset_event(AssetEvent {
                        path,
                        transfer_id: transfer_id.clone(),
                        asset_revision,
                        removed: false,
                        failed: false,
                        error: None,
                    });
                } else {
                    let _ = std::fs::remove_file(&temporary);
                    report_assets_received(&transfer_id, asset_revision, &[], &[path.clone()]);
                    queue_asset_event(AssetEvent {
                        path,
                        transfer_id: transfer_id.clone(),
                        asset_revision,
                        removed: false,
                        failed: true,
                        error: result.err().map(str::to_owned),
                    });
                }
            } else {
                report_assets_received(&transfer_id, asset_revision, &[], &[path.clone()]);
                queue_asset_event(AssetEvent {
                    path,
                    transfer_id,
                    asset_revision,
                    removed: false,
                    failed: true,
                    error: Some(if data.is_some() {
                        "invalid_base64"
                    } else {
                        "missing_data"
                    }
                    .into()),
                });
            }
        }
    } else if find_bytes(frame, b"\"probe_ui\"") {
        if let (Some(request_id), Some(window_id)) = (
            string_field(frame, "request_id"),
            string_field(frame, "window_id"),
        ) {
            UI_PROBES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(UiProbeRequest {
                    request_id,
                    window_id,
                    queued_at: std::time::Instant::now(),
                });
        }
    } else if find_bytes(frame, b"\"semantics_query\"") {
        if let (Some(request_id), Some(window_id)) = (
            string_field(frame, "request_id"),
            string_field(frame, "window_id"),
        ) {
            let max_bytes = number_field(frame, "max_bytes")
                .unwrap_or(MAX_SEMANTICS_TREE_BYTES as u64)
                .min(MAX_SEMANTICS_TREE_BYTES as u64) as usize;
            let mut requests = SEMANTICS_READS.lock().unwrap_or_else(|e| e.into_inner());
            if requests.len() < PENDING_CONTROL_BOUND {
                requests.push(SemanticsReadRequest {
                    request_id,
                    window_id,
                    max_bytes,
                });
            } else {
                // A bounded queue is preferable to silently growing work on
                // the UI thread. The supervisor deadline reports the missed
                // request as unavailable if this queue is full.
            }
        }
    } else if find_bytes(frame, b"\"scenario_reset\"") {
        if let (Some(request_id), Some(scenario_id)) = (
            string_field(frame, "request_id"),
            string_field(frame, "scenario_id"),
        ) {
            let mut requests = PREVIEW_RESETS.lock().unwrap_or_else(|e| e.into_inner());
            if requests.len() < PENDING_CONTROL_BOUND {
                requests.push(PreviewResetRequest {
                    request_id,
                    scenario_id,
                });
            } else if let (Some(request_id), Some(scenario_id)) = (
                string_field(frame, "request_id"),
                string_field(frame, "scenario_id"),
            ) {
                respond_scenario_reset(
                    &request_id,
                    &scenario_id,
                    false,
                    0,
                    Some("reset_queue_full"),
                );
            }
        }
    } else if find_bytes(frame, b"\"prepare_restart\"") {
        let Some(session) = string_field(frame, "session") else {
            return;
        };
        // Ship the latest published snapshot; with none published the CLI
        // treats the restart as a plain cold start.
        let data = PUBLISHED_STATE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        let payload = format!(
            "{{\"type\":\"state_saved\",\"session\":\"{}\",\"data\":\"{}\"}}",
            json_escape(&session),
            json_escape(&data),
        );
        send(payload);
    }
    let _ = config;
}

// ── panic + log forwarding ───────────────────────────────────────────────────

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let message = payload_message(info);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        // Bounded, non-blocking: a dying process must not hang or allocate wildly.
        let backtrace = {
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            trace.chars().take(8 * 1024).collect::<String>()
        };
        let payload = format!(
            "{{\"type\":\"panic\",\"message\":\"{}\",\"location\":\"{}\",\"backtrace\":\"{}\"}}",
            json_escape(&message),
            json_escape(&location),
            json_escape(&backtrace),
        );
        send(payload);
    }));
}

fn payload_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "Box<dyn Any> panic payload".to_string()
    }
}

struct LiveLogger;

impl log::Log for LiveLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let payload = format!(
            "{{\"type\":\"log\",\"level\":\"{}\",\"target\":\"{}\",\"message\":\"{}\"}}",
            record.level().to_string().to_lowercase(),
            json_escape(record.target()),
            json_escape(&record.args().to_string()),
        );
        // While the CLI is attached it prints forwarded logs itself — writing
        // them here as well would show every line twice.
        if !send(payload) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn install_log_forwarder() {
    // No-op if a platform logger already exists (e.g. `android_logger` on
    // Android); those platforms keep their native log destination.
    if log::set_boxed_logger(Box::new(LiveLogger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// Queues a payload for the CLI; returns false when there is no connection
/// (or the queue is full), so callers can fall back to local output.
fn send(payload: String) -> bool {
    let Ok(guard) = OUTBOUND.lock() else {
        return false;
    };
    match guard.as_ref() {
        Some(sender) => sender.try_send(payload).is_ok(),
        None => false,
    }
}

fn queue_control(payload: String) {
    if send(payload.clone()) {
        return;
    }
    let mut pending = PENDING_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if pending.len() < PENDING_CONTROL_BOUND {
        pending.push(payload);
    }
}

fn flush_pending_control() {
    let pending = {
        let mut guard = PENDING_CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };
    for payload in pending {
        let _ = send(payload);
    }
}

fn remember_window(window_id: &str) {
    let mut windows = REGISTERED_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !windows.iter().any(|registered| registered == window_id) {
        windows.push(window_id.to_string());
    }
}

fn forget_window(window_id: &str) {
    REGISTERED_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|registered| registered != window_id);
    REGISTERED_WINDOW_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(window_id);
}

// ── minimal JSON plumbing ────────────────────────────────────────────────────

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn write_frame(stream: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(payload.len() as u32).to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut len_bytes = [0u8; 4];
    stream.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len as u32 > MAX_FRAME_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(body)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn number_field(frame: &[u8], key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":").into_bytes();
    let start = frame
        .windows(needle.len())
        .position(|window| window == &needle[..])?
        + needle.len();
    let end = start
        + frame[start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .unwrap_or(frame.len() - start);
    std::str::from_utf8(&frame[start..end]).ok()?.parse().ok()
}

/// Extracts an escaped string field's value from a JSON frame.
fn string_field(frame: &[u8], key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"").into_bytes();
    let start = frame
        .windows(needle.len())
        .position(|window| window == &needle[..])?
        + needle.len();

    let mut out: Vec<u8> = Vec::new();
    let mut i = start;
    while i < frame.len() {
        match frame[i] {
            b'"' => break,
            b'\\' => {
                i += 1;
                if i >= frame.len() {
                    return None;
                }
                match frame[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        if i + 4 >= frame.len() {
                            return None;
                        }
                        let hex = std::str::from_utf8(&frame[i + 1..i + 5]).ok()?;
                        let code = u32::from_str_radix(hex, 16).unwrap_or(0xFFFD);
                        let mut buffer = [0u8; 4];
                        out.extend_from_slice(
                            char::from_u32(code)
                                .unwrap_or('\u{FFFD}')
                                .encode_utf8(&mut buffer)
                                .as_bytes(),
                        );
                        i += 4;
                    }
                    other => out.push(other),
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Extracts path/hash pairs from the bounded manifest array. Generated apps
/// deliberately avoid a JSON dependency, so this parser only accepts the
/// object shape emitted by the supervisor and still respects escaped strings.
fn object_entries(frame: &[u8], key: &str) -> Vec<(String, String)> {
    let needle = format!("\"{key}\":").into_bytes();
    let Some(position) = frame
        .windows(needle.len())
        .position(|window| window == needle.as_slice())
    else {
        return Vec::new();
    };
    let mut start = position + needle.len();
    while frame.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    if frame.get(start) != Some(&b'[') {
        return Vec::new();
    }
    let mut entries = Vec::new();
    let mut index = start + 1;
    while index < frame.len() {
        while frame
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
            || frame.get(index) == Some(&b',')
        {
            index += 1;
        }
        if frame.get(index) == Some(&b']') {
            break;
        }
        if frame.get(index) != Some(&b'{') {
            break;
        }
        let object_start = index;
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        while index < frame.len() {
            let byte = frame[index];
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
            } else if byte == b'"' {
                in_string = true;
            } else if byte == b'{' {
                depth += 1;
            } else if byte == b'}' {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    index += 1;
                    break;
                }
            }
            index += 1;
        }
        let object = &frame[object_start..index.min(frame.len())];
        if let (Some(path), Some(hash)) = (string_field(object, "path"), string_field(object, "hash")) {
            entries.push((path, hash));
        }
    }
    entries
}

fn string_array(frame: &[u8], key: &str) -> Vec<String> {
    let needle = format!("\"{key}\":").into_bytes();
    let Some(position) = frame
        .windows(needle.len())
        .position(|window| window == needle.as_slice())
    else {
        return Vec::new();
    };
    let mut index = position + needle.len();
    while frame.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    if frame.get(index) != Some(&b'[') {
        return Vec::new();
    }
    index += 1;
    let mut values = Vec::new();
    while index < frame.len() {
        while frame
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
            || frame.get(index) == Some(&b',')
        {
            index += 1;
        }
        if frame.get(index) == Some(&b']') {
            break;
        }
        if frame.get(index) != Some(&b'"') {
            break;
        }
        let start = index + 1;
        let mut out = Vec::new();
        index = start;
        while index < frame.len() {
            match frame[index] {
                b'"' => {
                    let Ok(value) = String::from_utf8(out) else {
                        return Vec::new();
                    };
                    values.push(value);
                    index += 1;
                    break;
                }
                b'\\' => {
                    index += 1;
                    let Some(&escaped) = frame.get(index) else {
                        return Vec::new();
                    };
                    match escaped {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let Some(hex_bytes) = frame.get(index + 1..index + 5) else {
                                return Vec::new();
                            };
                            let Ok(hex) = std::str::from_utf8(hex_bytes) else {
                                return Vec::new();
                            };
                            let code = u32::from_str_radix(hex, 16).unwrap_or(0xFFFD);
                            let mut buffer = [0u8; 4];
                            out.extend_from_slice(
                                char::from_u32(code)
                                    .unwrap_or('\u{FFFD}')
                                    .encode_utf8(&mut buffer)
                                    .as_bytes(),
                            );
                            index += 4;
                        }
                        other => out.push(other),
                    }
                }
                byte => out.push(byte),
            }
            index += 1;
        }
    }
    values
}

fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let mut padded = Vec::with_capacity((bytes.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(bytes);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&(bytes.len() as u64 * 8).to_be_bytes());

    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    for chunk in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let mut working = state;
        for index in 0..64 {
            let sigma1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choice = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(sigma1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sigma0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority = (working[0] & working[1])
                ^ (working[0] & working[2])
                ^ (working[1] & working[2]);
            let temp2 = sigma0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for (value, addend) in state.iter_mut().zip(working) {
            *value = (*value).wrapping_add(addend);
        }
    }

    let mut output = String::with_capacity(64);
    for value in state {
        output.push_str(&format!("{value:08x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn string_field_unescapes_known_sequences() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let frame = b"{\"type\":\"asset_changed\",\"path\":\"assets/a b\\\"c.png\"}";
        assert_eq!(string_field(frame, "path").as_deref(), Some("assets/a b\"c.png"));
        assert_eq!(string_field(frame, "type").as_deref(), Some("asset_changed"));
        assert_eq!(string_field(frame, "missing"), None);
    }

    #[test]
    fn manifest_entries_are_parsed_and_reconciled() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let entries = object_entries(
            br#"{"type":"asset_manifest","transfer_id":"t3","asset_revision":3,"entries":[{"path":"assets/a.png","hash":"aaa"},{"path":"assets/b b.png","hash":"bbb"}]}"#,
            "entries",
        );
        assert_eq!(
            entries,
            vec![
                ("assets/a.png".into(), "aaa".into()),
                ("assets/b b.png".into(), "bbb".into()),
            ]
        );
    }

    #[test]
    fn manifest_dispatch_reports_missing_and_removed_paths() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::sync_channel(8);
        *OUTBOUND.lock().unwrap() = Some(tx);
        DESIRED_ASSETS.lock().unwrap().clear();
        APPLIED_ASSETS.lock().unwrap().clear();
        APPLIED_ASSETS
            .lock()
            .unwrap()
            .insert("assets/old.png".into(), "old-hash".into());
        dispatch(
            br#"{"type":"asset_manifest","transfer_id":"t4","asset_revision":4,"entries":[{"path":"assets/new.png","hash":"new-hash"}]}"#,
            &LiveConfig {
                addr: String::new(),
                token: String::new(),
                project: String::new(),
                session: None,
                state_file: None,
            },
        );
        *OUTBOUND.lock().unwrap() = None;
        let payload = rx.recv().unwrap();
        assert!(payload.contains("\"type\":\"assets_reconciled\""));
        assert!(payload.contains("assets/new.png"));
        assert!(payload.contains("assets/old.png"));
    }

    #[test]
    fn explicit_asset_removal_is_queued_for_the_ui_thread() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        discard_pending_asset_batch();
        ASSET_EVENTS.lock().unwrap().clear();
        dispatch(
            br#"{"type":"asset_removed","transfer_id":"t8","path":"assets/old.png","asset_revision":8}"#,
            &LiveConfig {
                addr: String::new(),
                token: String::new(),
                project: String::new(),
                session: None,
                state_file: None,
            },
        );
        let events = take_asset_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, "assets/old.png");
        assert_eq!(events[0].asset_revision, 8);
        assert!(events[0].removed);
        assert!(!events[0].failed);
    }

    #[test]
    fn asset_transaction_holds_events_until_commit() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *OUTBOUND.lock().unwrap() = None;
        PENDING_CONTROL.lock().unwrap().clear();
        discard_pending_asset_batch();
        ASSET_EVENTS.lock().unwrap().clear();
        let config = LiveConfig {
            addr: String::new(),
            token: String::new(),
            project: String::new(),
            session: None,
            state_file: None,
        };

        dispatch(
            br#"{"type":"assets_begin","transfer_id":"t-tx","asset_revision":12,"entries":[{"path":"assets/new.png","hash":"hash"}],"removed":[]}"#,
            &config,
        );
        dispatch(
            br#"{"type":"asset_changed","transfer_id":"t-tx","path":"assets/new.png","asset_revision":12}"#,
            &config,
        );
        assert!(take_asset_events().is_empty());

        dispatch(
            br#"{"type":"assets_commit","transfer_id":"t-tx","asset_revision":12}"#,
            &config,
        );
        let events = take_asset_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, "assets/new.png");
        assert_eq!(events[0].transfer_id, "t-tx");
        PENDING_CONTROL.lock().unwrap().clear();
    }

    #[test]
    fn asset_data_hash_mismatch_is_failed_before_commit() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *OUTBOUND.lock().unwrap() = None;
        PENDING_CONTROL.lock().unwrap().clear();
        discard_pending_asset_batch();
        ASSET_EVENTS.lock().unwrap().clear();
        let config = LiveConfig {
            addr: String::new(),
            token: String::new(),
            project: String::new(),
            session: None,
            state_file: None,
        };

        dispatch(
            br#"{"type":"assets_begin","transfer_id":"t-hash","asset_revision":13,"entries":[{"path":"assets/hash.png","hash":"not-the-sha256"}],"removed":[]}"#,
            &config,
        );
        dispatch(
            br#"{"type":"asset_data","transfer_id":"t-hash","path":"assets/hash.png","asset_revision":13,"data":"YWJj"}"#,
            &config,
        );
        dispatch(
            br#"{"type":"assets_commit","transfer_id":"t-hash","asset_revision":13}"#,
            &config,
        );
        let events = take_asset_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].failed);
        assert_eq!(events[0].error.as_deref(), Some("hash_mismatch"));
        PENDING_CONTROL.lock().unwrap().clear();
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn json_escape_is_round_trip_safe_for_controls() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let escaped = json_escape("line\nbreak \"quoted\" \\slash");
        assert_eq!(escaped, "line\\nbreak \\\"quoted\\\" \\\\slash");
    }

    #[test]
    fn prepare_restart_ships_the_published_snapshot() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::sync_channel(8);
        *OUTBOUND.lock().unwrap() = Some(tx);
        *PUBLISHED_STATE.lock().unwrap() = Some("{\"clicks\":3}".to_string());

        dispatch(
            b"{\"type\":\"prepare_restart\",\"session\":\"s-abc\"}",
            &LiveConfig {
                addr: String::new(),
                token: String::new(),
                project: String::new(),
                session: None,
                state_file: None,
            },
        );

        *OUTBOUND.lock().unwrap() = None;
        let payload = rx.recv().unwrap();
        assert!(payload.contains("\"type\":\"state_saved\""));
        assert!(payload.contains("\"session\":\"s-abc\""));
        assert!(payload.contains("\"data\":\"{\\\"clicks\\\":3}\""));
    }

    #[test]
    fn window_registration_queues_before_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *OUTBOUND.lock().unwrap() = None;
        PENDING_CONTROL.lock().unwrap().clear();
        REGISTERED_WINDOWS.lock().unwrap().clear();
        register_window("w-main", "Counter", 800, 600, 1000, true);
        let pending = PENDING_CONTROL.lock().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].contains("\"window_registered\""));
        assert!(pending[0].contains("\"window_id\":\"w-main\""));
        assert!(window_is_registered("w-main"));
        drop(pending);
        close_window("w-main", Some("test"));
        assert!(!window_is_registered("w-main"));
    }

    #[test]
    fn probe_ui_dispatch_queues_a_ui_thread_request() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        UI_PROBES.lock().unwrap().clear();
        dispatch(
            br#"{"type":"probe_ui","request_id":"probe.1","window_id":"w-main"}"#,
            &LiveConfig {
                addr: String::new(),
                token: String::new(),
                project: String::new(),
                session: None,
                state_file: None,
            },
        );
        let requests = take_ui_probe_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request_id, "probe.1");
        assert_eq!(requests[0].window_id, "w-main");
    }

    #[test]
    fn semantics_query_dispatch_queues_a_bounded_ui_thread_request() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        SEMANTICS_READS.lock().unwrap().clear();
        dispatch(
            br#"{"type":"semantics_query","request_id":"op-1","window_id":"w-main","max_bytes":9999999}"#,
            &LiveConfig {
                addr: String::new(),
                token: String::new(),
                project: String::new(),
                session: None,
                state_file: None,
            },
        );
        let requests = take_semantics_read_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request_id, "op-1");
        assert_eq!(requests[0].window_id, "w-main");
        assert_eq!(requests[0].max_bytes, MAX_SEMANTICS_TREE_BYTES);
    }

    #[test]
    fn preview_reset_dispatch_queues_a_bounded_ui_thread_request() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        PREVIEW_RESETS.lock().unwrap().clear();
        dispatch(
            br#"{"type":"scenario_reset","request_id":"reset-1","scenario_id":"counter-basic"}"#,
            &LiveConfig {
                addr: String::new(),
                token: String::new(),
                project: String::new(),
                session: None,
                state_file: None,
            },
        );
        let requests = take_preview_reset_requests();
        assert_eq!(
            requests,
            vec![PreviewResetRequest {
                request_id: "reset-1".into(),
                scenario_id: "counter-basic".into(),
            }]
        );
    }

    #[test]
    fn preview_reset_result_has_a_bounded_wire_shape() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::sync_channel(8);
        *OUTBOUND.lock().unwrap() = Some(tx);
        respond_scenario_reset("reset-1", "counter-basic", true, 2, None);
        *OUTBOUND.lock().unwrap() = None;
        let payload = rx.recv().unwrap();
        assert!(payload.contains("\"type\":\"scenario_reset_result\""));
        assert!(payload.contains("\"accepted\":true"));
        assert!(payload.contains("\"reset_generation\":2"));
    }

    #[test]
    fn logical_id_enrichment_requires_explicit_declarations_and_rejects_duplicate_matches() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_declared_logical_ids();
        assert!(declare_logical_id("increment", "counter.increment").is_ok());
        let tree = enrich_semantics_tree(
            r#"{"nodes":{"a":{"element_id":"increment","aria":{}}}}"#,
        )
        .unwrap();
        let value: Value = serde_json::from_str(&tree).unwrap();
        assert_eq!(value["nodes"]["a"]["logical_id"], "counter.increment");
        assert!(declare_logical_id("other", "counter.increment").is_err());
        let duplicate = enrich_semantics_tree(
            r#"{"nodes":{"a":{"element_id":"increment"},"b":{"element_id":"increment"}}}"#,
        );
        assert_eq!(duplicate, Err("logical_id_duplicate_in_tree"));
        clear_declared_logical_ids();
    }

    #[test]
    fn asset_ack_contains_revision_and_cache_result() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::sync_channel(8);
        *OUTBOUND.lock().unwrap() = Some(tx);
        report_assets_applied(
            "t7",
            7,
            &["assets/logo.png".into()],
            &["assets/missing.png".into()],
            true,
        );
        *OUTBOUND.lock().unwrap() = None;
        let payload = rx.recv().unwrap();
        assert!(payload.contains("\"type\":\"assets_applied\""));
        assert!(payload.contains("\"asset_revision\":7"));
        assert!(payload.contains("\"cache_invalidated\":true"));
        assert!(payload.contains("assets/missing.png"));
    }

    #[test]
    fn received_ack_contains_transfer_identity_and_paths() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::sync_channel(8);
        *OUTBOUND.lock().unwrap() = Some(tx);
        report_assets_received(
            "t-received",
            7,
            &["assets/logo.png".into()],
            &["assets/broken.png".into()],
        );
        *OUTBOUND.lock().unwrap() = None;
        let payload = rx.recv().unwrap();
        assert!(payload.contains("\"type\":\"assets_received\""));
        assert!(payload.contains("\"transfer_id\":\"t-received\""));
        assert!(payload.contains("assets/broken.png"));
    }

    #[cfg(feature = "gpui-dev")]
    #[test]
    fn required_loaded_ack_reports_declared_status() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::sync_channel(8);
        *OUTBOUND.lock().unwrap() = Some(tx);
        *DESIRED_TRANSFER_ID.lock().unwrap() = Some("t-required".into());
        *DESIRED_ASSET_REVISION.lock().unwrap() = 14;
        REQUIRED_ASSETS.lock().unwrap().clear();
        REQUIRED_ASSETS
            .lock()
            .unwrap()
            .insert("assets/logo.png".into());
        LOADED_REQUIRED_ASSETS.lock().unwrap().clear();
        LOADED_REQUIRED_ASSETS
            .lock()
            .unwrap()
            .insert("assets/logo.png".into());
        FAILED_REQUIRED_ASSETS.lock().unwrap().clear();
        *LAST_REQUIRED_LOADED_REPORT.lock().unwrap() = None;

        assert!(report_required_assets_loaded("main", 3));
        *OUTBOUND.lock().unwrap() = None;
        let payload = rx.recv().unwrap();
        assert!(payload.contains("\"type\":\"assets_required_loaded\""));
        assert!(payload.contains("\"transfer_id\":\"t-required\""));
        assert!(payload.contains("\"window_id\":\"main\""));
        assert!(payload.contains("\"scene_epoch\":3"));
        assert!(payload.contains("assets/logo.png"));
        REQUIRED_ASSETS.lock().unwrap().clear();
        LOADED_REQUIRED_ASSETS.lock().unwrap().clear();
        *LAST_REQUIRED_LOADED_REPORT.lock().unwrap() = None;
    }

    #[cfg(feature = "gpui-dev")]
    #[test]
    fn scene_completed_report_is_deduplicated_and_keeps_presented_frame_optional() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::sync_channel(8);
        *OUTBOUND.lock().unwrap() = Some(tx);
        *LAST_SCENE_COMPLETION_REPORT.lock().unwrap() = None;

        assert!(report_scene_completed("main", 4, 8, 9, Some("frame-4")));
        assert!(!report_scene_completed("main", 4, 8, 9, Some("frame-4")));
        *OUTBOUND.lock().unwrap() = None;
        let payload = rx.recv().unwrap();
        assert!(payload.contains("\"type\":\"scene_completed\""));
        assert!(payload.contains("\"scene_epoch\":4"));
        assert!(payload.contains("\"source_revision\":8"));
        assert!(payload.contains("\"asset_revision\":9"));
        assert!(payload.contains("\"presented_frame_id\":\"frame-4\""));
        *LAST_SCENE_COMPLETION_REPORT.lock().unwrap() = None;
    }

    #[test]
    fn snapshot_number_builder_produces_valid_json() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(snapshot_json_number("clicks", 3), "{\"clicks\":3}");
    }
}

fn b64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let mut n = 0u32;
        let mut count = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                break;
            }
            n |= value(c)? << (18 - 6 * i);
            count += 1;
        }
        out.push((n >> 16) as u8);
        if count > 2 {
            out.push((n >> 8) as u8);
        }
        if count > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}
