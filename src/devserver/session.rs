//! One live session, shared by build, process, app-channel and control workers.

use super::events::{self, EventStore, Kind, LogRef, Revision, RollingFile, Scope, State};
use super::inputs::{AssetDelta, Inputs};
use super::timing::{SpanGuard, Timing};
use super::windows::WindowRegistry;
use anyhow::{Context, Result};
use serde_json::{Value, json};
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
    next_build: AtomicU64,
    next_run: AtomicU64,
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
                "native_mobile_logs": false, "timing_spans": true}),
            diagnostics: Vec::new(),
            diagnostics_omitted: 0,
            runtime_issues: Vec::new(),
            runtime_issues_omitted: 0,
            storage_error: None,
            watcher_error: None,
        };
        let timing = Arc::new(Timing::new(&dir, &id)?);
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
            dir,
            stopping: AtomicBool::new(false),
            inputs: Mutex::new(Inputs::default()),
            next_build: AtomicU64::new(1),
            next_run: AtomicU64::new(1),
        });
        for stage in [
            "device.lease_wait",
            "ui.ready",
            "observation.capture",
            "artifact.publish",
        ] {
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
