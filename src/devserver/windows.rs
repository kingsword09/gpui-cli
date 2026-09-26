//! Run-scoped window state and UI heartbeat scheduling.
//!
//! The app channel owns transport and decoding; this module owns the small
//! state machine that turns window registration and probe replies into a
//! bounded, queryable status. All deadlines use `Instant`, while emitted and
//! returned records use wall-clock milliseconds for persistence.

use super::events::{Scope, WindowSnapshot};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);
pub const PROBE_DEADLINE: Duration = Duration::from_secs(3);
pub const SEMANTICS_READ_DEADLINE: Duration = Duration::from_secs(5);
const MAX_PENDING_SEMANTICS_REQUESTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WindowKey {
    run_id: Option<String>,
    window_id: String,
}

#[derive(Clone, Debug)]
struct ProbeFlight {
    request_id: String,
    connection_id: u64,
    sent_at: Instant,
}

#[derive(Clone, Debug)]
struct TrackedWindow {
    snapshot: WindowSnapshot,
    connection_id: u64,
    last_sent: Option<Instant>,
    in_flight: Option<ProbeFlight>,
    semantics_in_flight: Option<SemanticsFlight>,
}

#[derive(Clone, Debug)]
struct SemanticsFlight {
    request_id: String,
    connection_id: u64,
    max_bytes: usize,
    expires_at: Instant,
}

#[derive(Default)]
struct Inner {
    windows: HashMap<WindowKey, TrackedWindow>,
    next_probe: u64,
    semantics_requests: VecDeque<SemanticsRequest>,
}

/// Shared registry owned by a live session and used by both app/control
/// workers. The lock is never held while doing network I/O.
#[derive(Default)]
pub struct WindowRegistry {
    inner: Mutex<Inner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeRequest {
    pub request_id: String,
    pub window_id: String,
    pub connection_id: u64,
}

pub struct WindowRegistration {
    pub window_id: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u32,
    pub foreground: bool,
    pub registered_at_ms: u64,
}

pub struct ProbeReply {
    pub connection_id: u64,
    pub request_id: String,
    pub window_id: String,
    pub responsive: bool,
    pub latency_ms: Option<u64>,
    pub received_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeTimeout {
    pub request_id: String,
    pub window_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub accepted: bool,
    pub reason: Option<&'static str>,
}

pub struct SceneCompletion {
    pub window_id: String,
    pub connection_id: u64,
    pub scene_epoch: u64,
    pub source_revision: u64,
    pub asset_revision: u64,
    pub presented_frame_id: Option<String>,
    pub completed_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneResult {
    pub accepted: bool,
    pub reason: Option<&'static str>,
}

pub const MAX_SEMANTICS_TREE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticsRequest {
    pub request_id: String,
    pub window_id: String,
    pub connection_id: u64,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticsReadReply {
    pub request_id: String,
    pub window_id: String,
    pub status: String,
    pub a11y_active: bool,
    pub tree_json: Option<String>,
    pub reason: Option<String>,
    pub captured_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticsResult {
    pub accepted: bool,
    pub reason: Option<&'static str>,
    pub reply: Option<SemanticsReadReply>,
}

#[derive(Default)]
pub struct HeartbeatTick {
    pub probes: Vec<ProbeRequest>,
    pub timeouts: Vec<ProbeTimeout>,
    pub semantics_timeouts: Vec<SemanticsRequest>,
}

impl WindowRegistry {
    /// Returns the active app-channel owner for one open window in the given
    /// run. Input delivery must stay on this connection; broadcasting an
    /// action could execute it more than once.
    pub fn owner_connection_id(&self, run_id: Option<&str>, window_id: &str) -> Option<u64> {
        let key = WindowKey {
            run_id: run_id.map(str::to_owned),
            window_id: window_id.to_owned(),
        };
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .windows
            .get(&key)
            .filter(|window| window.snapshot.lifecycle == "open")
            .map(|window| window.connection_id)
    }

    pub fn register(&self, scope: &Scope, connection_id: u64, registration: WindowRegistration) {
        let key = WindowKey {
            run_id: scope.run_id.clone(),
            window_id: registration.window_id.clone(),
        };
        let registered_window_id = registration.window_id.clone();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.windows.insert(
            key,
            TrackedWindow {
                snapshot: WindowSnapshot {
                    run_id: scope.run_id.clone(),
                    window_id: registration.window_id,
                    title: registration.title,
                    width: registration.width,
                    height: registration.height,
                    scale_milli: registration.scale_milli,
                    foreground: registration.foreground,
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
                    registered_at_ms: registration.registered_at_ms,
                },
                connection_id,
                last_sent: None,
                in_flight: None,
                semantics_in_flight: None,
            },
        );
        inner
            .semantics_requests
            .retain(|request| request.window_id != registered_window_id);
    }

    pub fn close(&self, scope: &Scope, window_id: &str, reason: Option<String>) {
        let key = WindowKey {
            run_id: scope.run_id.clone(),
            window_id: window_id.to_owned(),
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(window) = inner.windows.get_mut(&key) {
            window.snapshot.lifecycle = "closed".into();
            window.snapshot.ui = "unavailable".into();
            window.snapshot.reason = reason;
            window.in_flight = None;
            window.semantics_in_flight = None;
        }
    }

    /// Marks windows owned by a disconnected app connection as unknown. The
    /// window is not treated as closed: a process may reconnect after a
    /// transient channel failure.
    pub fn disconnect(&self, scope: &Scope, connection_id: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for window in inner.windows.values_mut().filter(|window| {
            window.connection_id == connection_id
                && window.snapshot.run_id == scope.run_id
                && window.snapshot.lifecycle == "open"
        }) {
            window.snapshot.ui = "unknown".into();
            window.snapshot.reason = Some("app_channel_disconnected".into());
            window.in_flight = None;
            window.semantics_in_flight = None;
        }
    }

    /// Queues one bounded semantics read for a registered window. The request
    /// is sent by the app-channel heartbeat worker, while the response is
    /// accepted only from the connection that owned the window at request
    /// time.
    pub fn request_semantics(
        &self,
        scope: &Scope,
        window_id: &str,
        request_id: String,
        max_bytes: usize,
    ) -> Result<SemanticsRequest, &'static str> {
        let key = WindowKey {
            run_id: scope.run_id.clone(),
            window_id: window_id.to_owned(),
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let active_semantics = inner
            .windows
            .values()
            .filter(|window| window.semantics_in_flight.is_some())
            .count();
        if inner.semantics_requests.len() + active_semantics >= MAX_PENDING_SEMANTICS_REQUESTS {
            return Err("semantics_busy");
        }
        let Some(window) = inner.windows.get_mut(&key) else {
            return Err("unknown_window");
        };
        if window.snapshot.lifecycle != "open" {
            return Err("window_closed");
        }
        if window.semantics_in_flight.is_some() {
            return Err("semantics_busy");
        }
        let request = SemanticsRequest {
            request_id,
            window_id: window_id.to_owned(),
            connection_id: window.connection_id,
            max_bytes: max_bytes.min(MAX_SEMANTICS_TREE_BYTES),
        };
        window.semantics_in_flight = Some(SemanticsFlight {
            request_id: request.request_id.clone(),
            connection_id: request.connection_id,
            max_bytes: request.max_bytes,
            expires_at: Instant::now() + SEMANTICS_READ_DEADLINE,
        });
        inner.semantics_requests.push_back(request.clone());
        Ok(request)
    }

    /// Takes requests for the current run. Requests left behind by an old run
    /// or a replaced app connection are discarded before they can be sent.
    pub fn take_semantics_requests(&self, run_id: Option<&str>) -> Vec<SemanticsRequest> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut requests = Vec::new();
        while let Some(request) = inner.semantics_requests.pop_front() {
            let key = WindowKey {
                run_id: run_id.map(str::to_owned),
                window_id: request.window_id.clone(),
            };
            let valid = inner.windows.get(&key).is_some_and(|window| {
                window.connection_id == request.connection_id
                    && window
                        .semantics_in_flight
                        .as_ref()
                        .is_some_and(|flight| flight.request_id == request.request_id)
            });
            if valid {
                requests.push(request);
            }
        }
        requests
    }

    pub fn record_semantics_result(
        &self,
        scope: &Scope,
        connection_id: u64,
        mut reply: SemanticsReadReply,
    ) -> SemanticsResult {
        let key = WindowKey {
            run_id: scope.run_id.clone(),
            window_id: reply.window_id.clone(),
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(window) = inner.windows.get_mut(&key) else {
            return SemanticsResult {
                accepted: false,
                reason: Some("unknown_window"),
                reply: None,
            };
        };
        let Some(flight) = &window.semantics_in_flight else {
            return SemanticsResult {
                accepted: false,
                reason: Some("late_semantics"),
                reply: None,
            };
        };
        if flight.request_id != reply.request_id || flight.connection_id != connection_id {
            return SemanticsResult {
                accepted: false,
                reason: Some("late_semantics"),
                reply: None,
            };
        }
        if window.snapshot.lifecycle != "open" {
            return SemanticsResult {
                accepted: false,
                reason: Some("window_closed"),
                reply: None,
            };
        }
        if reply
            .tree_json
            .as_ref()
            .is_some_and(|tree| tree.len() > flight.max_bytes)
        {
            reply.status = "too_large".into();
            reply.a11y_active = true;
            reply.tree_json = None;
            reply.reason = Some("tree_exceeds_request_limit".into());
        }
        if reply.status == "ready" && (!reply.a11y_active || reply.tree_json.is_none()) {
            reply.status = "unavailable".into();
            reply.reason = Some("ready_result_missing_tree".into());
            reply.tree_json = None;
        }
        window.semantics_in_flight = None;
        SemanticsResult {
            accepted: true,
            reason: None,
            reply: Some(reply),
        }
    }

    /// Converts a delivery failure into a terminal unavailable response so an
    /// observe operation cannot wait until its outer deadline for a request
    /// that was never delivered.
    pub fn fail_semantics_request(
        &self,
        scope: &Scope,
        request: &SemanticsRequest,
        reason: &str,
        captured_at_ms: u64,
    ) -> SemanticsResult {
        self.record_semantics_result(
            scope,
            request.connection_id,
            SemanticsReadReply {
                request_id: request.request_id.clone(),
                window_id: request.window_id.clone(),
                status: "unavailable".into(),
                a11y_active: false,
                tree_json: None,
                reason: Some(reason.into()),
                captured_at_ms,
            },
        )
    }

    pub fn record_probe_result(&self, scope: &Scope, reply: ProbeReply) -> ProbeResult {
        let key = WindowKey {
            run_id: scope.run_id.clone(),
            window_id: reply.window_id.clone(),
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(window) = inner.windows.get_mut(&key) else {
            return ProbeResult {
                accepted: false,
                reason: Some("unknown_window"),
            };
        };
        let Some(flight) = &window.in_flight else {
            return ProbeResult {
                accepted: false,
                reason: Some("late_probe"),
            };
        };
        if flight.request_id != reply.request_id || flight.connection_id != reply.connection_id {
            return ProbeResult {
                accepted: false,
                reason: Some("late_probe"),
            };
        }
        if window.snapshot.lifecycle != "open" {
            return ProbeResult {
                accepted: false,
                reason: Some("window_closed"),
            };
        }
        window.in_flight = None;
        window.snapshot.ui = if reply.responsive {
            "responsive"
        } else {
            "unresponsive"
        }
        .into();
        window.snapshot.reason = None;
        window.snapshot.last_probe_at_ms = Some(reply.received_at_ms);
        window.snapshot.last_latency_ms = reply.latency_ms;
        window.snapshot.last_probe_request_id = Some(reply.request_id);
        ProbeResult {
            accepted: true,
            reason: None,
        }
    }

    pub fn record_scene_completed(
        &self,
        scope: &Scope,
        completion: SceneCompletion,
    ) -> SceneResult {
        if completion.scene_epoch == 0 {
            return SceneResult {
                accepted: false,
                reason: Some("invalid_scene_epoch"),
            };
        }
        let key = WindowKey {
            run_id: scope.run_id.clone(),
            window_id: completion.window_id,
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(window) = inner.windows.get_mut(&key) else {
            return SceneResult {
                accepted: false,
                reason: Some("unknown_window"),
            };
        };
        if window.connection_id != completion.connection_id {
            return SceneResult {
                accepted: false,
                reason: Some("stale_connection"),
            };
        }
        if window.snapshot.lifecycle != "open" {
            return SceneResult {
                accepted: false,
                reason: Some("window_closed"),
            };
        }
        if completion.scene_epoch < window.snapshot.scene_epoch {
            return SceneResult {
                accepted: false,
                reason: Some("stale_scene_epoch"),
            };
        }
        if completion.scene_epoch == window.snapshot.scene_epoch {
            return SceneResult {
                accepted: false,
                reason: Some("duplicate_scene_epoch"),
            };
        }
        window.snapshot.scene_epoch = completion.scene_epoch;
        window.snapshot.scene_source_revision = Some(completion.source_revision);
        window.snapshot.scene_asset_revision = Some(completion.asset_revision);
        window.snapshot.presented_frame_id = completion.presented_frame_id;
        window.snapshot.scene_completed_at_ms = Some(completion.completed_at_ms);
        SceneResult {
            accepted: true,
            reason: None,
        }
    }

    /// Advances all open windows for one scheduler tick. A timeout is emitted
    /// once per request, then a later tick may schedule the next probe.
    pub fn tick(&self, run_id: Option<&str>, now: Instant) -> HeartbeatTick {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut tick = HeartbeatTick::default();
        let mut next_probe = inner.next_probe;
        for window in inner.windows.values_mut().filter(|window| {
            window.snapshot.lifecycle == "open"
                && window.snapshot.foreground
                && window.snapshot.run_id.as_deref() == run_id
        }) {
            if let Some(flight) = &window.semantics_in_flight
                && now >= flight.expires_at
            {
                tick.semantics_timeouts.push(SemanticsRequest {
                    request_id: flight.request_id.clone(),
                    window_id: window.snapshot.window_id.clone(),
                    connection_id: flight.connection_id,
                    max_bytes: flight.max_bytes,
                });
            }
            if let Some(flight) = &window.in_flight
                && now.duration_since(flight.sent_at) >= PROBE_DEADLINE
            {
                let flight = window.in_flight.take().expect("probe flight still exists");
                window.snapshot.ui = "unresponsive".into();
                window.snapshot.reason = Some("probe_timeout".into());
                tick.timeouts.push(ProbeTimeout {
                    request_id: flight.request_id,
                    window_id: window.snapshot.window_id.clone(),
                });
            }

            if window.in_flight.is_none()
                && window
                    .last_sent
                    .is_none_or(|sent| now.duration_since(sent) >= PROBE_INTERVAL)
            {
                next_probe = next_probe.saturating_add(1);
                let request_id = format!("heartbeat.{}.{}", std::process::id(), next_probe);
                window.last_sent = Some(now);
                window.in_flight = Some(ProbeFlight {
                    request_id: request_id.clone(),
                    connection_id: window.connection_id,
                    sent_at: now,
                });
                tick.probes.push(ProbeRequest {
                    request_id,
                    window_id: window.snapshot.window_id.clone(),
                    connection_id: window.connection_id,
                });
            }
        }
        inner.next_probe = next_probe;
        tick
    }

    pub fn snapshots(&self, run_id: Option<&str>) -> Vec<WindowSnapshot> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut snapshots: Vec<_> = inner
            .windows
            .values()
            .filter(|window| window.snapshot.run_id.as_deref() == run_id)
            .map(|window| window.snapshot.clone())
            .collect();
        snapshots.sort_by(|left, right| left.window_id.cmp(&right.window_id));
        snapshots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            run_id: Some("run-1".into()),
            ..Scope::default()
        }
    }

    #[test]
    fn one_window_has_one_in_flight_probe_and_accepts_matching_reply() {
        let registry = WindowRegistry::default();
        registry.register(
            &scope(),
            7,
            WindowRegistration {
                window_id: "main".into(),
                title: "Counter".into(),
                width: 800,
                height: 600,
                scale_milli: 1000,
                foreground: true,
                registered_at_ms: 10,
            },
        );
        let start = Instant::now();
        let first = registry.tick(Some("run-1"), start);
        assert_eq!(first.probes.len(), 1);
        assert!(
            registry
                .tick(Some("run-1"), start + Duration::from_millis(500))
                .probes
                .is_empty()
        );
        let result = registry.record_probe_result(
            &scope(),
            ProbeReply {
                connection_id: 7,
                request_id: first.probes[0].request_id.clone(),
                window_id: "main".into(),
                responsive: true,
                latency_ms: Some(2),
                received_at_ms: 20,
            },
        );
        assert_eq!(
            result,
            ProbeResult {
                accepted: true,
                reason: None
            }
        );
        assert_eq!(registry.snapshots(Some("run-1"))[0].ui, "responsive");
    }

    #[test]
    fn scene_completion_is_monotonic_and_connection_scoped() {
        let registry = WindowRegistry::default();
        registry.register(
            &scope(),
            7,
            WindowRegistration {
                window_id: "main".into(),
                title: "Counter".into(),
                width: 800,
                height: 600,
                scale_milli: 1000,
                foreground: true,
                registered_at_ms: 10,
            },
        );
        let first = registry.record_scene_completed(
            &scope(),
            SceneCompletion {
                window_id: "main".into(),
                connection_id: 7,
                scene_epoch: 3,
                source_revision: 8,
                asset_revision: 9,
                presented_frame_id: None,
                completed_at_ms: 20,
            },
        );
        assert_eq!(
            first,
            SceneResult {
                accepted: true,
                reason: None
            }
        );
        let duplicate = registry.record_scene_completed(
            &scope(),
            SceneCompletion {
                window_id: "main".into(),
                connection_id: 7,
                scene_epoch: 3,
                source_revision: 8,
                asset_revision: 9,
                presented_frame_id: None,
                completed_at_ms: 21,
            },
        );
        assert_eq!(duplicate.reason, Some("duplicate_scene_epoch"));
        let stale_connection = registry.record_scene_completed(
            &scope(),
            SceneCompletion {
                window_id: "main".into(),
                connection_id: 8,
                scene_epoch: 4,
                source_revision: 8,
                asset_revision: 9,
                presented_frame_id: Some("frame-4".into()),
                completed_at_ms: 22,
            },
        );
        assert_eq!(stale_connection.reason, Some("stale_connection"));
        let snapshot = registry.snapshots(Some("run-1")).pop().unwrap();
        assert_eq!(snapshot.scene_epoch, 3);
        assert_eq!(snapshot.scene_asset_revision, Some(9));
    }

    #[test]
    fn semantics_reply_is_bound_to_the_window_connection_and_size_limit() {
        let registry = WindowRegistry::default();
        registry.register(
            &scope(),
            7,
            WindowRegistration {
                window_id: "main".into(),
                title: "Counter".into(),
                width: 800,
                height: 600,
                scale_milli: 1000,
                foreground: true,
                registered_at_ms: 10,
            },
        );
        let request = registry
            .request_semantics(&scope(), "main", "op-1".into(), 16)
            .unwrap();
        assert_eq!(
            registry.take_semantics_requests(Some("run-1")),
            vec![request]
        );

        let stale = registry.record_semantics_result(
            &scope(),
            8,
            SemanticsReadReply {
                request_id: "op-1".into(),
                window_id: "main".into(),
                status: "ready".into(),
                a11y_active: true,
                tree_json: Some("{}".into()),
                reason: None,
                captured_at_ms: 20,
            },
        );
        assert_eq!(stale.reason, Some("late_semantics"));

        let accepted = registry.record_semantics_result(
            &scope(),
            7,
            SemanticsReadReply {
                request_id: "op-1".into(),
                window_id: "main".into(),
                status: "ready".into(),
                a11y_active: true,
                tree_json: Some("0123456789abcdefg".into()),
                reason: None,
                captured_at_ms: 21,
            },
        );
        assert_eq!(accepted.reason, None);
        let reply = accepted.reply.unwrap();
        assert_eq!(reply.status, "too_large");
        assert_eq!(reply.reason.as_deref(), Some("tree_exceeds_request_limit"));
        assert!(registry.take_semantics_requests(Some("run-1")).is_empty());
    }

    #[test]
    fn timed_out_and_late_probes_cannot_rewrite_current_state() {
        let registry = WindowRegistry::default();
        registry.register(
            &scope(),
            7,
            WindowRegistration {
                window_id: "main".into(),
                title: "Counter".into(),
                width: 800,
                height: 600,
                scale_milli: 1000,
                foreground: true,
                registered_at_ms: 10,
            },
        );
        let start = Instant::now();
        let first = registry.tick(Some("run-1"), start);
        let timeout = registry.tick(Some("run-1"), start + PROBE_DEADLINE);
        assert_eq!(timeout.timeouts[0].request_id, first.probes[0].request_id);
        assert_eq!(registry.snapshots(Some("run-1"))[0].ui, "unresponsive");
        let late = registry.record_probe_result(
            &scope(),
            ProbeReply {
                connection_id: 7,
                request_id: first.probes[0].request_id.clone(),
                window_id: "main".into(),
                responsive: true,
                latency_ms: Some(1),
                received_at_ms: 30,
            },
        );
        assert_eq!(late.reason, Some("late_probe"));
        assert_eq!(registry.snapshots(Some("run-1"))[0].ui, "unresponsive");
    }

    #[test]
    fn closing_a_window_stops_future_probes() {
        let registry = WindowRegistry::default();
        registry.register(
            &scope(),
            7,
            WindowRegistration {
                window_id: "main".into(),
                title: "Counter".into(),
                width: 800,
                height: 600,
                scale_milli: 1000,
                foreground: true,
                registered_at_ms: 10,
            },
        );
        registry.close(&scope(), "main", Some("user".into()));
        let tick = registry.tick(Some("run-1"), Instant::now());
        assert!(tick.probes.is_empty());
        assert_eq!(registry.snapshots(Some("run-1"))[0].lifecycle, "closed");
    }
}
