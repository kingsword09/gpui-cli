//! Ordered, bounded live events and the state derived from them.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const SCHEMA_VERSION: u32 = 1;
const EVENT_BYTES: usize = 4 * 1024 * 1024;
const EVENT_COUNT: usize = 2048;
const DIAGNOSTIC_BYTES: usize = 256 * 1024;
const ISSUE_BYTES: usize = 128 * 1024;
const PAGE_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Revision {
    pub source_revision: u64,
    pub asset_revision: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(flatten)]
    pub revision: Revision,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    #[serde(rename = "session.started")]
    SessionStarted,
    #[serde(rename = "session.ended")]
    SessionEnded,
    #[serde(rename = "source.changed")]
    SourceChanged,
    #[serde(rename = "build.requested")]
    BuildRequested,
    #[serde(rename = "operation.queued")]
    OperationQueued,
    #[serde(rename = "operation.started")]
    OperationStarted,
    #[serde(rename = "operation.finished")]
    OperationFinished,
    #[serde(rename = "build.started")]
    BuildStarted,
    #[serde(rename = "build.finished")]
    BuildFinished,
    #[serde(rename = "build.superseded")]
    BuildSuperseded,
    #[serde(rename = "stage.started")]
    StageStarted,
    #[serde(rename = "stage.finished")]
    StageFinished,
    #[serde(rename = "diagnostic")]
    Diagnostic,
    #[serde(rename = "app.starting")]
    AppStarting,
    #[serde(rename = "app.started")]
    AppStarted,
    #[serde(rename = "app.launch_failed")]
    AppLaunchFailed,
    #[serde(rename = "app.exited")]
    AppExited,
    #[serde(rename = "app.connected")]
    AppConnected,
    #[serde(rename = "app.disconnected")]
    AppDisconnected,
    #[serde(rename = "app.log")]
    AppLog,
    #[serde(rename = "app.panic")]
    AppPanic,
    #[serde(rename = "output")]
    Output,
    #[serde(rename = "watch.error")]
    WatchError,
    #[serde(rename = "assets.sent")]
    AssetsSent,
    #[serde(rename = "assets.reconciled")]
    AssetsReconciled,
    #[serde(rename = "assets.received")]
    AssetsReceived,
    #[serde(rename = "assets.required_loaded")]
    AssetsRequiredLoaded,
    #[serde(rename = "assets.applied")]
    AssetsApplied,
    #[serde(rename = "window.registered")]
    WindowRegistered,
    #[serde(rename = "window.closed")]
    WindowClosed,
    #[serde(rename = "ui.probe_result")]
    UiProbeResult,
    #[serde(rename = "scene.completed")]
    SceneCompleted,
    #[serde(rename = "artifact.declared")]
    ArtifactDeclared,
    #[serde(rename = "artifact.published")]
    ArtifactPublished,
    #[serde(rename = "artifact.rejected")]
    ArtifactRejected,
    #[serde(rename = "artifact.expired")]
    ArtifactExpired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub session_id: String,
    pub target_id: String,
    pub seq: u64,
    pub received_at_ms: u64,
    #[serde(flatten)]
    pub scope: Scope,
    pub kind: Kind,
    pub data: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildState {
    #[serde(flatten)]
    pub scope: Scope,
    pub status: String,
    pub stage: Option<String>,
    pub error: Option<Value>,
    pub last_output: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunState {
    #[serde(flatten)]
    pub scope: Scope,
    pub pid: Option<u32>,
    pub process: String,
    pub channel: String,
    pub health: String,
    // D1 can confirm delivery only. Rendering and asset ACKs arrive in D2.
    pub assets_confirmed: bool,
    pub exit: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub window_id: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u32,
    pub foreground: bool,
    pub lifecycle: String,
    pub ui: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_probe_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_probe_request_id: Option<String>,
    #[serde(default)]
    pub scene_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_source_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_asset_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presented_frame_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_completed_at_ms: Option<u64>,
    pub registered_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub schema_version: u32,
    pub session_id: String,
    pub project_root: PathBuf,
    pub project: String,
    pub target_id: String,
    pub supervisor_pid: u32,
    pub lifecycle: String,
    pub seq: u64,
    pub desired: Revision,
    pub build: Option<BuildState>,
    pub running: Option<RunState>,
    #[serde(default)]
    pub windows: Vec<WindowSnapshot>,
    pub capabilities: Value,
    pub diagnostics: Vec<Value>,
    pub diagnostics_omitted: u64,
    pub runtime_issues: Vec<Value>,
    pub runtime_issues_omitted: u64,
    pub storage_error: Option<String>,
    pub watcher_error: Option<String>,
}

impl State {
    pub fn stale(&self) -> bool {
        self.running
            .as_ref()
            .is_none_or(|run| run.scope.revision != self.desired || run.process != "running")
    }

    pub fn status_json(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("serializable live state");
        value["stale"] = json!(self.stale());
        let ui = if self
            .windows
            .iter()
            .any(|window| window.ui == "unresponsive")
        {
            "unresponsive"
        } else if self.windows.iter().any(|window| window.ui == "responsive") {
            "responsive"
        } else if self.windows.iter().any(|window| window.ui == "unknown") {
            "unknown"
        } else {
            "unavailable"
        };
        value["ui"] = json!({"status": ui, "window_count": self.windows.len()});
        value
    }

    fn reduce(&mut self, event: &Event) {
        self.seq = event.seq;
        let data = &event.data;
        match event.kind {
            Kind::SessionEnded => self.lifecycle = "ended".into(),
            Kind::SourceChanged => {
                if let Some(run) = &mut self.running
                    && event.scope.revision.asset_revision != self.desired.asset_revision
                {
                    run.assets_confirmed = false;
                }
                self.desired = event.scope.revision.clone();
                self.watcher_error = None;
            }
            Kind::WatchError => self.watcher_error = data["message"].as_str().map(str::to_owned),
            Kind::BuildStarted => {
                self.build = Some(BuildState {
                    scope: event.scope.clone(),
                    status: "building".into(),
                    stage: None,
                    error: None,
                    last_output: json!({}),
                });
                self.diagnostics.clear();
                self.diagnostics_omitted = 0;
            }
            Kind::StageStarted
            | Kind::StageFinished
            | Kind::BuildFinished
            | Kind::BuildSuperseded => {
                if let Some(build) = &mut self.build
                    && build.scope.build_id == event.scope.build_id
                {
                    match event.kind {
                        Kind::StageStarted => {
                            build.stage = data["stage"].as_str().map(str::to_owned)
                        }
                        Kind::StageFinished if data["success"] == false => {
                            build.error = Some(data.clone())
                        }
                        Kind::BuildFinished => {
                            build.status = if data["success"] == true {
                                "succeeded"
                            } else {
                                "failed"
                            }
                            .into();
                            if let Some(error) = data.get("error") {
                                build.error = Some(error.clone());
                            }
                        }
                        Kind::BuildSuperseded => build.status = "superseded".into(),
                        _ => {}
                    }
                }
            }
            Kind::Diagnostic => {
                if self
                    .build
                    .as_ref()
                    .is_some_and(|b| b.scope.build_id == event.scope.build_id)
                {
                    let record =
                        json!({"seq": event.seq, "scope": event.scope, "diagnostic": data});
                    retain_bounded(
                        &mut self.diagnostics,
                        record,
                        DIAGNOSTIC_BYTES,
                        &mut self.diagnostics_omitted,
                    );
                }
            }
            Kind::Output => {
                if event.scope.run_id.is_none()
                    && let Some(build) = &mut self.build
                    && build.scope.build_id == event.scope.build_id
                    && let Some(stream) = data["stream"].as_str()
                {
                    build.last_output[stream] = data.clone();
                }
            }
            Kind::AppStarting => {
                self.windows.clear();
                self.running = Some(RunState {
                    scope: event.scope.clone(),
                    pid: None,
                    process: "starting".into(),
                    channel: "disconnected".into(),
                    health: "unknown".into(),
                    assets_confirmed: false,
                    exit: None,
                });
                for issue in &mut self.runtime_issues {
                    issue["verification"] = json!("pending");
                }
            }
            Kind::WindowRegistered => {
                let Some(window_id) = data["window_id"].as_str() else {
                    return;
                };
                let snapshot = WindowSnapshot {
                    run_id: event.scope.run_id.clone(),
                    window_id: window_id.to_owned(),
                    title: data["title"].as_str().unwrap_or_default().to_owned(),
                    width: data["width"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(0),
                    height: data["height"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(0),
                    scale_milli: data["scale_milli"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(1000),
                    foreground: data["foreground"].as_bool().unwrap_or(false),
                    lifecycle: "open".into(),
                    ui: "unknown".into(),
                    reason: None,
                    last_probe_at_ms: None,
                    last_latency_ms: None,
                    last_probe_request_id: None,
                    scene_epoch: 0,
                    scene_source_revision: None,
                    scene_asset_revision: None,
                    presented_frame_id: None,
                    scene_completed_at_ms: None,
                    registered_at_ms: data["registered_at_ms"]
                        .as_u64()
                        .unwrap_or(event.received_at_ms),
                };
                if let Some(existing) = self.windows.iter_mut().find(|window| {
                    window.run_id == snapshot.run_id && window.window_id == snapshot.window_id
                }) {
                    *existing = snapshot;
                } else {
                    self.windows.push(snapshot);
                }
            }
            Kind::WindowClosed => {
                if let Some(window_id) = data["window_id"].as_str()
                    && let Some(window) = self.windows.iter_mut().find(|window| {
                        window.run_id == event.scope.run_id && window.window_id == window_id
                    })
                {
                    window.lifecycle = "closed".into();
                    window.ui = "unavailable".into();
                    window.reason = data["reason"].as_str().map(str::to_owned);
                }
            }
            Kind::UiProbeResult => {
                if data["accepted"] == false {
                    return;
                }
                let Some(window_id) = data["window_id"].as_str() else {
                    return;
                };
                if let Some(window) = self.windows.iter_mut().find(|window| {
                    window.run_id == event.scope.run_id && window.window_id == window_id
                }) {
                    window.ui = if data["responsive"].as_bool().unwrap_or(false) {
                        "responsive"
                    } else {
                        "unresponsive"
                    }
                    .into();
                    window.reason = data["reason"].as_str().map(str::to_owned);
                    window.last_probe_at_ms = Some(
                        data["received_at_ms"]
                            .as_u64()
                            .unwrap_or(event.received_at_ms),
                    );
                    window.last_latency_ms = data["latency_ms"].as_u64();
                    window.last_probe_request_id = data["request_id"].as_str().map(str::to_owned);
                }
            }
            Kind::SceneCompleted => {
                if data["accepted"] == false {
                    return;
                }
                let Some(window_id) = data["window_id"].as_str() else {
                    return;
                };
                if let Some(window) = self.windows.iter_mut().find(|window| {
                    window.run_id == event.scope.run_id && window.window_id == window_id
                }) {
                    window.scene_epoch = data["scene_epoch"].as_u64().unwrap_or(0);
                    window.scene_source_revision = data["source_revision"].as_u64();
                    window.scene_asset_revision = data["asset_revision"].as_u64();
                    window.presented_frame_id =
                        data["presented_frame_id"].as_str().map(str::to_owned);
                    window.scene_completed_at_ms = data["received_at_ms"]
                        .as_u64()
                        .or(Some(event.received_at_ms));
                }
            }
            Kind::AssetsApplied => {
                if let Some(run) = &mut self.running
                    && event.scope.run_id == run.scope.run_id
                {
                    let revision = data["asset_revision"].as_u64();
                    let failed = data["failed"]
                        .as_array()
                        .is_some_and(|paths| !paths.is_empty());
                    run.assets_confirmed = data["accepted"] != false
                        && revision == Some(self.desired.asset_revision)
                        && data["cache_invalidated"] == true
                        && !failed;
                }
            }
            Kind::AppStarted
            | Kind::AppLaunchFailed
            | Kind::AppExited
            | Kind::AppConnected
            | Kind::AppDisconnected => {
                if let Some(run) = &mut self.running
                    && event.scope.run_id.is_some()
                    && run.scope.run_id == event.scope.run_id
                {
                    match event.kind {
                        Kind::AppStarted => {
                            if run.process == "starting" {
                                run.process = if data["confirmed"] == false {
                                    "launched"
                                } else {
                                    "running"
                                }
                                .into();
                            }
                            if let Some(pid) = data["pid"].as_u64() {
                                run.pid = u32::try_from(pid).ok();
                            }
                        }
                        Kind::AppLaunchFailed => {
                            run.process = "launch_failed".into();
                            run.exit = Some(data.clone());
                        }
                        Kind::AppExited => {
                            run.process = "exited".into();
                            run.exit = Some(data.clone());
                        }
                        Kind::AppConnected => {
                            run.channel = "connected".into();
                            if matches!(run.process.as_str(), "starting" | "launched") {
                                run.process = "running".into();
                            }
                            if let Some(pid) = data["pid"].as_u64() {
                                run.pid = u32::try_from(pid).ok();
                            }
                        }
                        Kind::AppDisconnected => run.channel = "disconnected".into(),
                        _ => {}
                    }
                }
                if event.kind == Kind::AppExited
                    && data["success"] == false
                    && data["expected"] == false
                {
                    retain_bounded(
                        &mut self.runtime_issues,
                        json!({"seq": event.seq, "scope": event.scope, "kind": event.kind, "detail": data,
                            "verification": "unverified"}),
                        ISSUE_BYTES,
                        &mut self.runtime_issues_omitted,
                    );
                }
                if event.kind == Kind::AppDisconnected {
                    for window in self.windows.iter_mut().filter(|window| {
                        window.run_id == event.scope.run_id && window.lifecycle == "open"
                    }) {
                        window.ui = "unknown".into();
                        window.reason = Some("app_channel_disconnected".into());
                    }
                }
                if event.kind == Kind::AppExited {
                    for window in self.windows.iter_mut().filter(|window| {
                        window.run_id == event.scope.run_id && window.lifecycle == "open"
                    }) {
                        window.ui = "unavailable".into();
                        window.reason = Some("app_exited".into());
                    }
                }
            }
            Kind::AppPanic | Kind::AppLog => {
                if event.kind == Kind::AppPanic || data["level"] == "error" {
                    let issue = json!({"seq": event.seq, "scope": event.scope, "kind": event.kind,
                        "detail": data, "verification": "unverified"});
                    retain_bounded(
                        &mut self.runtime_issues,
                        issue,
                        ISSUE_BYTES,
                        &mut self.runtime_issues_omitted,
                    );
                }
                if event.kind == Kind::AppPanic
                    && let Some(run) = &mut self.running
                    && event.scope.run_id.is_some()
                    && run.scope.run_id == event.scope.run_id
                {
                    run.health = "unhealthy".into();
                }
            }
            _ => {}
        }
    }
}

fn retain_bounded(records: &mut Vec<Value>, record: Value, limit: usize, omitted: &mut u64) {
    let len = serde_json::to_vec(&record).map_or(limit + 1, |b| b.len());
    if len > limit {
        *omitted += 1;
        return;
    }
    records.push(record);
    while records.len() > 128 || serde_json::to_vec(records).is_ok_and(|b| b.len() > limit) {
        records.remove(0);
        *omitted += 1;
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub events: Vec<Event>,
    pub next_seq: u64,
    pub last_seq: u64,
    pub earliest_seq: u64,
    pub gap: bool,
    pub has_more: bool,
    pub ended: bool,
}

struct Inner {
    state: State,
    events: VecDeque<(Event, usize)>,
    bytes: usize,
    journal: RollingFile,
    checkpoint: Instant,
}

pub struct EventStore {
    inner: Mutex<Inner>,
    changed: Condvar,
    dir: PathBuf,
}

impl EventStore {
    pub fn new(dir: &Path, state: State) -> Result<Self> {
        let journal = RollingFile::new(dir, "events", 1024 * 1024, 8)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                state,
                events: VecDeque::new(),
                bytes: 0,
                journal,
                checkpoint: Instant::now(),
            }),
            changed: Condvar::new(),
            dir: dir.to_owned(),
        })
    }

    pub fn emit(&self, kind: Kind, scope: &Scope, data: Value) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.state.lifecycle == "ended" {
            return;
        }
        // Every retained event must fit one response page. Full compiler and
        // app payloads already have references into the raw output journal.
        let data_bytes = serde_json::to_vec(&data)
            .expect("serializable event data")
            .len();
        let data = if data_bytes > 192 * 1024 {
            json!({"payload_truncated": true, "original_bytes": data_bytes, "log": data.get("log"),
                "message": clip(data["message"].as_str().unwrap_or("Payload exceeds the event limit; inspect the raw log"), 4096),
                "level": data.get("level"), "code": data.get("code"), "success": data.get("success"),
                "stage": data.get("stage")})
        } else {
            data
        };
        let event = Event {
            schema_version: SCHEMA_VERSION,
            session_id: inner.state.session_id.clone(),
            target_id: inner.state.target_id.clone(),
            seq: inner.state.seq + 1,
            received_at_ms: now_ms(),
            scope: scope.clone(),
            kind,
            data,
        };
        let bytes = serde_json::to_vec(&event).expect("serializable live event");
        if let Err(error) = inner.journal.append(&bytes) {
            inner.state.storage_error = Some(error.to_string());
        }
        inner.state.reduce(&event);
        inner.bytes += bytes.len();
        inner.events.push_back((event, bytes.len()));
        while inner.events.len() > EVENT_COUNT || inner.bytes > EVENT_BYTES {
            if let Some((_, len)) = inner.events.pop_front() {
                inner.bytes -= len;
            }
        }
        if !matches!(kind, Kind::Output | Kind::AppLog)
            || inner.checkpoint.elapsed() >= Duration::from_millis(250)
        {
            if let Err(error) = atomic_json(&self.dir.join("state.json"), &inner.state) {
                inner.state.storage_error = Some(error.to_string());
            }
            inner.checkpoint = Instant::now();
        }
        self.changed.notify_all();
    }

    pub fn state(&self) -> State {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .clone()
    }

    pub fn storage_error(&self, error: String) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .storage_error = Some(error);
    }

    pub fn events(&self, after: u64, timeout: Duration) -> Page {
        let deadline = Instant::now() + timeout;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        while inner.state.seq <= after && inner.state.lifecycle != "ended" {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            inner = self
                .changed
                .wait_timeout(inner, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let earliest_seq = inner
            .events
            .front()
            .map_or(inner.state.seq + 1, |(e, _)| e.seq);
        let mut bytes = 0;
        let events: Vec<Event> = inner
            .events
            .iter()
            .filter(|(e, _)| e.seq > after)
            .take(128)
            .take_while(|(_, len)| {
                bytes += len;
                bytes <= PAGE_BYTES
            })
            .map(|(e, _)| e.clone())
            .collect();
        let next_seq = events.last().map_or(after, |e| e.seq);
        Page {
            events,
            next_seq,
            last_seq: inner.state.seq,
            earliest_seq,
            gap: after.saturating_add(1) < earliest_seq,
            has_more: next_seq < inner.state.seq,
            ended: inner.state.lifecycle == "ended",
        }
    }
}

/// One bounded journal shared by all producers; references include offsets so
/// callers can recover full output without sending it through every query.
pub struct RollingFile {
    dir: PathBuf,
    prefix: String,
    file: File,
    index: u64,
    bytes: u64,
    segment_bytes: u64,
    segments: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogRef {
    pub path: PathBuf,
    pub offset: u64,
    pub bytes: usize,
}

impl RollingFile {
    pub fn new(dir: &Path, prefix: &str, segment_bytes: u64, segments: u64) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let file = File::create(dir.join(format!("{prefix}-000000.ndjson")))?;
        Ok(Self {
            dir: dir.into(),
            prefix: prefix.into(),
            file,
            index: 0,
            bytes: 0,
            segment_bytes,
            segments,
        })
    }

    fn path(&self) -> PathBuf {
        self.dir
            .join(format!("{}-{:06}.ndjson", self.prefix, self.index))
    }

    pub fn append(&mut self, bytes: &[u8]) -> Result<LogRef> {
        anyhow::ensure!(
            (bytes.len() as u64) < self.segment_bytes,
            "record exceeds the {} byte journal segment limit",
            self.segment_bytes
        );
        if self.bytes > 0 && self.bytes + bytes.len() as u64 + 1 > self.segment_bytes {
            self.index += 1;
            self.file = File::create(self.path())?;
            self.bytes = 0;
            if self.index >= self.segments {
                let old = self.dir.join(format!(
                    "{}-{:06}.ndjson",
                    self.prefix,
                    self.index - self.segments
                ));
                match fs::remove_file(old) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        let reference = LogRef {
            path: self.path(),
            offset: self.bytes,
            bytes: bytes.len() + 1,
        };
        self.file.write_all(bytes)?;
        self.file.write_all(b"\n")?;
        self.bytes += bytes.len() as u64 + 1;
        Ok(reference)
    }
}

/// Every event this session journaled to disk, in seq order.
///
/// A follower reads these when the supervisor is already gone: the closing
/// events are on disk even though the control socket is closed. The journal
/// rotates, so the earliest events may already be missing.
pub fn archived(dir: &Path) -> Result<Vec<Event>> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("events-") && name.ends_with(".ndjson") {
            segments.push(entry.path());
        }
    }
    // Segment names are zero-padded, so lexicographic order is seq order.
    segments.sort();
    let mut events = Vec::new();
    for segment in segments {
        let text = match fs::read_to_string(&segment) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            // A record still being written when the process died is not fatal.
            Err(_) => continue,
        };
        events.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok()),
        );
    }
    Ok(events)
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("state directory"))?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.persist(path)?;
    Ok(())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
