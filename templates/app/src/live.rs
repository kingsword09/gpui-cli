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
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

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

/// The latest manifest the supervisor declared for this connection's run.
static DESIRED_ASSETS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
static DESIRED_TRANSFER_ID: Mutex<Option<String>> = Mutex::new(None);
/// Hashes of assets whose UI-thread cache invalidation has completed.
static APPLIED_ASSETS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

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

static UI_PROBES: Mutex<Vec<UiProbeRequest>> = Mutex::new(Vec::new());
static PENDING_CONTROL: Mutex<Vec<String>> = Mutex::new(Vec::new());
static REGISTERED_WINDOWS: Mutex<Vec<String>> = Mutex::new(Vec::new());
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

/// Asset paths reported changed by the CLI since the last call.
pub fn take_asset_events() -> Vec<AssetEvent> {
    std::mem::take(&mut ASSET_EVENTS.lock().unwrap_or_else(|e| e.into_inner()))
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
    remember_window(window_id);
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
                return Ok(Some(std::borrow::Cow::Owned(bytes)));
            }
        }
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
        "{{\"type\":\"hello\",\"proto\":{PROTO_VERSION},\"token\":\"{}\",\"project\":\"{}\",\"pid\":{},\"platform\":\"{platform}\",\"asset_reload\":{},\"runtime_version\":\"{RUNTIME_VERSION}\",\"gpui_version\":\"{GPUI_VERSION}\",\"capabilities\":[\"logs\",\"panic\",\"state\"{}]}}",
        json_escape(&config.token),
        json_escape(&config.project),
        std::process::id(),
        ASSET_SOURCE_INSTALLED.load(std::sync::atomic::Ordering::SeqCst),
        if ASSET_SOURCE_INSTALLED.load(std::sync::atomic::Ordering::SeqCst) {
            ",\"asset_reload\",\"asset_manifest\""
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
        *DESIRED_ASSETS.lock().unwrap_or_else(|e| e.into_inner()) = desired;
        *DESIRED_TRANSFER_ID
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(transfer_id.clone());
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
    } else if find_bytes(frame, b"\"asset_removed\"") {
        if let Some(path) = string_field(frame, "path") {
            let transfer_id = string_field(frame, "transfer_id").unwrap_or_default();
            let asset_revision = number_field(frame, "asset_revision").unwrap_or(0);
            ASSET_EVENTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(AssetEvent {
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
            ASSET_EVENTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(AssetEvent {
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
                let target = std::env::temp_dir()
                    .join("gpui-assets")
                    .join(&path);
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&target, bytes).is_ok() {
                    report_assets_received(&transfer_id, asset_revision, &[path.clone()], &[]);
                    ASSET_EVENTS
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(AssetEvent {
                            path,
                            transfer_id: transfer_id.clone(),
                            asset_revision,
                            removed: false,
                            failed: false,
                            error: None,
                        });
                } else {
                    report_assets_received(&transfer_id, asset_revision, &[], &[path.clone()]);
                    ASSET_EVENTS
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(AssetEvent {
                            path,
                            transfer_id: transfer_id.clone(),
                            asset_revision,
                            removed: false,
                            failed: true,
                            error: Some("write_failed".into()),
                        });
                }
            } else {
                report_assets_received(&transfer_id, asset_revision, &[], &[path.clone()]);
                ASSET_EVENTS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(AssetEvent {
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
}

// ── minimal JSON plumbing ────────────────────────────────────────────────────

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
