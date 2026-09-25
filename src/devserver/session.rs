//! One live session, shared by build, process, app-channel and control workers.

use super::artifacts::{ArtifactInfo, ArtifactLimits, ArtifactStore};
use super::capture;
use super::events::{self, EventStore, Kind, LogRef, Revision, RollingFile, Scope, State};
use super::inputs::{AssetDelta, Inputs};
use super::operations::{
    MAX_ACTIVE_OPERATIONS, OperationError, OperationSnapshot, OperationState, OperationStore,
    SubmitResult, Transition,
};
use super::protocol::AssetManifestEntry;
use super::timing::{SpanGuard, Timing};
use super::windows::{MAX_SEMANTICS_TREE_BYTES, SemanticsReadReply, WindowRegistry};
use anyhow::{Context, Result};
use gpui_dev_protocol::{ARTIFACT_CHUNK_BYTES, ArtifactKind, ArtifactManifest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Session {
    pub id: String,
    pub root: PathBuf,
    pub dir: PathBuf,
    pub store: EventStore,
    pub stopping: AtomicBool,
    inputs: Mutex<Inputs>,
    output: Mutex<RollingFile>,
    pub timing: Arc<Timing>,
    pub windows: Arc<WindowRegistry>,
    pub artifacts: Arc<ArtifactStore>,
    pub operations: Arc<OperationStore>,
    build_requests: Mutex<Option<BuildRequest>>,
    observe_requests: Mutex<VecDeque<ObserveRequest>>,
    semantics_results: Mutex<HashMap<String, SemanticsReadReply>>,
    next_build: AtomicU64,
    next_run: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct BuildRequest {
    pub request_id: String,
}

#[derive(Clone, Debug)]
pub struct ObserveRequest {
    pub operation_id: String,
    pub sync: bool,
    pub window_id: Option<String>,
    pub require: Vec<String>,
    pub target_revision: Revision,
    pub input_hash: String,
    pub build_requested: bool,
    pub semantics_requested: bool,
}

struct WindowCaptureTarget<'a> {
    run: &'a super::events::RunState,
    window: &'a super::events::WindowSnapshot,
    pid: Option<u32>,
    provider: Option<&'a str>,
    timeout: Duration,
}

struct ArtifactPublication<'a> {
    operation: &'a OperationSnapshot,
    event_scope: &'a Scope,
    run_id: Option<String>,
    identity: &'a str,
    suffix: &'a str,
    bytes: &'a [u8],
    kind: ArtifactKind,
    mime: &'a str,
    provider: Option<&'a str>,
    scope: &'a str,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct BuildRequestResult {
    pub accepted: bool,
    pub coalesced: bool,
    pub queue_depth: usize,
}

pub struct Build {
    pub session: Arc<Session>,
    pub scope: Scope,
    span: SpanGuard,
}

impl Session {
    pub fn start(root: &Path, project: &str, target: &str) -> Result<Arc<Self>> {
        let root = root.canonicalize().context("resolving live project root")?;
        let id = format!("live-{}", random_token()?);
        let dir = root.join(".gpui/live").join(&id);
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        let state = State {
            schema_version: events::SCHEMA_VERSION,
            session_id: id.clone(),
            project_root: root.clone(),
            project: project.into(),
            target_id: target.into(),
            supervisor_pid: std::process::id(),
            lifecycle: "running".into(),
            seq: 0,
            desired: Revision::default(),
            build: None,
            running: None,
            windows: Vec::new(),
            capabilities: json!({"status": true, "diagnostics": true, "events": true,
            "run_identity": "launch_token", "ui_observation": true, "asset_confirmation": true,
                "actions": false, "checks": false, "input_scope": "project_files",
                "native_mobile_logs": false, "timing_spans": true,
                "capture.scene": {"available": false, "reason": "backend_unsupported",
                    "provider": null, "constraints": {}},
                "capture.window": {"available": capture::window_capture_available(),
                    "reason": if capture::window_capture_available() { Value::Null } else { json!("backend_unsupported") },
                    "provider": if capture::window_capture_available() { json!("macos_screencapture") } else { Value::Null },
                    "constraints": {"scope": "window", "consistency": "best_effort"}},
                "capture.device": {"available": false, "reason": "backend_unsupported",
                    "provider": null, "constraints": {}},
                "semantics.read": {"available": false, "reason": "runtime_query_required",
                    "provider": "gpui-debug-a11y", "constraints": {
                        "max_bytes": MAX_SEMANTICS_TREE_BYTES,
                        "max_nodes": super::artifacts::MAX_TREE_NODES}},
                "artifact_store": {"available": true, "provider": "session_file_store",
                    "constraints": {"chunk_bytes": gpui_dev_protocol::ARTIFACT_CHUNK_BYTES,
                        "session_bytes": ArtifactLimits::default().session_quota_bytes,
                        "project_bytes": ArtifactLimits::default().project_quota_bytes}}}),
            diagnostics: Vec::new(),
            diagnostics_omitted: 0,
            runtime_issues: Vec::new(),
            runtime_issues_omitted: 0,
            storage_error: None,
            watcher_error: None,
        };
        let timing = Arc::new(Timing::new(&dir, &id)?);
        let artifacts = Arc::new(ArtifactStore::new(&root, &id, ArtifactLimits::default())?);
        let session = Arc::new(Self {
            id,
            root,
            store: EventStore::new(&dir, state)?,
            output: Mutex::new(RollingFile::new(
                &dir.join("logs"),
                "output",
                4 * 1024 * 1024,
                8,
            )?),
            timing,
            windows: Arc::new(WindowRegistry::default()),
            artifacts,
            operations: Arc::new(OperationStore::new(MAX_ACTIVE_OPERATIONS)),
            build_requests: Mutex::new(None),
            observe_requests: Mutex::new(VecDeque::new()),
            semantics_results: Mutex::new(HashMap::new()),
            dir,
            stopping: AtomicBool::new(false),
            inputs: Mutex::new(Inputs::default()),
            next_build: AtomicU64::new(1),
            next_run: AtomicU64::new(1),
        });
        for stage in ["device.lease_wait", "ui.ready", "observation.capture"] {
            session.timing.not_instrumented(
                stage,
                &Scope::default(),
                json!({"reason": "not implemented in the D1 live runtime"}),
            );
        }
        session.emit(Kind::SessionStarted, &Scope::default(), json!({}));
        session.sync_inputs()?;
        Ok(session)
    }

    pub fn submit_operation(
        &self,
        request_id: &str,
        kind: &str,
        scope: Scope,
        target: Value,
        deadline_at_ms: u64,
    ) -> Result<SubmitResult, OperationError> {
        let result = self.operations.submit(
            request_id,
            kind,
            scope,
            target,
            deadline_at_ms,
            events::now_ms(),
        )?;
        if let SubmitResult::Created(snapshot) = &result {
            self.emit_operation(Kind::OperationQueued, snapshot);
        }
        Ok(result)
    }

    pub fn submit_observe(
        &self,
        request_id: &str,
        sync: bool,
        window_id: Option<String>,
        require: Vec<String>,
        deadline_ms: u64,
    ) -> Result<SubmitResult, OperationError> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err(OperationError::new(
                "session_ended",
                "the live session is stopping",
            ));
        }
        let require = normalize_requirements(&require)?;
        let (target_revision, _, input_hash) = self
            .sync_inputs_with_delta_and_hash()
            .map_err(|error| OperationError::new("input_scan_failed", error.to_string()))?;
        let state = self.store.state();
        let run = state.running.as_ref();
        let run_id = run.and_then(|run| run.scope.run_id.clone());
        let windows = self.windows.snapshots(run_id.as_deref());
        if window_id.is_none() && windows.len() > 1 {
            return Err(OperationError::with_details(
                "ambiguous_window",
                "multiple live windows require an explicit window_id",
                json!({"windows": windows.iter().map(|window| &window.window_id).collect::<Vec<_>>() }),
            ));
        }
        if let Some(window_id) = &window_id
            && !windows.is_empty()
            && !windows.iter().any(|window| &window.window_id == window_id)
        {
            return Err(OperationError::with_details(
                "unknown_window",
                "the requested window is not registered in the current run",
                json!({"window_id": window_id}),
            ));
        }
        let now_ms = events::now_ms();
        let deadline_at_ms = now_ms.checked_add(deadline_ms).ok_or_else(|| {
            OperationError::new("invalid_deadline", "operation deadline overflowed")
        })?;
        let target = json!({
            "sync": sync,
            "window_id": window_id,
            "require": require,
            "target_revision": target_revision,
            "input_hash": input_hash,
            "input_consistency": "tracked_scan",
        });
        let scope = Scope {
            build_id: run.and_then(|run| run.scope.build_id.clone()),
            run_id,
            revision: target_revision.clone(),
        };
        let result = self.submit_operation(request_id, "observe", scope, target, deadline_at_ms)?;
        if let SubmitResult::Created(snapshot) = &result {
            self.observe_requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(ObserveRequest {
                    operation_id: snapshot.operation_id.clone(),
                    sync,
                    window_id,
                    require,
                    target_revision,
                    input_hash,
                    build_requested: false,
                    semantics_requested: false,
                });
        }
        Ok(result)
    }

    pub fn take_observe_request(&self) -> Option<ObserveRequest> {
        self.observe_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// Stores only bounded, identity-checked replies from the app channel.
    /// The live coordinator consumes them on its next non-blocking tick.
    pub fn accept_semantics_result(&self, reply: SemanticsReadReply) {
        let mut results = self
            .semantics_results
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if results.len() >= 64 {
            results.clear();
        }
        results.insert(reply.request_id.clone(), reply);
    }

    fn take_semantics_result(&self, request_id: &str) -> Option<SemanticsReadReply> {
        self.semantics_results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id)
    }

    /// Advances observe work on the single live coordinator. Pending requests
    /// stay queued while a requested build or app window becomes ready.
    pub fn advance_observe_requests(&self) {
        self.expire_operations();
        let pending_count = self
            .observe_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        for _ in 0..pending_count {
            let Some(mut request) = self.take_observe_request() else {
                break;
            };
            let operation = match self.operations.get(&request.operation_id, events::now_ms()) {
                Ok(operation) if operation.state.is_terminal() => continue,
                Ok(operation) if operation.state == OperationState::Queued => {
                    match self.start_operation(&request.operation_id) {
                        Ok(transition) => transition.snapshot,
                        Err(_) => continue,
                    }
                }
                Ok(operation) => operation,
                Err(_) => continue,
            };
            if operation.state != OperationState::Running {
                continue;
            }
            if let Some(unavailable) = self.unavailable_observe_requirements(&request.require) {
                self.fail_observe(
                    &operation,
                    "unavailable",
                    "a required observation capability has no available provider",
                    json!({"unavailable": unavailable}),
                );
                continue;
            }
            let state = self.store.state();
            if request.sync {
                let (current_revision, _, current_input_hash) =
                    match self.sync_inputs_with_delta_and_hash() {
                        Ok(value) => value,
                        Err(error) => {
                            self.fail_observe(
                                &operation,
                                "input_scan_failed",
                                "could not verify the requested input snapshot",
                                json!({"message": error.to_string()}),
                            );
                            continue;
                        }
                    };
                if current_revision != request.target_revision
                    || current_input_hash != request.input_hash
                {
                    let _ = self.finish_operation(
                        &request.operation_id,
                        OperationState::Superseded,
                        None,
                        Some(OperationError::with_details(
                            "superseded",
                            "project inputs changed after the observe target was recorded",
                            json!({"target_revision": request.target_revision,
                                "current_revision": current_revision,
                                "target_input_hash": request.input_hash,
                                "current_input_hash": current_input_hash}),
                        )),
                    );
                    continue;
                }
                let current_run = state.running.as_ref();
                let run_matches = current_run.is_some_and(|run| {
                    run.scope.revision == request.target_revision && run.process == "running"
                });
                if !run_matches {
                    if current_run.is_some_and(|run| {
                        run.scope.revision == request.target_revision
                            && matches!(run.process.as_str(), "exited" | "launch_failed")
                    }) && state.build.as_ref().is_some_and(|build| {
                        build.scope.revision == request.target_revision
                            && build.status == "succeeded"
                    }) {
                        let run = current_run.expect("run was matched by the condition");
                        self.fail_observe(
                            &operation,
                            "launch_failed",
                            "the build succeeded but its app did not remain available",
                            json!({"run_id": run.scope.run_id, "process": run.process}),
                        );
                        continue;
                    }
                    if state.build.as_ref().is_some_and(|build| {
                        build.scope.revision == request.target_revision && build.status == "failed"
                    }) {
                        self.fail_observe(
                            &operation,
                            "build_failed",
                            "the build for the observe target revision failed",
                            json!({"build": state.build}),
                        );
                        continue;
                    }
                    let build_in_progress = state.build.as_ref().is_some_and(|build| {
                        build.scope.revision == request.target_revision
                            && build.status == "building"
                    });
                    let target_run_starting = current_run.is_some_and(|run| {
                        run.scope.revision == request.target_revision
                            && matches!(run.process.as_str(), "starting" | "launched")
                    });
                    if !request.build_requested && !build_in_progress && !target_run_starting {
                        self.request_build(&request.operation_id);
                        request.build_requested = true;
                    }
                    self.requeue_observe(request);
                    continue;
                }
                if !current_run.is_some_and(|run| {
                    run.assets_confirmed
                        && run.scope.revision.asset_revision
                            == request.target_revision.asset_revision
                }) {
                    self.requeue_observe(request);
                    continue;
                }
            }
            let Some(run) = state.running.as_ref() else {
                self.fail_observe(
                    &operation,
                    "target_unavailable",
                    "there is no running app to observe",
                    json!({}),
                );
                continue;
            };
            let mut observed_scope = operation.scope.clone();
            observed_scope.build_id = run.scope.build_id.clone();
            observed_scope.run_id = run.scope.run_id.clone();
            observed_scope.revision = run.scope.revision.clone();
            let operation = match self.bind_operation_scope(&request.operation_id, observed_scope) {
                Ok(transition) => transition.snapshot,
                Err(_) => continue,
            };
            if run.process != "running" || run.channel != "connected" {
                if run.process == "exited" || run.process == "launch_failed" {
                    self.fail_observe(
                        &operation,
                        "target_exited",
                        "the selected app run is no longer available",
                        json!({"run_id": run.scope.run_id, "process": run.process,
                            "channel": run.channel}),
                    );
                } else {
                    self.requeue_observe(request);
                }
                continue;
            }
            let windows = self.windows.snapshots(run.scope.run_id.as_deref());
            if request.window_id.is_none() && windows.len() > 1 {
                self.fail_observe(
                    &operation,
                    "ambiguous_window",
                    "multiple live windows require an explicit window_id",
                    json!({"windows": windows.iter().map(|window| &window.window_id).collect::<Vec<_>>() }),
                );
                continue;
            }
            if let Some(window_id) = &request.window_id
                && !windows.is_empty()
                && !windows.iter().any(|window| &window.window_id == window_id)
            {
                self.fail_observe(
                    &operation,
                    "unknown_window",
                    "the requested window is not registered in the current run",
                    json!({"window_id": window_id, "run_id": run.scope.run_id}),
                );
                continue;
            }
            let selected = request
                .window_id
                .as_deref()
                .and_then(|id| windows.iter().find(|window| window.window_id == id))
                .or_else(|| (windows.len() == 1).then(|| &windows[0]));
            let Some(window) = selected else {
                self.requeue_observe(request);
                continue;
            };
            if window.lifecycle != "open" {
                self.fail_observe(
                    &operation,
                    "window_closed",
                    "the selected window is closed",
                    json!({"window_id": window.window_id}),
                );
                continue;
            }
            if window.ui == "unresponsive" {
                self.fail_observe(
                    &operation,
                    "ui_unresponsive",
                    "the selected UI did not answer its heartbeat",
                    json!({"window_id": window.window_id, "reason": window.reason}),
                );
                continue;
            }
            if window.ui != "responsive" {
                self.requeue_observe(request);
                continue;
            }
            let provider = self.selected_window_provider(&request.require);
            let needs_screenshot = request.require.iter().any(|requirement| {
                matches!(
                    requirement.as_str(),
                    "screenshot" | "capture.scene" | "capture.window" | "capture.device"
                )
            });
            if needs_screenshot && provider.is_none() {
                self.fail_observe(
                    &operation,
                    "unavailable",
                    "no screenshot provider is available for this request",
                    json!({"required": request.require}),
                );
                continue;
            }
            let needs_semantics = request
                .require
                .iter()
                .any(|requirement| matches!(requirement.as_str(), "semantics" | "semantics.read"));
            let semantics = if needs_semantics {
                if !request.semantics_requested {
                    match self.windows.request_semantics(
                        &operation.scope,
                        &window.window_id,
                        request.operation_id.clone(),
                        MAX_SEMANTICS_TREE_BYTES,
                    ) {
                        Ok(_) => {
                            request.semantics_requested = true;
                            self.requeue_observe(request);
                            continue;
                        }
                        Err("semantics_busy") => {
                            self.requeue_observe(request);
                            continue;
                        }
                        Err(reason) => {
                            self.fail_observe(
                                &operation,
                                if reason == "window_closed" {
                                    "window_closed"
                                } else {
                                    "unavailable"
                                },
                                "the selected window cannot answer a semantics read",
                                json!({"window_id": window.window_id, "reason": reason}),
                            );
                            continue;
                        }
                    }
                }
                let Some(result) = self.take_semantics_result(&request.operation_id) else {
                    self.requeue_observe(request);
                    continue;
                };
                Some(result)
            } else {
                None
            };
            let pid = if provider.is_some() {
                let Some(pid) = run.pid else {
                    self.requeue_observe(request);
                    continue;
                };
                Some(pid)
            } else {
                None
            };
            let remaining =
                Duration::from_millis(operation.deadline_at_ms.saturating_sub(events::now_ms()));
            if remaining.is_zero() {
                self.expire_operations();
                continue;
            }
            match self.capture_observation(
                &operation,
                &request,
                WindowCaptureTarget {
                    run,
                    window,
                    pid,
                    provider,
                    timeout: remaining,
                },
                semantics,
            ) {
                Ok(result) => {
                    let _ = self.finish_operation(
                        &request.operation_id,
                        OperationState::Succeeded,
                        Some(result),
                        None,
                    );
                }
                Err(error) => {
                    let _ = self.finish_operation(
                        &request.operation_id,
                        OperationState::Failed,
                        None,
                        Some(error),
                    );
                }
            }
        }
    }

    fn unavailable_observe_requirements(&self, requirements: &[String]) -> Option<Vec<Value>> {
        let state = self.store.state();
        let semantics_available = state.capabilities["semantics.read"]["available"] == true
            && state
                .running
                .as_ref()
                .is_some_and(|run| run.channel == "connected");
        let missing = requirements
            .iter()
            .filter_map(|requirement| match requirement.as_str() {
                "screenshot" if capture::window_capture_available() => None,
                "capture.window" if capture::window_capture_available() => None,
                "screenshot" => Some(json!({"requirement": requirement,
                    "alternatives": ["capture.scene", "capture.window", "capture.device"],
                    "reason": "backend_unsupported"})),
                "semantics" | "semantics.read" if semantics_available => None,
                "semantics" | "semantics.read" => Some(json!({"requirement": requirement,
                    "alternatives": ["semantics.read"], "reason": "runtime_unavailable"})),
                capability => Some(json!({"requirement": capability,
                    "alternatives": [capability], "reason": "backend_unsupported"})),
            })
            .collect::<Vec<_>>();
        (!missing.is_empty()).then_some(missing)
    }

    fn selected_window_provider(&self, requirements: &[String]) -> Option<&'static str> {
        let explicit_window = requirements
            .iter()
            .any(|requirement| requirement == "capture.window");
        let screenshot = requirements
            .iter()
            .any(|requirement| requirement == "screenshot");
        if capture::window_capture_available() && (explicit_window || screenshot) {
            Some("macos_screencapture")
        } else {
            None
        }
    }

    fn requeue_observe(&self, request: ObserveRequest) {
        self.observe_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(request);
    }

    fn fail_observe(
        &self,
        operation: &OperationSnapshot,
        code: &str,
        message: &str,
        details: Value,
    ) {
        let _ = self.finish_operation(
            &operation.operation_id,
            OperationState::Failed,
            None,
            Some(OperationError::with_details(code, message, details)),
        );
    }

    fn publish_observation_artifact(
        &self,
        publication: ArtifactPublication<'_>,
    ) -> Result<ArtifactInfo, OperationError> {
        let ArtifactPublication {
            operation,
            event_scope,
            run_id,
            identity,
            suffix,
            bytes,
            kind,
            mime,
            provider,
            scope,
        } = publication;
        let digest = Sha256::digest(bytes);
        let digest_hex = format!("{digest:x}");
        let artifact_id = format!("obs-{identity}-{suffix}");
        let transfer_id = format!("xfer-{identity}-{suffix}");
        let kind_name = match kind {
            ArtifactKind::Png => "png",
            ArtifactKind::Tree => "tree",
            ArtifactKind::Blob => "blob",
        };
        let manifest = ArtifactManifest {
            artifact_id: artifact_id.clone(),
            transfer_id: transfer_id.clone(),
            run_id,
            kind,
            mime: mime.into(),
            declared_bytes: bytes.len() as u64,
            sha256: digest_hex.clone(),
        };
        let artifact = self.artifacts.begin(manifest).map_err(|error| {
            self.emit(
                Kind::ArtifactRejected,
                event_scope,
                json!({"operation_id": operation.operation_id, "artifact_id": artifact_id,
                    "code": error.code.as_str(), "message": error.message}),
            );
            OperationError::with_details(
                error.code.as_str(),
                error.message,
                json!({"artifact_id": artifact_id}),
            )
        })?;
        self.emit(
            Kind::ArtifactDeclared,
            event_scope,
            json!({"operation_id": operation.operation_id, "artifact_id": artifact_id,
                "kind": kind_name, "declared_bytes": artifact.manifest.declared_bytes,
                "sha256": digest_hex, "provider": provider, "scope": scope}),
        );
        for (index, chunk) in bytes.chunks(ARTIFACT_CHUNK_BYTES).enumerate() {
            if !self.operation_is_running(&operation.operation_id) {
                let _ = self.artifacts.abort(&artifact_id, &transfer_id);
                return Err(OperationError::with_details(
                    "cancelled",
                    "observe was cancelled while an observation artifact was transferring",
                    json!({"artifact_id": artifact_id}),
                ));
            }
            let offset = (index * ARTIFACT_CHUNK_BYTES) as u64;
            if let Err(error) =
                self.artifacts
                    .write_chunk(&artifact_id, &transfer_id, offset, chunk)
            {
                let _ = self.artifacts.abort(&artifact_id, &transfer_id);
                self.emit(
                    Kind::ArtifactRejected,
                    event_scope,
                    json!({"operation_id": operation.operation_id, "artifact_id": artifact_id,
                        "code": error.code.as_str(), "message": error.message}),
                );
                return Err(OperationError::with_details(
                    error.code.as_str(),
                    error.message,
                    json!({"artifact_id": artifact_id}),
                ));
            }
        }
        if !self.operation_is_running(&operation.operation_id) {
            let _ = self.artifacts.abort(&artifact_id, &transfer_id);
            return Err(OperationError::with_details(
                "cancelled",
                "observe was cancelled before an observation artifact was published",
                json!({"artifact_id": artifact_id}),
            ));
        }
        let artifact = self
            .artifacts
            .finish(&artifact_id, &transfer_id)
            .map_err(|error| {
                self.emit(
                    Kind::ArtifactRejected,
                    event_scope,
                    json!({"operation_id": operation.operation_id, "artifact_id": artifact_id,
                        "code": error.code.as_str(), "message": error.message}),
                );
                OperationError::with_details(
                    error.code.as_str(),
                    error.message,
                    json!({"artifact_id": artifact_id}),
                )
            })?;
        self.emit(
            Kind::ArtifactPublished,
            event_scope,
            json!({"operation_id": operation.operation_id, "artifact_id": artifact_id,
                "kind": kind_name, "declared_bytes": artifact.manifest.declared_bytes,
                "sha256": artifact.manifest.sha256}),
        );
        Ok(artifact)
    }

    fn capture_observation(
        &self,
        operation: &OperationSnapshot,
        request: &ObserveRequest,
        target: WindowCaptureTarget<'_>,
        semantics: Option<SemanticsReadReply>,
    ) -> Result<Value, OperationError> {
        let WindowCaptureTarget {
            run,
            window,
            pid,
            provider,
            timeout,
        } = target;
        let scene_epoch_before = window.scene_epoch;
        let semantics = if request
            .require
            .iter()
            .any(|requirement| requirement == "semantics" || requirement == "semantics.read")
        {
            let Some(result) = semantics else {
                return Err(OperationError::new(
                    "unavailable",
                    "the runtime did not return a semantics result",
                ));
            };
            if result.status != "ready" || !result.a11y_active || result.tree_json.is_none() {
                return Err(OperationError::with_details(
                    "unavailable",
                    "semantic capture is unavailable for the selected window",
                    json!({"provider": "gpui-debug-a11y", "status": result.status,
                        "a11y_active": result.a11y_active, "reason": result.reason,
                        "captured_at_ms": result.captured_at_ms}),
                ));
            }
            Some(result)
        } else {
            None
        };
        if !self.operation_is_running(&operation.operation_id) {
            return Err(OperationError::new(
                "cancelled",
                "observe was cancelled before the window screenshot started",
            ));
        }
        let capture = if let Some(provider) = provider {
            let pid = pid.expect("screenshot provider requires a process id");
            match capture::capture_window_with_cancel(pid, &window.title, timeout, || {
                !self.operation_is_running(&operation.operation_id)
            }) {
                Ok(capture) => Some(capture),
                Err(_) if !self.operation_is_running(&operation.operation_id) => {
                    return Err(OperationError::new(
                        "cancelled",
                        "observe was cancelled while the window screenshot was being captured",
                    ));
                }
                Err(error) => {
                    let code = error
                        .downcast_ref::<capture::CaptureError>()
                        .map_or("capture_failed", |failure| failure.code());
                    return Err(OperationError::with_details(
                        code,
                        error.to_string(),
                        json!({"provider": provider, "code": code, "window_id": window.window_id,
                            "run_id": run.scope.run_id, "pid": pid}),
                    ));
                }
            }
        } else {
            None
        };
        if !self.operation_is_running(&operation.operation_id) {
            return Err(OperationError::new(
                "cancelled",
                "observe was cancelled while the window screenshot was being captured",
            ));
        }

        let current_state = self.store.state();
        let same_run = current_state.running.as_ref().is_some_and(|current| {
            current.scope.run_id == run.scope.run_id
                && current.process == "running"
                && (!request.sync || current.assets_confirmed)
        });
        let current_window = self
            .windows
            .snapshots(run.scope.run_id.as_deref())
            .into_iter()
            .find(|current| current.window_id == window.window_id);
        let same_window = current_window.as_ref().is_some_and(|current| {
            current.lifecycle == "open"
                && current.title == window.title
                && current.ui == "responsive"
        });
        if !same_run || !same_window {
            return Err(OperationError::with_details(
                "stale_observation",
                "the app run or window changed during screenshot capture",
                json!({"run_id": run.scope.run_id, "window_id": window.window_id}),
            ));
        }
        let (latest_revision, _, latest_hash) = self
            .sync_inputs_with_delta_and_hash()
            .map_err(|error| OperationError::new("input_scan_failed", error.to_string()))?;
        if latest_revision != request.target_revision || latest_hash != request.input_hash {
            return Err(OperationError::with_details(
                if request.sync {
                    "superseded"
                } else {
                    "capture_unstable"
                },
                "project inputs changed during screenshot capture",
                json!({"target_revision": request.target_revision,
                    "current_revision": latest_revision,
                    "target_input_hash": request.input_hash,
                    "current_input_hash": latest_hash}),
            ));
        }
        let identity = format!("{:x}", Sha256::digest(operation.operation_id.as_bytes()));
        let event_scope = Scope {
            build_id: run.scope.build_id.clone(),
            run_id: run.scope.run_id.clone(),
            revision: run.scope.revision.clone(),
        };
        let screenshot_artifact = if let Some(capture) = capture.as_ref() {
            let (pixel_width, pixel_height) = png_dimensions(&capture.bytes)
                .map_err(|message| OperationError::new("capture_invalid", message))?;
            let artifact = self.publish_observation_artifact(ArtifactPublication {
                operation,
                event_scope: &event_scope,
                run_id: run.scope.run_id.clone(),
                identity: &identity[..32],
                suffix: "window",
                bytes: &capture.bytes,
                kind: ArtifactKind::Png,
                mime: "image/png",
                provider,
                scope: "window",
            })?;
            Some((artifact, pixel_width, pixel_height))
        } else {
            None
        };
        let semantics_artifact = if let Some(semantics) = semantics.as_ref() {
            let tree = semantics
                .tree_json
                .as_ref()
                .expect("ready semantics result has a tree");
            let tree_value: Value = serde_json::from_str(tree).map_err(|error| {
                OperationError::with_details(
                    "semantics_invalid",
                    "the runtime returned invalid semantics JSON",
                    json!({"message": error.to_string()}),
                )
            })?;
            let node_count = tree_value
                .get("nodes")
                .and_then(Value::as_object)
                .map_or(0, serde_json::Map::len);
            let artifact = self.publish_observation_artifact(ArtifactPublication {
                operation,
                event_scope: &event_scope,
                run_id: run.scope.run_id.clone(),
                identity: &identity[..32],
                suffix: "semantics",
                bytes: tree.as_bytes(),
                kind: ArtifactKind::Tree,
                mime: "application/json",
                provider: Some("gpui-debug-a11y"),
                scope: "semantics",
            })?;
            Some((artifact, node_count, semantics.captured_at_ms))
        } else {
            None
        };
        let scene_matches = current_window.as_ref().is_some_and(|current| {
            semantics.is_none()
                && current.scene_epoch == scene_epoch_before
                && current.scene_source_revision == Some(run.scope.revision.source_revision)
                && current.scene_asset_revision == Some(run.scope.revision.asset_revision)
        });
        let mut artifacts = Vec::new();
        if let Some((artifact, _, _)) = &screenshot_artifact {
            artifacts.push(json!(artifact));
        }
        if let Some((artifact, _, _)) = &semantics_artifact {
            artifacts.push(json!(artifact));
        }
        let (pixel_width, pixel_height) = screenshot_artifact
            .as_ref()
            .map(|(_, width, height)| (*width, *height))
            .unwrap_or((0, 0));
        let observation_provider = provider.unwrap_or("gpui-debug-a11y");
        let observation_id = format!("observation-{}", &identity[..32]);
        let mut result = json!({
            "observation_id": observation_id,
            "run_id": run.scope.run_id,
            "build_id": run.scope.build_id,
            "source_revision": run.scope.revision.source_revision,
            "asset_revision": run.scope.revision.asset_revision,
            "input_hash": request.input_hash,
            "input_consistency": "tracked_scan",
            "window_id": window.window_id,
            "foreground": window.foreground,
            "target_source_revision": request.target_revision.source_revision,
            "target_asset_revision": request.target_revision.asset_revision,
            "scene_epoch_before": scene_epoch_before,
            "scene_epoch_after": current_window.as_ref().map(|current| current.scene_epoch),
            "presented_frame_id": current_window.as_ref().and_then(|current| current.presented_frame_id.clone()),
            "provider": observation_provider,
            "scope": if provider.is_some() { "window" } else { "semantics" },
            "consistency": "best_effort",
            "capture_started_at_ms": capture.as_ref().map(|capture| capture.started_at_ms),
            "capture_finished_at_ms": capture.as_ref().map(|capture| capture.finished_at_ms),
            "window_number": capture.as_ref().map(|capture| capture.window_number),
            "window_match": capture.as_ref().map(|capture| capture.window_match.clone()),
            "window_bounds": capture.as_ref().map(|capture| capture.bounds),
            "pixel_width": capture.as_ref().map(|_| pixel_width),
            "pixel_height": capture.as_ref().map(|_| pixel_height),
            "logical_width": window.width,
            "logical_height": window.height,
            "scale_milli": window.scale_milli,
            "orientation": capture
                .as_ref()
                .map(|_| capture_orientation(pixel_width, pixel_height)),
            "includes_system_ui": false,
            "freshness": {
                "source": if run.scope.revision.source_revision == request.target_revision.source_revision { "current" } else { "stale" },
                "assets": if run.assets_confirmed && run.scope.revision.asset_revision == request.target_revision.asset_revision { "applied" } else { "unknown" },
                "scene": if scene_matches { "matches" } else { "unknown" },
            },
            "artifacts": artifacts,
        });
        if let Some((artifact, node_count, captured_at_ms)) = semantics_artifact {
            result["semantics"] = json!({
                "artifact": artifact,
                "artifact_id": result["artifacts"].as_array()
                    .and_then(|artifacts| artifacts.last())
                    .and_then(|artifact| artifact["artifact_id"].as_str()),
                "node_count": node_count,
                "captured_at_ms": captured_at_ms,
                "scene_epoch": current_window.as_ref().map(|current| current.scene_epoch),
                "provider": "gpui-debug-a11y",
                "consistency": "best_effort",
            });
        }
        Ok(result)
    }

    fn operation_is_running(&self, operation_id: &str) -> bool {
        self.expire_operations();
        self.operations
            .get(operation_id, events::now_ms())
            .is_ok_and(|operation| operation.state == OperationState::Running)
    }

    pub fn start_operation(&self, operation_id: &str) -> Result<Transition, OperationError> {
        self.expire_operations();
        let transition = self.operations.start(operation_id, events::now_ms())?;
        if transition.changed {
            self.emit_operation(Kind::OperationStarted, &transition.snapshot);
        }
        Ok(transition)
    }

    pub fn bind_operation_scope(
        &self,
        operation_id: &str,
        scope: Scope,
    ) -> Result<Transition, OperationError> {
        let transition = self
            .operations
            .bind_scope(operation_id, scope, events::now_ms())?;
        if transition.changed {
            self.emit_operation(Kind::OperationBound, &transition.snapshot);
        }
        Ok(transition)
    }

    pub fn finish_operation(
        &self,
        operation_id: &str,
        state: OperationState,
        result: Option<Value>,
        error: Option<OperationError>,
    ) -> Result<Transition, OperationError> {
        self.expire_operations();
        let transition =
            self.operations
                .finish(operation_id, state, result, error, events::now_ms())?;
        if transition.changed {
            self.emit_operation(Kind::OperationFinished, &transition.snapshot);
        }
        Ok(transition)
    }

    pub fn wait_operation(
        &self,
        operation_id: &str,
        wait_ms: u64,
    ) -> Result<OperationSnapshot, OperationError> {
        let _ = self.operations.wait(operation_id, wait_ms)?;
        self.expire_operations();
        self.operations.get(operation_id, events::now_ms())
    }

    pub fn cancel_operation(&self, operation_id: &str) -> Result<Transition, OperationError> {
        self.expire_operations();
        let transition = self.operations.cancel(operation_id, events::now_ms())?;
        if transition.changed {
            self.emit_operation(Kind::OperationFinished, &transition.snapshot);
        }
        Ok(transition)
    }

    pub fn expire_operations(&self) -> Vec<OperationSnapshot> {
        let expired = self.operations.expire_due(events::now_ms());
        for snapshot in &expired {
            self.emit_operation(Kind::OperationFinished, snapshot);
        }
        expired
    }

    fn emit_operation(&self, kind: Kind, snapshot: &OperationSnapshot) {
        self.emit(kind, &snapshot.scope, json!({"operation": snapshot}));
    }

    /// Enqueues a supervisor-owned build trigger. There is intentionally only
    /// one pending trigger: edits and explicit requests that arrive together
    /// must share the next build rather than creating an unbounded queue.
    pub fn request_build(&self, request_id: &str) -> BuildRequestResult {
        let mut pending = self
            .build_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let coalesced = pending.is_some();
        if pending.is_none() {
            *pending = Some(BuildRequest {
                request_id: request_id.to_owned(),
            });
        }
        let queue_depth = usize::from(pending.is_some());
        let scope = Scope {
            revision: self.store.state().desired,
            ..Scope::default()
        };
        self.emit(
            Kind::BuildRequested,
            &scope,
            json!({
                "request_id": request_id,
                "accepted": true,
                "coalesced": coalesced,
                "queue_depth": queue_depth,
            }),
        );
        BuildRequestResult {
            accepted: true,
            coalesced,
            queue_depth,
        }
    }

    /// Takes the current explicit build trigger for the live coordinator.
    /// Watcher changes can still coalesce into the same cycle through its
    /// existing event channel.
    pub fn take_build_request(&self) -> Option<BuildRequest> {
        self.build_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    pub fn emit(&self, kind: Kind, scope: &Scope, data: Value) {
        if let Some(error) = self.timing.last_error() {
            self.store.storage_error(error);
        }
        self.store.emit(kind, scope, data);
    }

    pub fn start_span(
        &self,
        name: impl Into<String>,
        scope: &Scope,
        parent_id: Option<&str>,
        attributes: Value,
    ) -> SpanGuard {
        self.timing.start(name, scope, parent_id, attributes)
    }

    pub fn sync_inputs(&self) -> Result<Revision> {
        self.sync_inputs_with_delta().map(|(revision, _)| revision)
    }

    pub fn sync_inputs_with_delta(&self) -> Result<(Revision, AssetDelta)> {
        self.sync_inputs_with_delta_and_hash()
            .map(|(revision, delta, _)| (revision, delta))
    }

    pub fn sync_inputs_with_delta_and_hash(&self) -> Result<(Revision, AssetDelta, String)> {
        let scan_scope = Scope {
            revision: self.store.state().desired,
            ..Scope::default()
        };
        let scan = self.start_span("inputs.scan", &scan_scope, None, json!({}));
        // Serialize scans without holding the event/state mutex. Queries stay
        // responsive even with a large workspace or a running compiler.
        let mut previous = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
        let inputs = match Inputs::scan(&self.root) {
            Ok(inputs) => inputs,
            Err(error) => {
                let message = error.to_string();
                scan.finish("failed", Some(&message));
                return Err(error);
            }
        };
        let asset_delta = AssetDelta::between(&previous.assets, &inputs.assets);
        let input_hash = inputs.digest();
        let mut revision = self.store.state().desired;
        if revision.source_revision == 0 || inputs != *previous {
            if revision.source_revision == 0
                || inputs.sources != previous.sources
                || inputs.untracked_directory_links != previous.untracked_directory_links
            {
                revision.source_revision += 1;
            }
            if inputs.assets != previous.assets {
                revision.asset_revision += 1;
            }
            self.emit(Kind::SourceChanged, &Scope { revision: revision.clone(), ..Scope::default() },
                json!({"input_hash": inputs.digest(), "sources": inputs.sources.len(), "assets": inputs.assets.len(),
                    "untracked_directory_links": inputs.untracked_directory_links}));
            *previous = inputs;
        }
        scan.finish("ok", None);
        Ok((revision, asset_delta, input_hash))
    }

    pub fn asset_manifest(&self) -> Vec<AssetManifestEntry> {
        self.inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .assets
            .iter()
            .map(|(path, hash)| AssetManifestEntry {
                path: path.clone(),
                hash: hash.clone(),
            })
            .collect()
    }

    pub fn begin_build(self: &Arc<Self>) -> Result<Build> {
        self.sync_inputs()?;
        let inputs = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
        let scope = Scope {
            build_id: Some(format!(
                "b{}",
                self.next_build.fetch_add(1, Ordering::Relaxed)
            )),
            revision: self.store.state().desired,
            ..Scope::default()
        };
        let build_span = self.start_span(
            "build",
            &scope,
            None,
            json!({"input_hash": inputs.digest()}),
        );
        let queue_span = self.start_span(
            "build.queue",
            &scope,
            Some(build_span.span_id()),
            json!({"input_hash": inputs.digest()}),
        );
        let manifest = self.record(&scope, "inputs", &json!(inputs.clone()));
        self.emit(
            Kind::BuildStarted,
            &scope,
            json!({"input_hash": inputs.digest(), "manifest": manifest}),
        );
        queue_span.finish("ok", None);
        Ok(Build {
            session: self.clone(),
            scope,
            span: build_span,
        })
    }

    pub fn begin_run(&self, build: &Build) -> Scope {
        let scope = Scope {
            run_id: Some(format!(
                "r{}",
                self.next_run.fetch_add(1, Ordering::Relaxed)
            )),
            ..build.scope.clone()
        };
        self.emit(Kind::AppStarting, &scope, json!({}));
        scope
    }

    pub fn current_run(&self) -> Option<Scope> {
        self.store.state().running.map(|r| r.scope)
    }

    pub fn record(&self, scope: &Scope, source: &str, data: &Value) -> Option<LogRef> {
        let record = json!({"received_at_ms": events::now_ms(), "scope": scope, "source": source, "data": data});
        let mut output = self.output.lock().unwrap_or_else(|e| e.into_inner());
        match serde_json::to_vec(&record)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| output.append(&bytes))
        {
            Ok(reference) => Some(reference),
            Err(error) => {
                self.store.storage_error(error.to_string());
                None
            }
        }
    }

    pub fn output(
        &self,
        scope: &Scope,
        stage: &str,
        stream: &str,
        bytes: &[u8],
        continued: bool,
    ) -> Option<LogRef> {
        let text = String::from_utf8_lossy(bytes);
        let raw = json!({"stage": stage, "stream": stream, "text": text, "continued": continued,
            "invalid_utf8_base64": if std::str::from_utf8(bytes).is_err() { Some(super::protocol::b64::encode(bytes)) } else { None }});
        let reference = self.record(scope, "output", &raw);
        self.emit(
            Kind::Output,
            scope,
            json!({"stage": stage, "stream": stream, "text": events::clip(&text, 4096),
            "truncated": text.len() > 4096, "continued": continued, "log": reference}),
        );
        reference
    }

    pub fn end(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.emit(Kind::SessionEnded, &Scope::default(), json!({}));
        let _ = self.timing.flush();
    }
}

impl Build {
    pub fn span_id(&self) -> &str {
        self.span.span_id()
    }

    pub fn is_current(&self) -> Result<bool> {
        Ok(!self.session.stopping.load(Ordering::SeqCst)
            && self.session.sync_inputs()? == self.scope.revision)
    }

    pub fn finish(&self, success: bool, error: Option<String>) {
        let mut data = json!({"success": success});
        if let Some(error) = &error {
            data["error"] = json!(error);
        }
        self.session.emit(Kind::BuildFinished, &self.scope, data);
        self.span.finish(
            if success {
                "ok"
            } else if self.session.stopping.load(Ordering::SeqCst) {
                "cancelled"
            } else {
                "failed"
            },
            error.as_deref(),
        );
    }

    pub fn superseded(&self) {
        self.session.emit(
            Kind::BuildSuperseded,
            &self.scope,
            json!({"desired": self.session.store.state().desired}),
        );
        self.span.finish("superseded", None);
    }
}

pub fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("generating live credentials: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn normalize_requirements(values: &[String]) -> Result<Vec<String>, OperationError> {
    let values = if values.is_empty() {
        vec!["screenshot".to_owned()]
    } else {
        values
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let allowed = [
        "screenshot",
        "semantics",
        "capture.scene",
        "capture.window",
        "capture.device",
        "semantics.read",
    ];
    let mut normalized = BTreeSet::new();
    for value in values {
        if !allowed.contains(&value.as_str()) {
            return Err(OperationError::with_details(
                "invalid_requirement",
                format!("unsupported observe requirement `{value}`"),
                json!({"requirement": value, "allowed": allowed}),
            ));
        }
        normalized.insert(value);
    }
    if normalized.is_empty() {
        return Err(OperationError::new(
            "invalid_requirement",
            "observe requires at least one capability",
        ));
    }
    Ok(normalized.into_iter().collect())
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("capture provider did not return a valid PNG header".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("four PNG width bytes"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("four PNG height bytes"));
    if width == 0 || height == 0 {
        return Err("capture provider returned a PNG with empty dimensions".into());
    }
    Ok((width, height))
}

fn capture_orientation(width: u32, height: u32) -> &'static str {
    match width.cmp(&height) {
        std::cmp::Ordering::Less => "portrait",
        std::cmp::Ordering::Equal => "square",
        std::cmp::Ordering::Greater => "landscape",
    }
}

#[cfg(test)]
mod observation_tests {
    use super::{capture_orientation, png_dimensions};

    #[test]
    fn png_dimensions_require_signature_and_nonzero_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(png_dimensions(&png).unwrap(), (640, 480));
        assert!(png_dimensions(b"not a png").is_err());

        png[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert!(png_dimensions(&png).is_err());
        assert_eq!(capture_orientation(640, 480), "landscape");
        assert_eq!(capture_orientation(480, 640), "portrait");
        assert_eq!(capture_orientation(480, 480), "square");
    }
}
