//! Bounded asynchronous operation state and idempotency records.
//!
//! The coordinator owns the actual work. This module only owns the small,
//! queryable state machine around it, so control requests never need to hold a
//! build, app-channel, or artifact lock while waiting.

use super::events::{self, Scope};
use gpui_dev_protocol::valid_request_id;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub const MAX_ACTIVE_OPERATIONS: usize = 4;
pub const MAX_RETAINED_OPERATIONS: usize = 2048;
pub const MAX_RETAINED_BYTES: usize = 8 * 1024 * 1024;
pub const OPERATION_TTL_MS: u64 = 10 * 60 * 1000;
pub const MAX_DEADLINE_MS: u64 = 120 * 1000;

const MAX_KIND_BYTES: usize = 64;
const MAX_TARGET_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Superseded,
    Unknown,
}

impl OperationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::TimedOut
                | Self::Superseded
                | Self::Unknown
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OperationError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl OperationError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(code: &str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Some(details),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationSnapshot {
    pub operation_id: String,
    pub request_id: String,
    pub kind: String,
    #[serde(flatten)]
    pub scope: Scope,
    pub target: Value,
    pub state: OperationState,
    pub created_at_ms: u64,
    pub deadline_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OperationError>,
    pub cancel_requested: bool,
}

#[derive(Clone, Debug)]
struct Record {
    snapshot: OperationSnapshot,
}

#[derive(Clone, Debug)]
struct RequestRecord {
    operation_id: String,
    fingerprint: String,
}

struct Inner {
    records: HashMap<String, Record>,
    requests: HashMap<String, RequestRecord>,
    request_order: VecDeque<String>,
    terminal_order: VecDeque<String>,
    next_id: u64,
    bytes: usize,
}

#[derive(Clone, Debug)]
pub enum SubmitResult {
    Created(OperationSnapshot),
    Existing(OperationSnapshot),
}

#[derive(Clone, Debug)]
pub struct Transition {
    pub snapshot: OperationSnapshot,
    pub changed: bool,
}

pub struct OperationStore {
    inner: Mutex<Inner>,
    changed: Condvar,
    max_active: usize,
}

impl OperationStore {
    pub fn new(max_active: usize) -> Self {
        assert!(max_active > 0);
        Self {
            inner: Mutex::new(Inner {
                records: HashMap::new(),
                requests: HashMap::new(),
                request_order: VecDeque::new(),
                terminal_order: VecDeque::new(),
                next_id: 1,
                bytes: 0,
            }),
            changed: Condvar::new(),
            max_active,
        }
    }

    pub fn submit(
        &self,
        request_id: &str,
        kind: &str,
        scope: Scope,
        target: Value,
        deadline_at_ms: u64,
        now_ms: u64,
    ) -> Result<SubmitResult, OperationError> {
        if !valid_request_id(request_id) {
            return Err(OperationError::new(
                "invalid_request_id",
                "request_id must be 1-128 ASCII identifier characters",
            ));
        }
        if kind.is_empty() || kind.len() > MAX_KIND_BYTES || !kind.is_ascii() {
            return Err(OperationError::new(
                "invalid_operation_kind",
                "operation kind must be non-empty ASCII and at most 64 bytes",
            ));
        }
        let target_bytes = serde_json::to_vec(&target)
            .map_err(|error| OperationError::new("invalid_operation_target", error.to_string()))?;
        if target_bytes.len() > MAX_TARGET_BYTES {
            return Err(OperationError::with_details(
                "operation_target_too_large",
                "operation target exceeds the bounded request size",
                serde_json::json!({"max_bytes": MAX_TARGET_BYTES, "actual_bytes": target_bytes.len()}),
            ));
        }
        if deadline_at_ms <= now_ms {
            return Err(OperationError::new(
                "invalid_deadline",
                "operation deadline must be in the future",
            ));
        }
        if deadline_at_ms - now_ms > MAX_DEADLINE_MS {
            return Err(OperationError::with_details(
                "deadline_too_large",
                "operation deadline exceeds the maximum allowed duration",
                serde_json::json!({"max_ms": MAX_DEADLINE_MS}),
            ));
        }

        let fingerprint = fingerprint(kind, &scope, &target);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        prune_locked(&mut inner, now_ms);

        if let Some(existing) = inner.requests.get(request_id) {
            if existing.fingerprint != fingerprint {
                return Err(OperationError::with_details(
                    "idempotency_conflict",
                    "request_id was already used with different operation parameters",
                    serde_json::json!({"operation_id": existing.operation_id}),
                ));
            }
            let snapshot = inner
                .records
                .get(&existing.operation_id)
                .map(|record| record.snapshot.clone())
                .ok_or_else(|| {
                    OperationError::new(
                        "operation_expired",
                        "the idempotency record outlived its operation",
                    )
                })?;
            return Ok(SubmitResult::Existing(snapshot));
        }

        let active = inner
            .records
            .values()
            .filter(|record| {
                matches!(
                    record.snapshot.state,
                    OperationState::Queued | OperationState::Running
                )
            })
            .count();
        if active >= self.max_active {
            return Err(OperationError::new(
                "busy",
                "the live session has reached its active operation limit",
            ));
        }

        if inner.records.len() >= MAX_RETAINED_OPERATIONS {
            return Err(OperationError::new(
                "quota_exceeded",
                "operation history is full of active or retained records",
            ));
        }

        let operation_id = format!("op-{}-{}", std::process::id(), inner.next_id);
        inner.next_id = inner.next_id.saturating_add(1);
        let snapshot = OperationSnapshot {
            operation_id: operation_id.clone(),
            request_id: request_id.to_owned(),
            kind: kind.to_owned(),
            scope,
            target,
            state: OperationState::Queued,
            created_at_ms: now_ms,
            deadline_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
            result: None,
            error: None,
            cancel_requested: false,
        };
        let bytes = snapshot_bytes(&snapshot);
        if inner.bytes.saturating_add(bytes) > MAX_RETAINED_BYTES {
            return Err(OperationError::new(
                "quota_exceeded",
                "operation history exceeds its bounded memory budget",
            ));
        }
        inner.bytes = inner.bytes.saturating_add(bytes);
        inner.requests.insert(
            request_id.to_owned(),
            RequestRecord {
                operation_id: operation_id.clone(),
                fingerprint: fingerprint.clone(),
            },
        );
        inner.request_order.push_back(request_id.to_owned());
        inner.records.insert(
            operation_id,
            Record {
                snapshot: snapshot.clone(),
            },
        );
        Ok(SubmitResult::Created(snapshot))
    }

    pub fn get(
        &self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<OperationSnapshot, OperationError> {
        validate_operation_id(operation_id)?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        prune_locked(&mut inner, now_ms);
        inner
            .records
            .get(operation_id)
            .map(|record| record.snapshot.clone())
            .ok_or_else(|| {
                OperationError::new("operation_expired", "operation is unknown or expired")
            })
    }

    pub fn find_observation(
        &self,
        observation_id: &str,
        now_ms: u64,
    ) -> Result<OperationSnapshot, OperationError> {
        if observation_id.is_empty() || observation_id.len() > 128 || !observation_id.is_ascii() {
            return Err(OperationError::new(
                "invalid_observation_id",
                "observation_id must be a bounded ASCII identifier",
            ));
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        prune_locked(&mut inner, now_ms);
        inner
            .records
            .values()
            .find(|record| {
                record
                    .snapshot
                    .result
                    .as_ref()
                    .is_some_and(|result| result["observation_id"].as_str() == Some(observation_id))
            })
            .map(|record| record.snapshot.clone())
            .ok_or_else(|| {
                OperationError::new(
                    "observation_not_found",
                    "observation is unknown or its operation history expired",
                )
            })
    }

    /// Waits for a terminal transition or until the caller's bounded wait
    /// expires. The operation deadline is also an upper bound, but expiration
    /// is applied by the session wrapper so it can emit the corresponding
    /// operation.finished event exactly once.
    pub fn wait(
        &self,
        operation_id: &str,
        wait_ms: u64,
    ) -> Result<OperationSnapshot, OperationError> {
        validate_operation_id(operation_id)?;
        let wait_until = Instant::now() + Duration::from_millis(wait_ms);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        prune_locked(&mut inner, events::now_ms());
        loop {
            let record = inner.records.get(operation_id).ok_or_else(|| {
                OperationError::new("operation_expired", "operation is unknown or expired")
            })?;
            let snapshot = record.snapshot.clone();
            if snapshot.state.is_terminal() || wait_ms == 0 {
                return Ok(snapshot);
            }

            let now_ms = events::now_ms();
            let operation_remaining =
                Duration::from_millis(snapshot.deadline_at_ms.saturating_sub(now_ms));
            let wait_remaining = wait_until.saturating_duration_since(Instant::now());
            let remaining = operation_remaining.min(wait_remaining);
            if remaining.is_zero() {
                return Ok(snapshot);
            }
            inner = self
                .changed
                .wait_timeout(inner, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    pub fn expire_due(&self, now_ms: u64) -> Vec<OperationSnapshot> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let expired = expire_locked(&mut inner, now_ms);
        if !expired.is_empty() {
            self.changed.notify_all();
        }
        expired
    }

    pub fn start(&self, operation_id: &str, now_ms: u64) -> Result<Transition, OperationError> {
        validate_operation_id(operation_id)?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = expire_locked(&mut inner, now_ms);
        let record = inner.records.get_mut(operation_id).ok_or_else(|| {
            OperationError::new("operation_expired", "operation is unknown or expired")
        })?;
        if record.snapshot.state != OperationState::Queued {
            return Ok(Transition {
                snapshot: record.snapshot.clone(),
                changed: false,
            });
        }
        record.snapshot.state = OperationState::Running;
        record.snapshot.started_at_ms = Some(now_ms);
        let snapshot = record.snapshot.clone();
        refresh_bytes(&mut inner);
        self.changed.notify_all();
        Ok(Transition {
            snapshot,
            changed: true,
        })
    }

    pub fn bind_scope(
        &self,
        operation_id: &str,
        scope: Scope,
        now_ms: u64,
    ) -> Result<Transition, OperationError> {
        validate_operation_id(operation_id)?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = expire_locked(&mut inner, now_ms);
        let record = inner.records.get_mut(operation_id).ok_or_else(|| {
            OperationError::new("operation_expired", "operation is unknown or expired")
        })?;
        if record.snapshot.state != OperationState::Running {
            return Ok(Transition {
                snapshot: record.snapshot.clone(),
                changed: false,
            });
        }
        let unchanged = record.snapshot.scope.build_id == scope.build_id
            && record.snapshot.scope.run_id == scope.run_id
            && record.snapshot.scope.revision == scope.revision;
        if unchanged {
            return Ok(Transition {
                snapshot: record.snapshot.clone(),
                changed: false,
            });
        }
        record.snapshot.scope = scope;
        let snapshot = record.snapshot.clone();
        refresh_bytes(&mut inner);
        self.changed.notify_all();
        Ok(Transition {
            snapshot,
            changed: true,
        })
    }

    pub fn finish(
        &self,
        operation_id: &str,
        state: OperationState,
        result: Option<Value>,
        error: Option<OperationError>,
        now_ms: u64,
    ) -> Result<Transition, OperationError> {
        validate_operation_id(operation_id)?;
        if !matches!(
            state,
            OperationState::Succeeded
                | OperationState::Failed
                | OperationState::Superseded
                | OperationState::Unknown
        ) {
            return Err(OperationError::new(
                "invalid_transition",
                "finish requires a non-cancel terminal operation state",
            ));
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = expire_locked(&mut inner, now_ms);
        let record = inner.records.get_mut(operation_id).ok_or_else(|| {
            OperationError::new("operation_expired", "operation is unknown or expired")
        })?;
        if record.snapshot.state.is_terminal() {
            return Ok(Transition {
                snapshot: record.snapshot.clone(),
                changed: false,
            });
        }
        if record.snapshot.state != OperationState::Running {
            return Err(OperationError::new(
                "invalid_transition",
                "an operation must be running before it can finish",
            ));
        }
        if now_ms >= record.snapshot.deadline_at_ms {
            mark_terminal(
                record,
                OperationState::TimedOut,
                None,
                Some(OperationError::new(
                    "timed_out",
                    "operation deadline elapsed before completion",
                )),
                now_ms,
            );
        } else {
            mark_terminal(record, state, result, error, now_ms);
        }
        let snapshot = record.snapshot.clone();
        inner.terminal_order.push_back(operation_id.to_owned());
        refresh_bytes(&mut inner);
        self.changed.notify_all();
        Ok(Transition {
            snapshot,
            changed: true,
        })
    }

    pub fn cancel(&self, operation_id: &str, now_ms: u64) -> Result<Transition, OperationError> {
        validate_operation_id(operation_id)?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = expire_locked(&mut inner, now_ms);
        let record = inner.records.get_mut(operation_id).ok_or_else(|| {
            OperationError::new("operation_expired", "operation is unknown or expired")
        })?;
        if record.snapshot.state.is_terminal() {
            return Ok(Transition {
                snapshot: record.snapshot.clone(),
                changed: false,
            });
        }
        record.snapshot.cancel_requested = true;
        mark_terminal(
            record,
            OperationState::Cancelled,
            None,
            Some(OperationError::new("cancelled", "operation was cancelled")),
            now_ms,
        );
        let snapshot = record.snapshot.clone();
        inner.terminal_order.push_back(operation_id.to_owned());
        refresh_bytes(&mut inner);
        self.changed.notify_all();
        Ok(Transition {
            snapshot,
            changed: true,
        })
    }

    #[cfg(test)]
    fn retained_len(&self, now_ms: u64) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        prune_locked(&mut inner, now_ms);
        inner.records.len()
    }
}

fn mark_terminal(
    record: &mut Record,
    state: OperationState,
    result: Option<Value>,
    error: Option<OperationError>,
    now_ms: u64,
) {
    record.snapshot.state = state;
    record.snapshot.result = result;
    record.snapshot.error = error;
    record.snapshot.finished_at_ms = Some(now_ms);
}

fn validate_operation_id(operation_id: &str) -> Result<(), OperationError> {
    if operation_id.is_empty() || operation_id.len() > 128 || !operation_id.is_ascii() {
        return Err(OperationError::new(
            "invalid_operation_id",
            "operation_id must be non-empty ASCII and at most 128 bytes",
        ));
    }
    Ok(())
}

fn expire_locked(inner: &mut Inner, now_ms: u64) -> Vec<OperationSnapshot> {
    let expired = inner
        .records
        .iter()
        .filter(|(_, record)| {
            matches!(
                record.snapshot.state,
                OperationState::Queued | OperationState::Running
            ) && now_ms >= record.snapshot.deadline_at_ms
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let mut snapshots = Vec::new();
    for operation_id in expired {
        if let Some(record) = inner.records.get_mut(&operation_id) {
            mark_terminal(
                record,
                OperationState::TimedOut,
                None,
                Some(OperationError::new(
                    "timed_out",
                    "operation deadline elapsed",
                )),
                now_ms,
            );
            snapshots.push(record.snapshot.clone());
            inner.terminal_order.push_back(operation_id);
        }
    }
    refresh_bytes(inner);
    snapshots
}

fn prune_locked(inner: &mut Inner, now_ms: u64) {
    let _ = expire_locked(inner, now_ms);
    while let Some(operation_id) = inner.terminal_order.front().cloned() {
        let remove = inner.records.get(&operation_id).is_none_or(|record| {
            record
                .snapshot
                .finished_at_ms
                .is_some_and(|finished| now_ms.saturating_sub(finished) >= OPERATION_TTL_MS)
        });
        if !remove {
            break;
        }
        inner.terminal_order.pop_front();
        remove_record(inner, &operation_id);
    }
    refresh_bytes(inner);
    while inner.records.len() > MAX_RETAINED_OPERATIONS || inner.bytes > MAX_RETAINED_BYTES {
        let Some(operation_id) = inner.terminal_order.pop_front() else {
            break;
        };
        remove_record(inner, &operation_id);
        refresh_bytes(inner);
    }
    prune_request_tombstones(inner);
}

fn remove_record(inner: &mut Inner, operation_id: &str) {
    let _ = inner.records.remove(operation_id);
}

fn prune_request_tombstones(inner: &mut Inner) {
    let mut attempts = inner.request_order.len();
    while inner.requests.len() > MAX_RETAINED_OPERATIONS && attempts > 0 {
        attempts -= 1;
        let Some(request_id) = inner.request_order.pop_front() else {
            break;
        };
        let removable = inner
            .requests
            .get(&request_id)
            .is_some_and(|request| !inner.records.contains_key(&request.operation_id));
        if removable {
            inner.requests.remove(&request_id);
        } else {
            inner.request_order.push_back(request_id);
        }
    }
}

fn refresh_bytes(inner: &mut Inner) {
    inner.bytes = inner
        .records
        .values()
        .map(|record| snapshot_bytes(&record.snapshot))
        .sum();
}

fn snapshot_bytes(snapshot: &OperationSnapshot) -> usize {
    serde_json::to_vec(snapshot).map_or(MAX_RETAINED_BYTES, |bytes| bytes.len())
}

fn fingerprint(kind: &str, scope: &Scope, target: &Value) -> String {
    let canonical = serde_json::json!({
        "kind": kind,
        "scope": serde_json::to_value(scope).expect("serializable operation scope"),
        "target": canonicalize(target),
    });
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical).expect("serializable fingerprint"))
    )
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize(&values[key]));
            }
            Value::Object(canonical)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devserver::events::Revision;
    use std::sync::Arc;
    use std::thread;

    fn scope() -> Scope {
        Scope {
            build_id: Some("b1".into()),
            run_id: Some("r1".into()),
            revision: Revision {
                source_revision: 3,
                asset_revision: 4,
            },
        }
    }

    fn submit(store: &OperationStore, request_id: &str, now_ms: u64) -> OperationSnapshot {
        match store
            .submit(
                request_id,
                "observe",
                scope(),
                serde_json::json!({"window_id": "main"}),
                now_ms + 1_000,
                now_ms,
            )
            .unwrap()
        {
            SubmitResult::Created(snapshot) => snapshot,
            SubmitResult::Existing(_) => panic!("expected a new operation"),
        }
    }

    #[test]
    fn operation_lifecycle_is_terminal_and_deadline_bound() {
        let store = OperationStore::new(4);
        let queued = submit(&store, "req-1", 100);
        assert_eq!(queued.state, OperationState::Queued);
        let started = store.start(&queued.operation_id, 200).unwrap();
        assert!(started.changed);
        assert_eq!(started.snapshot.state, OperationState::Running);
        let finished = store
            .finish(
                &queued.operation_id,
                OperationState::Succeeded,
                Some(serde_json::json!({"observation_id": "obs-1"})),
                None,
                500,
            )
            .unwrap();
        assert!(finished.changed);
        assert_eq!(finished.snapshot.state, OperationState::Succeeded);
        let late = store
            .finish(
                &queued.operation_id,
                OperationState::Failed,
                None,
                Some(OperationError::new("late", "late result")),
                600,
            )
            .unwrap();
        assert!(!late.changed);
        assert_eq!(late.snapshot.state, OperationState::Succeeded);
    }

    #[test]
    fn idempotency_reuses_matching_request_and_rejects_conflicts() {
        let store = OperationStore::new(4);
        let first = submit(&store, "req-1", 100);
        let existing = store
            .submit(
                "req-1",
                "observe",
                scope(),
                serde_json::json!({"window_id": "main"}),
                1_100,
                100,
            )
            .unwrap();
        assert!(
            matches!(existing, SubmitResult::Existing(snapshot) if snapshot.operation_id == first.operation_id)
        );
        let conflict = store
            .submit(
                "req-1",
                "observe",
                scope(),
                serde_json::json!({"window_id": "other"}),
                1_100,
                100,
            )
            .unwrap_err();
        assert_eq!(conflict.code, "idempotency_conflict");
    }

    #[test]
    fn cancellation_wins_over_late_completion() {
        let store = OperationStore::new(4);
        let queued = submit(&store, "req-1", 100);
        store.start(&queued.operation_id, 200).unwrap();
        let cancelled = store.cancel(&queued.operation_id, 300).unwrap();
        assert!(cancelled.changed);
        assert_eq!(cancelled.snapshot.state, OperationState::Cancelled);
        let late = store
            .finish(
                &queued.operation_id,
                OperationState::Succeeded,
                None,
                None,
                400,
            )
            .unwrap();
        assert!(!late.changed);
        assert_eq!(late.snapshot.state, OperationState::Cancelled);
    }

    #[test]
    fn wait_returns_when_a_running_operation_reaches_a_terminal_state() {
        let store = Arc::new(OperationStore::new(4));
        let queued = submit(&store, "req-1", events::now_ms());
        store.start(&queued.operation_id, events::now_ms()).unwrap();
        let worker = store.clone();
        let operation_id = queued.operation_id.clone();
        let join = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            worker
                .finish(
                    &operation_id,
                    OperationState::Succeeded,
                    Some(serde_json::json!({"ok": true})),
                    None,
                    events::now_ms(),
                )
                .unwrap();
        });

        let snapshot = store.wait(&queued.operation_id, 1_000).unwrap();
        join.join().unwrap();
        assert_eq!(snapshot.state, OperationState::Succeeded);
        assert_eq!(snapshot.result.unwrap()["ok"], true);
    }

    #[test]
    fn running_operation_can_bind_to_the_actual_run_identity() {
        let store = OperationStore::new(4);
        let queued = submit(&store, "req-1", 100);
        store.start(&queued.operation_id, 200).unwrap();
        let mut actual_scope = scope();
        actual_scope.build_id = Some("b2".into());
        actual_scope.run_id = Some("r2".into());
        let bound = store
            .bind_scope(&queued.operation_id, actual_scope, 300)
            .unwrap();
        assert!(bound.changed);
        assert_eq!(bound.snapshot.scope.build_id.as_deref(), Some("b2"));
        assert_eq!(bound.snapshot.scope.run_id.as_deref(), Some("r2"));
        assert_eq!(bound.snapshot.scope.revision.source_revision, 3);
    }

    #[test]
    fn expired_terminal_records_are_removed_but_active_work_is_preserved() {
        let store = OperationStore::new(4);
        let first = submit(&store, "req-1", 100);
        store.start(&first.operation_id, 101).unwrap();
        store
            .finish(
                &first.operation_id,
                OperationState::Succeeded,
                None,
                None,
                102,
            )
            .unwrap();
        let active = match store
            .submit(
                "req-2",
                "observe",
                scope(),
                serde_json::json!({"window_id": "main"}),
                OPERATION_TTL_MS + 1_000,
                OPERATION_TTL_MS,
            )
            .unwrap()
        {
            SubmitResult::Created(snapshot) => snapshot,
            SubmitResult::Existing(_) => panic!("expected a new operation"),
        };
        let check_now = 102 + OPERATION_TTL_MS;
        assert_eq!(store.retained_len(check_now), 1);
        assert_eq!(
            store.get(&active.operation_id, check_now).unwrap().state,
            OperationState::Queued
        );
        let reused = store
            .submit(
                "req-1",
                "observe",
                scope(),
                serde_json::json!({"window_id": "main"}),
                check_now + 1_000,
                check_now,
            )
            .unwrap_err();
        assert_eq!(reused.code, "operation_expired");
    }
}
