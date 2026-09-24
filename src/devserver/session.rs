//! One live session, shared by build, process, app-channel and control workers.

use super::artifacts::{ArtifactLimits, ArtifactStore};
use super::events::{self, EventStore, Kind, LogRef, Revision, RollingFile, Scope, State};
use super::inputs::{AssetDelta, Inputs};
use super::operations::{
    MAX_ACTIVE_OPERATIONS, OperationError, OperationSnapshot, OperationState, OperationStore,
    SubmitResult, Transition,
};
use super::protocol::AssetManifestEntry;
use super::timing::{SpanGuard, Timing};
use super::windows::WindowRegistry;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
                "capture.window": {"available": false, "reason": "backend_unsupported",
                    "provider": null, "constraints": {}},
                "capture.device": {"available": false, "reason": "backend_unsupported",
                    "provider": null, "constraints": {}},
                "semantics.read": {"available": false, "reason": "backend_unsupported",
                    "provider": null, "constraints": {}},
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
        let (target_revision, _) = self
            .sync_inputs_with_delta()
            .map_err(|error| OperationError::new("input_scan_failed", error.to_string()))?;
        let input_hash = self.input_hash();
        let run_id = self
            .store
            .state()
            .running
            .as_ref()
            .and_then(|run| run.scope.run_id.clone());
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
            "input_hash": input_hash,
            "input_consistency": "tracked_scan",
        });
        let scope = Scope {
            revision: target_revision.clone(),
            ..Scope::default()
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

    /// Advances queued observe operations from the single live coordinator.
    /// Capability resolution happens here rather than in a control worker so a
    /// later provider can use the same transition boundary for build, run and
    /// capture work. Until a real provider is registered, an observation ends
    /// explicitly as unavailable instead of claiming scene evidence is an
    /// image or semantics result.
    pub fn advance_observe_requests(&self) {
        self.expire_operations();
        while let Some(request) = self.take_observe_request() {
            let Ok(started) = self.start_operation(&request.operation_id) else {
                continue;
            };
            if !started.changed {
                continue;
            }
            let unavailable = self.unavailable_observe_requirements(&request.require);
            let error = OperationError::with_details(
                "unavailable",
                "no requested observation provider is currently available",
                json!({
                    "operation_id": request.operation_id,
                    "sync": request.sync,
                    "window_id": request.window_id,
                    "required": request.require,
                    "unavailable": unavailable,
                    "source_revision": request.target_revision.source_revision,
                    "asset_revision": request.target_revision.asset_revision,
                    "input_hash": request.input_hash,
                }),
            );
            let _ = self.finish_operation(
                &started.snapshot.operation_id,
                OperationState::Failed,
                None,
                Some(error),
            );
        }
    }

    fn unavailable_observe_requirements(&self, requirements: &[String]) -> Vec<Value> {
        requirements
            .iter()
            .map(|requirement| match requirement.as_str() {
                "screenshot" => json!({
                    "requirement": requirement,
                    "alternatives": ["capture.scene", "capture.window", "capture.device"],
                    "reason": "backend_unsupported",
                }),
                "semantics" => json!({
                    "requirement": requirement,
                    "alternatives": ["semantics.read"],
                    "reason": "backend_unsupported",
                }),
                capability => json!({
                    "requirement": capability,
                    "alternatives": [capability],
                    "reason": "backend_unsupported",
                }),
            })
            .collect()
    }

    pub fn start_operation(&self, operation_id: &str) -> Result<Transition, OperationError> {
        self.expire_operations();
        let transition = self.operations.start(operation_id, events::now_ms())?;
        if transition.changed {
            self.emit_operation(Kind::OperationStarted, &transition.snapshot);
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
        Ok((revision, asset_delta))
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

    pub fn input_hash(&self) -> String {
        self.inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .digest()
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
