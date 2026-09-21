//! Monotonic supervisor spans for the live development loop.
//!
//! Wall-clock timestamps are useful for a human log, but they cannot be used
//! to compare stages when a machine clock is adjusted. This module records
//! elapsed nanoseconds from one session-local `Instant` and keeps the raw
//! records in a bounded `spans.ndjson` file.

use super::events::{Scope, now_ms};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_SPAN_LOG_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SPAN_RECORDS: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpanRecord {
    pub schema_version: u32,
    pub trace_id: String,
    pub span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub name: String,
    /// `None` is intentional for a stage that has not been instrumented yet;
    /// it must not be represented by a fake zero-duration measurement.
    pub started_at_ns: Option<u64>,
    pub ended_at_ns: Option<u64>,
    pub duration_ns: Option<u64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub attributes: Value,
    pub recorded_at_ms: u64,
}

struct SpanLog {
    file: File,
    bytes: usize,
    records: VecDeque<Vec<u8>>,
    dropped: u64,
}

impl SpanLog {
    fn new(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("spans.ndjson");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let bytes = file.seek(SeekFrom::End(0))? as usize;
        Ok(Self {
            file,
            bytes,
            records: VecDeque::new(),
            dropped: 0,
        })
    }

    fn append(&mut self, mut line: Vec<u8>) -> Result<()> {
        line.push(b'\n');
        if line.len() > MAX_SPAN_LOG_BYTES {
            self.dropped += 1;
            return Ok(());
        }

        self.records.push_back(line.clone());
        let evicted = if self.records.len() > MAX_SPAN_RECORDS {
            self.records.pop_front();
            self.dropped += 1;
            true
        } else {
            false
        };
        let needs_compaction = evicted || self.bytes + line.len() > MAX_SPAN_LOG_BYTES;
        if needs_compaction {
            self.file.set_len(0)?;
            self.file.seek(SeekFrom::Start(0))?;
            self.bytes = 0;
            for record in &self.records {
                self.file.write_all(record)?;
                self.bytes += record.len();
            }
        } else {
            self.file.seek(SeekFrom::End(0))?;
            self.file.write_all(&line)?;
            self.bytes += line.len();
        }
        self.file.flush()?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        Ok(())
    }
}

/// Session-local span recorder. A failed span write is retained as metadata so
/// callers can surface storage trouble without taking down the live loop.
pub struct Timing {
    session_id: String,
    trace_id: String,
    origin: Instant,
    next_span: AtomicU64,
    log: Mutex<SpanLog>,
    last_error: Mutex<Option<String>>,
}

impl Timing {
    pub fn new(dir: &Path, session_id: &str) -> Result<Self> {
        Ok(Self {
            session_id: session_id.to_string(),
            trace_id: format!("trace-{session_id}"),
            origin: Instant::now(),
            next_span: AtomicU64::new(1),
            log: Mutex::new(SpanLog::new(dir)?),
            last_error: Mutex::new(None),
        })
    }

    pub fn start(
        self: &Arc<Self>,
        name: impl Into<String>,
        scope: &Scope,
        parent_id: Option<&str>,
        attributes: Value,
    ) -> SpanGuard {
        SpanGuard {
            recorder: self.clone(),
            span_id: format!("sp{}", self.next_span.fetch_add(1, Ordering::Relaxed)),
            parent_id: parent_id.map(str::to_owned),
            build_id: scope.build_id.clone(),
            run_id: scope.run_id.clone(),
            name: name.into(),
            started_at_ns: self.elapsed_ns(),
            attributes,
            finished: AtomicBool::new(false),
        }
    }

    /// Record a known gap in instrumentation explicitly. In particular, this
    /// keeps future reports from mistaking an absent UI probe for zero cost.
    pub fn not_instrumented(&self, name: impl Into<String>, scope: &Scope, attributes: Value) {
        let record = SpanRecord {
            schema_version: SCHEMA_VERSION,
            trace_id: self.trace_id.clone(),
            span_id: format!("sp{}", self.next_span.fetch_add(1, Ordering::Relaxed)),
            parent_id: None,
            session_id: self.session_id.clone(),
            build_id: scope.build_id.clone(),
            run_id: scope.run_id.clone(),
            operation_id: None,
            name: name.into(),
            started_at_ns: None,
            ended_at_ns: None,
            duration_ns: None,
            status: "not_instrumented".into(),
            error: None,
            attributes,
            recorded_at_ms: now_ms(),
        };
        self.write(record);
    }

    pub fn flush(&self) -> Result<()> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn elapsed_ns(&self) -> u64 {
        self.origin
            .elapsed()
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn write(&self, record: SpanRecord) {
        let result = serde_json::to_vec(&record)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| {
                self.log
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .append(bytes)
            });
        if let Err(error) = result {
            *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
        }
    }
}

pub struct SpanGuard {
    recorder: Arc<Timing>,
    span_id: String,
    parent_id: Option<String>,
    build_id: Option<String>,
    run_id: Option<String>,
    name: String,
    started_at_ns: u64,
    attributes: Value,
    finished: AtomicBool,
}

impl SpanGuard {
    pub fn span_id(&self) -> &str {
        &self.span_id
    }

    pub fn finish(&self, status: &str, error: Option<&str>) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let ended_at_ns = self.recorder.elapsed_ns();
        self.recorder.write(SpanRecord {
            schema_version: SCHEMA_VERSION,
            trace_id: self.recorder.trace_id.clone(),
            span_id: self.span_id.clone(),
            parent_id: self.parent_id.clone(),
            session_id: self.recorder.session_id.clone(),
            build_id: self.build_id.clone(),
            run_id: self.run_id.clone(),
            operation_id: None,
            name: self.name.clone(),
            started_at_ns: Some(self.started_at_ns),
            ended_at_ns: Some(ended_at_ns),
            duration_ns: Some(ended_at_ns.saturating_sub(self.started_at_ns)),
            status: status.into(),
            error: error.map(str::to_owned),
            attributes: self.attributes.clone(),
            recorded_at_ms: now_ms(),
        });
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        self.finish("cancelled", None);
    }
}

/// Map existing live stage labels to the stable performance vocabulary.
pub fn stage_name(stage: &str) -> &str {
    if stage == "cargo.build" || stage == "cargo.ndk" {
        "cargo.compile"
    } else if stage == "xcodegen generate"
        || stage.starts_with("xcodebuild")
        || stage.starts_with("gradlew")
    {
        "native.package"
    } else if stage == "rustup.target" {
        "toolchain.target"
    } else if stage == "ios.install" || stage == "android.install" {
        "device.install"
    } else if stage == "ios.launch" || stage == "android.launch" {
        "app.launch"
    } else {
        stage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    fn scope() -> Scope {
        Scope {
            build_id: Some("b1".into()),
            run_id: Some("r1".into()),
            revision: super::super::events::Revision {
                source_revision: 1,
                asset_revision: 2,
            },
        }
    }

    #[test]
    fn records_monotonic_parented_spans() {
        let dir = tempfile::tempdir().unwrap();
        let timing = Arc::new(Timing::new(dir.path(), "session-1").unwrap());
        let parent = timing.start("build", &scope(), None, serde_json::json!({}));
        thread::sleep(Duration::from_millis(1));
        let child = timing.start(
            "cargo.compile",
            &scope(),
            Some(parent.span_id()),
            serde_json::json!({"stage": "cargo.build"}),
        );
        child.finish("ok", None);
        parent.finish("failed", Some("fixture failure"));

        let lines = fs::read_to_string(dir.path().join("spans.ndjson")).unwrap();
        let records: Vec<SpanRecord> = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|record| { record.ended_at_ns.unwrap() >= record.started_at_ns.unwrap() })
        );
        let parent = records
            .iter()
            .find(|record| record.name == "build")
            .unwrap();
        let child = records
            .iter()
            .find(|record| record.name == "cargo.compile")
            .unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(parent.span_id.as_str()));
        assert_eq!(parent.status, "failed");
        assert_eq!(parent.error.as_deref(), Some("fixture failure"));
    }

    #[test]
    fn uninstrumented_stages_have_no_fake_duration() {
        let dir = tempfile::tempdir().unwrap();
        let timing = Arc::new(Timing::new(dir.path(), "session-2").unwrap());
        timing.not_instrumented("ui.ready", &Scope::default(), serde_json::json!({}));
        let line = fs::read_to_string(dir.path().join("spans.ndjson")).unwrap();
        let record: SpanRecord = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record.status, "not_instrumented");
        assert!(record.started_at_ns.is_none());
        assert!(record.ended_at_ns.is_none());
        assert!(record.duration_ns.is_none());
    }

    #[test]
    fn dropped_span_is_recorded_as_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let timing = Arc::new(Timing::new(dir.path(), "session-cancel").unwrap());
        let span = timing.start(
            "cargo.compile",
            &Scope::default(),
            None,
            serde_json::json!({}),
        );
        drop(span);
        let line = fs::read_to_string(dir.path().join("spans.ndjson")).unwrap();
        let record: SpanRecord = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record.status, "cancelled");
        assert!(record.duration_ns.is_some());
    }

    #[test]
    fn stable_stage_names_keep_install_and_launch_separate() {
        assert_eq!(stage_name("ios.install"), "device.install");
        assert_eq!(stage_name("android.launch"), "app.launch");
        assert_eq!(stage_name("cargo.build"), "cargo.compile");
    }

    #[test]
    fn span_log_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let timing = Arc::new(Timing::new(dir.path(), "session-3").unwrap());
        for _ in 0..(MAX_SPAN_RECORDS + 8) {
            let span = timing.start("stage", &Scope::default(), None, serde_json::json!({}));
            span.finish("ok", None);
        }
        let metadata = fs::metadata(dir.path().join("spans.ndjson")).unwrap();
        assert!(metadata.len() <= MAX_SPAN_LOG_BYTES as u64);
        let count = fs::read_to_string(dir.path().join("spans.ndjson"))
            .unwrap()
            .lines()
            .count();
        assert!(count <= MAX_SPAN_RECORDS);
    }
}
