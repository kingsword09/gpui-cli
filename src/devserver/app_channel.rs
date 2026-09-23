//! App connections have launch-scoped credentials and bounded output queues.
//! Logs go directly to the event store; snapshot replies go to their requester.

use super::events::{Kind, Scope, clip, now_ms};
use super::protocol::{self, AssetManifestEntry, ClientMessage, PROTO_VERSION, ServerMessage};
use super::session::{Session, random_token};
use super::windows::{ProbeReply, WindowRegistration, WindowRegistry};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashMap;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct ClientConn {
    sender: mpsc::SyncSender<Vec<u8>>,
    socket: TcpStream,
    scope: Scope,
    asset_reload: bool,
}

struct ExpectedRun {
    token: String,
    scope: Scope,
}
type SnapshotKey = (Option<String>, String);

#[derive(Clone, Debug)]
pub struct AssetReconciliation {
    pub connection_id: u64,
    pub scope: Scope,
    pub transfer_id: String,
    pub asset_revision: u64,
    pub present: Vec<String>,
    pub missing: Vec<String>,
    pub stale: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Clone, Debug)]
struct AssetManifestSnapshot {
    transfer_id: String,
    asset_revision: u64,
    entries: Vec<AssetManifestEntry>,
}

struct Shared {
    token: String,
    expected: Mutex<Option<ExpectedRun>>,
    session: Option<Arc<Session>>,
    clients: Mutex<HashMap<u64, ClientConn>>,
    changed: Condvar,
    waiters: Mutex<HashMap<SnapshotKey, mpsc::SyncSender<String>>>,
    asset_manifest: Mutex<Option<AssetManifestSnapshot>>,
    reconciliations: Mutex<Vec<AssetReconciliation>>,
    windows: Arc<WindowRegistry>,
    next_id: AtomicU64,
    next_transfer: AtomicU64,
    connections: AtomicUsize,
    shutdown: AtomicBool,
    write_failed: AtomicBool,
}

impl Shared {
    fn current_run(&self) -> Option<String> {
        self.expected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|r| r.scope.run_id.clone())
    }

    fn current_scope(&self) -> Option<Scope> {
        self.expected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|run| run.scope.clone())
    }

    fn emit(&self, kind: Kind, scope: &Scope, data: serde_json::Value) {
        if let Some(session) = &self.session {
            session.emit(kind, scope, data);
        }
    }

    fn send_to(&self, connection_id: u64, message: &ServerMessage) -> bool {
        let Ok(payload) = protocol::encode(message) else {
            return false;
        };
        if payload.len() > protocol::MAX_FRAME_LEN as usize {
            self.write_failed.store(true, Ordering::SeqCst);
            return false;
        }
        let clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let Some(client) = clients.get(&connection_id) else {
            return false;
        };
        if client.sender.try_send(payload).is_err() {
            self.write_failed.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }
}

pub struct DevServer {
    pub port: u16,
    pub token: String,
    shared: Arc<Shared>,
    accept_handle: Option<JoinHandle<()>>,
    heartbeat_handle: Option<JoinHandle<()>>,
}

impl DevServer {
    pub fn start() -> Result<Self> {
        Self::start_inner(None)
    }
    pub fn start_observed(session: Arc<Session>) -> Result<Self> {
        Self::start_inner(Some(session))
    }

    fn start_inner(session: Option<Arc<Session>>) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding live app channel")?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let token = random_token()?;
        let windows = session
            .as_ref()
            .map(|session| session.windows.clone())
            .unwrap_or_default();
        let shared = Arc::new(Shared {
            token: token.clone(),
            expected: Mutex::new(None),
            session,
            clients: Mutex::new(HashMap::new()),
            changed: Condvar::new(),
            waiters: Mutex::new(HashMap::new()),
            asset_manifest: Mutex::new(None),
            reconciliations: Mutex::new(Vec::new()),
            windows,
            next_id: AtomicU64::new(0),
            next_transfer: AtomicU64::new(1),
            connections: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
            write_failed: AtomicBool::new(false),
        });
        let state = shared.clone();
        let accept_handle =
            thread::Builder::new()
                .name("gpui-devserver".into())
                .spawn(move || {
                    while !state.shutdown.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if state.connections.load(Ordering::SeqCst) >= 16 {
                                    continue;
                                }
                                state.connections.fetch_add(1, Ordering::SeqCst);
                                let connection = state.clone();
                                if thread::Builder::new()
                                    .name("gpui-devconn".into())
                                    .spawn(move || {
                                        handle_connection(stream, &connection);
                                        connection.connections.fetch_sub(1, Ordering::SeqCst);
                                    })
                                    .is_err()
                                {
                                    state.connections.fetch_sub(1, Ordering::SeqCst);
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(20))
                            }
                            Err(_) => thread::sleep(Duration::from_millis(100)),
                        }
                    }
                })?;
        let heartbeat_state = shared.clone();
        let heartbeat_handle = thread::Builder::new()
            .name("gpui-ui-heartbeat".into())
            .spawn(move || heartbeat_loop(&heartbeat_state))?;
        Ok(Self {
            port,
            token,
            shared,
            accept_handle: Some(accept_handle),
            heartbeat_handle: Some(heartbeat_handle),
        })
    }

    /// Rotating the token binds each current app launch to an exact run.
    pub fn expect_run(&self, scope: Scope) -> Result<String> {
        let token = random_token()?;
        *self
            .shared
            .expected
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(ExpectedRun {
            token: token.clone(),
            scope,
        });
        self.shared.changed.notify_all();
        Ok(token)
    }

    /// Sends only to the current run. Late connections from old runs cannot
    /// receive new assets or answer a new process's snapshot request.
    pub fn broadcast(&self, message: &ServerMessage) {
        let Ok(payload) = protocol::encode(message) else {
            return;
        };
        if payload.len() > protocol::MAX_FRAME_LEN as usize {
            self.shared.write_failed.store(true, Ordering::SeqCst);
            return;
        }
        let run = self.shared.current_run();
        let clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for conn in clients.values().filter(|c| c.scope.run_id == run) {
            if conn.sender.try_send(payload.clone()).is_err() {
                self.shared.write_failed.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Publishes the current content manifest. New connections receive it
    /// immediately and answer with a bounded reconciliation instead of making
    /// the supervisor assume that a previous transfer reached the app.
    pub fn set_asset_manifest(
        &self,
        asset_revision: u64,
        entries: Vec<AssetManifestEntry>,
    ) -> bool {
        let transfer_id = format!(
            "asset-t{}-r{asset_revision}",
            self.shared.next_transfer.fetch_add(1, Ordering::Relaxed)
        );
        let message = ServerMessage::AssetManifest {
            transfer_id: transfer_id.clone(),
            asset_revision,
            entries: entries.clone(),
        };
        let Ok(payload) = protocol::encode(&message) else {
            return false;
        };
        if payload.len() > protocol::MAX_FRAME_LEN as usize {
            self.shared.write_failed.store(true, Ordering::SeqCst);
            return false;
        }
        *self
            .shared
            .asset_manifest
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(AssetManifestSnapshot {
            transfer_id,
            asset_revision,
            entries,
        });
        true
    }

    pub fn current_asset_manifest(&self) -> Option<(String, u64, Vec<AssetManifestEntry>)> {
        self.shared
            .asset_manifest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|manifest| {
                (
                    manifest.transfer_id.clone(),
                    manifest.asset_revision,
                    manifest.entries.clone(),
                )
            })
    }

    pub fn send_to(&self, connection_id: u64, message: &ServerMessage) -> bool {
        self.shared.send_to(connection_id, message)
    }

    pub fn take_asset_reconciliations(&self) -> Vec<AssetReconciliation> {
        std::mem::take(
            &mut *self
                .shared
                .reconciliations
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    pub fn wait_for_client(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let run = self.shared.current_run();
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        loop {
            if clients.values().any(|c| c.scope.run_id == run) {
                return true;
            }
            if self.shared.shutdown.load(Ordering::SeqCst)
                || self
                    .shared
                    .session
                    .as_ref()
                    .is_some_and(|s| s.stopping.load(Ordering::SeqCst))
            {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            clients = self
                .shared
                .changed
                .wait_timeout(clients, remaining.min(Duration::from_millis(100)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    pub fn save_state(&self, request_id: &str, timeout: Duration) -> Option<String> {
        let key = (self.shared.current_run(), request_id.to_owned());
        let (tx, rx) = mpsc::sync_channel(1);
        {
            let mut waiters = self.shared.waiters.lock().ok()?;
            if waiters.len() >= 8 || waiters.contains_key(&key) {
                return None;
            }
            waiters.insert(key.clone(), tx);
        }
        self.broadcast(&ServerMessage::PrepareRestart {
            session: request_id.into(),
        });
        let result = rx.recv_timeout(timeout).ok();
        if let Ok(mut waiters) = self.shared.waiters.lock() {
            waiters.remove(&key);
        }
        result
    }

    pub fn has_clients(&self) -> bool {
        self.wait_for_client(Duration::ZERO)
    }

    pub fn all_clients_support_asset_reload(&self) -> bool {
        let run = self.shared.current_run();
        let clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let current: Vec<_> = clients.values().filter(|c| c.scope.run_id == run).collect();
        !current.is_empty() && current.iter().all(|c| c.asset_reload)
    }

    pub fn take_write_error(&self) -> bool {
        self.shared.write_failed.swap(false, Ordering::SeqCst)
    }

    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for conn in clients.values() {
            let _ = conn.socket.shutdown(Shutdown::Both);
        }
        clients.clear();
        self.shared
            .waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.shared.changed.notify_all();
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.accept_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.heartbeat_handle.take() {
            let _ = handle.join();
        }
    }
}

fn heartbeat_loop(shared: &Arc<Shared>) {
    while !shared.shutdown.load(Ordering::SeqCst) {
        if let Some(scope) = shared.current_scope() {
            let tick = shared.windows.tick(scope.run_id.as_deref(), Instant::now());
            for timeout in tick.timeouts {
                shared.emit(
                    Kind::UiProbeResult,
                    &scope,
                    json!({
                        "request_id": timeout.request_id,
                        "window_id": timeout.window_id,
                        "responsive": false,
                        "latency_ms": null,
                        "accepted": true,
                        "timeout": true,
                        "reason": "probe_timeout",
                        "received_at_ms": now_ms(),
                    }),
                );
            }
            for probe in tick.probes {
                let _ = shared.send_to(
                    probe.connection_id,
                    &ServerMessage::ProbeUi {
                        request_id: probe.request_id,
                        window_id: probe.window_id,
                    },
                );
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn handle_connection(mut stream: TcpStream, shared: &Arc<Shared>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let hello = protocol::read_frame(&mut stream)
        .and_then(|payload| protocol::decode::<ClientMessage>(&payload));
    let Ok(ClientMessage::Hello {
        proto,
        token,
        project,
        pid,
        platform,
        asset_reload,
        runtime_version,
        gpui_version,
        capabilities,
    }) = hello
    else {
        return;
    };
    let scope = {
        let expected = shared.expected.lock().unwrap_or_else(|e| e.into_inner());
        match expected.as_ref() {
            Some(run)
                if token == run.token
                    && shared
                        .session
                        .as_ref()
                        .is_none_or(|s| s.store.state().project == project) =>
            {
                run.scope.clone()
            }
            None if shared.session.is_none() && token == shared.token => Scope::default(),
            _ => return,
        }
    };
    if proto != PROTO_VERSION {
        let _ = protocol::encode(&ServerMessage::HelloError {
            code: "unsupported_version".to_owned(),
            message: format!("app protocol {proto} is not supported by this supervisor"),
            current_proto: PROTO_VERSION,
        })
        .and_then(|payload| protocol::write_frame(&mut stream, &payload));
        return;
    }
    let connect_span = shared.session.as_ref().map(|session| {
        session.start_span(
            "app.connect",
            &scope,
            None,
            json!({"project": project, "platform": platform, "pid": pid}),
        )
    });
    if protocol::write_frame(
        &mut stream,
        &protocol::encode(&ServerMessage::HelloOk {
            proto: PROTO_VERSION,
        })
        .unwrap_or_default(),
    )
    .is_err()
    {
        if let Some(span) = &connect_span {
            span.finish("failed", Some("writing hello_ok failed"));
        }
        return;
    }
    let _ = stream.set_read_timeout(None);
    let Ok(mut reader) = stream.try_clone() else {
        return;
    };
    let Ok(socket) = stream.try_clone() else {
        return;
    };
    let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(16);
    shared
        .clients
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            id,
            ClientConn {
                sender: tx,
                socket,
                scope: scope.clone(),
                asset_reload,
            },
        );
    shared.emit(
        Kind::AppConnected,
        &scope,
        json!({"project": project, "platform": platform, "pid": pid,
        "asset_reload": asset_reload, "connection_id": id,
        "runtime_version": runtime_version.as_deref().map(|value| clip(value, 128)),
        "gpui_version": gpui_version.as_deref().map(|value| clip(value, 64)),
        "capabilities": capabilities.iter().take(16).map(|value| clip(value, 64)).collect::<Vec<_>>()}),
    );
    if let Some(span) = &connect_span {
        span.finish("ok", None);
    }
    shared.changed.notify_all();
    let writer_state = shared.clone();
    let writer = thread::Builder::new()
        .name("gpui-devwrite".into())
        .spawn(move || {
            for payload in rx {
                if protocol::write_frame(&mut stream, &payload).is_err() {
                    writer_state.write_failed.store(true, Ordering::SeqCst);
                    let _ = stream.shutdown(Shutdown::Both);
                    break;
                }
            }
        });
    if let Some(manifest) = shared
        .asset_manifest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        let _ = shared.send_to(
            id,
            &ServerMessage::AssetManifest {
                transfer_id: manifest.transfer_id,
                asset_revision: manifest.asset_revision,
                entries: manifest.entries,
            },
        );
    }
    if writer.is_ok() {
        while !shared.shutdown.load(Ordering::SeqCst) {
            let Ok(frame) = protocol::read_frame(&mut reader) else {
                break;
            };
            let Ok(message) = protocol::decode::<ClientMessage>(&frame) else {
                continue;
            };
            match message {
                ClientMessage::Log {
                    level,
                    target,
                    message,
                } => {
                    let raw = json!({"level": level, "target": target, "message": message});
                    let reference = shared
                        .session
                        .as_ref()
                        .and_then(|s| s.record(&scope, "app.log", &raw));
                    shared.emit(Kind::AppLog, &scope, json!({"level": clip(&level, 32), "target": clip(&target, 512),
                        "message": clip(&message, 8192), "truncated": message.len() > 8192, "log": reference}));
                    super::output::terminal(
                        format!(
                            "[app {level}] {}: {}\n",
                            clip(&target, 512),
                            clip(&message, 8192)
                        )
                        .as_bytes(),
                        true,
                    );
                }
                ClientMessage::Panic {
                    message,
                    location,
                    backtrace,
                } => {
                    let raw =
                        json!({"message": message, "location": location, "backtrace": backtrace});
                    let reference = shared
                        .session
                        .as_ref()
                        .and_then(|s| s.record(&scope, "app.panic", &raw));
                    shared.emit(
                        Kind::AppPanic,
                        &scope,
                        json!({"message": clip(&message, 8192), "location": clip(&location, 1024),
                        "backtrace": clip(&backtrace, 16384), "log": reference}),
                    );
                    super::output::terminal(
                        format!(
                            "[live] app panicked: {} ({})\n{}\n",
                            clip(&message, 8192),
                            clip(&location, 1024),
                            clip(&backtrace, 16384)
                        )
                        .as_bytes(),
                        true,
                    );
                }
                ClientMessage::StateSaved { session, data } => {
                    if let Some(waiter) = shared
                        .waiters
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&(scope.run_id.clone(), session))
                    {
                        let _ = waiter.try_send(data);
                    }
                }
                ClientMessage::WindowRegistered {
                    window_id,
                    title,
                    width,
                    height,
                    scale_milli,
                    foreground,
                } => {
                    let window_id = clip(&window_id, 128);
                    let title = clip(&title, 256);
                    let registered_at_ms = now_ms();
                    shared.windows.register(
                        &scope,
                        id,
                        WindowRegistration {
                            window_id: window_id.clone(),
                            title: title.clone(),
                            width,
                            height,
                            scale_milli,
                            foreground,
                            registered_at_ms,
                        },
                    );
                    shared.emit(
                        Kind::WindowRegistered,
                        &scope,
                        json!({"window_id": window_id, "title": title,
                            "width": width, "height": height, "scale_milli": scale_milli,
                            "foreground": foreground, "registered_at_ms": registered_at_ms}),
                    );
                }
                ClientMessage::WindowClosed { window_id, reason } => {
                    let window_id = clip(&window_id, 128);
                    let reason = reason.map(|value| clip(&value, 256));
                    shared.windows.close(&scope, &window_id, reason.clone());
                    shared.emit(
                        Kind::WindowClosed,
                        &scope,
                        json!({"window_id": window_id, "reason": reason}),
                    );
                }
                ClientMessage::UiProbeResult {
                    request_id,
                    window_id,
                    responsive,
                    latency_ms,
                } => {
                    let received_at_ms = now_ms();
                    let result = shared.windows.record_probe_result(
                        &scope,
                        ProbeReply {
                            connection_id: id,
                            request_id: request_id.clone(),
                            window_id: window_id.clone(),
                            responsive,
                            latency_ms,
                            received_at_ms,
                        },
                    );
                    shared.emit(
                        Kind::UiProbeResult,
                        &scope,
                        json!({"request_id": clip(&request_id, 128),
                            "window_id": clip(&window_id, 128), "responsive": responsive,
                            "latency_ms": latency_ms, "accepted": result.accepted,
                            "reason": result.reason, "received_at_ms": received_at_ms}),
                    );
                }
                ClientMessage::AssetsApplied {
                    transfer_id,
                    asset_revision,
                    applied,
                    failed,
                    cache_invalidated,
                } => {
                    let accepted = shared
                        .asset_manifest
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                        .map(|manifest| {
                            transfer_id == manifest.transfer_id
                                && asset_revision == manifest.asset_revision
                        })
                        .unwrap_or_else(|| {
                            shared.session.as_ref().is_none_or(|session| {
                                asset_revision == session.store.state().desired.asset_revision
                            })
                        });
                    shared.emit(
                        Kind::AssetsApplied,
                        &scope,
                        json!({
                            "transfer_id": transfer_id,
                            "asset_revision": asset_revision,
                            "applied": applied.iter().take(128).map(|path| clip(path, 256)).collect::<Vec<_>>(),
                            "failed": failed.iter().take(128).map(|path| clip(path, 256)).collect::<Vec<_>>(),
                            "cache_invalidated": cache_invalidated,
                            "accepted": accepted,
                            "received_at_ms": now_ms(),
                        }),
                    );
                }
                ClientMessage::AssetsReceived {
                    transfer_id,
                    asset_revision,
                    received,
                    failed,
                } => {
                    let accepted = shared
                        .asset_manifest
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                        .map(|manifest| {
                            transfer_id == manifest.transfer_id
                                && asset_revision == manifest.asset_revision
                        })
                        .unwrap_or_else(|| {
                            shared.session.as_ref().is_none_or(|session| {
                                asset_revision == session.store.state().desired.asset_revision
                            })
                        });
                    shared.emit(
                        Kind::AssetsReceived,
                        &scope,
                        json!({
                            "transfer_id": transfer_id,
                            "asset_revision": asset_revision,
                            "received": received.iter().take(256).map(|path| clip(path, 256)).collect::<Vec<_>>(),
                            "failed": failed.iter().take(256).map(|path| clip(path, 256)).collect::<Vec<_>>(),
                            "accepted": accepted,
                            "connection_id": id,
                            "received_at_ms": now_ms(),
                        }),
                    );
                }
                ClientMessage::AssetsReconciled {
                    transfer_id,
                    asset_revision,
                    present,
                    missing,
                    stale,
                    removed,
                } => {
                    let accepted = asset_reload
                        && shared
                            .asset_manifest
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                            .is_some_and(|manifest| {
                                transfer_id == manifest.transfer_id
                                    && asset_revision == manifest.asset_revision
                            });
                    let present: Vec<String> = present
                        .iter()
                        .take(256)
                        .map(|path| clip(path, 256))
                        .collect();
                    let missing: Vec<String> = missing
                        .iter()
                        .take(256)
                        .map(|path| clip(path, 256))
                        .collect();
                    let stale: Vec<String> =
                        stale.iter().take(256).map(|path| clip(path, 256)).collect();
                    let removed: Vec<String> = removed
                        .iter()
                        .take(256)
                        .map(|path| clip(path, 256))
                        .collect();
                    shared.emit(
                        Kind::AssetsReconciled,
                        &scope,
                        json!({
                            "transfer_id": transfer_id,
                            "asset_revision": asset_revision,
                            "present": present,
                            "missing": missing,
                            "stale": stale,
                            "removed": removed,
                            "accepted": accepted,
                            "connection_id": id,
                            "received_at_ms": now_ms(),
                        }),
                    );
                    if accepted {
                        shared
                            .reconciliations
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(AssetReconciliation {
                                connection_id: id,
                                scope: scope.clone(),
                                transfer_id,
                                asset_revision,
                                present,
                                missing,
                                stale,
                                removed,
                            });
                        shared.changed.notify_all();
                    }
                }
                ClientMessage::Hello { .. } => {}
            }
        }
    }
    let mut clients = shared.clients.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(conn) = clients.remove(&id) {
        let _ = conn.socket.shutdown(Shutdown::Both);
    }
    let disconnected = !clients.values().any(|c| c.scope.run_id == scope.run_id);
    drop(clients);
    shared.windows.disconnect(&scope, id);
    if disconnected {
        shared.emit(Kind::AppDisconnected, &scope, json!({"connection_id": id}));
    }
    shared.changed.notify_all();
    if let Ok(writer) = writer {
        let _ = writer.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn connect(server: &DevServer, token: &str) -> (std::net::TcpStream, Vec<u8>) {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let hello = protocol::encode(&ClientMessage::Hello {
            proto: PROTO_VERSION,
            token: token.to_string(),
            project: "test".to_string(),
            pid: 1,
            platform: "test".to_string(),
            asset_reload: true,
            runtime_version: None,
            gpui_version: None,
            capabilities: Vec::new(),
        })
        .unwrap();
        protocol::write_frame(&mut stream, &hello).unwrap();
        let mut reply = Vec::new();
        // Read exactly one frame.
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_be_bytes(len_bytes) as usize;
        reply.resize(len, 0);
        stream.read_exact(&mut reply).unwrap();
        (stream, reply)
    }

    #[test]
    fn handshake_accepts_the_right_token_and_rejects_wrong_ones() {
        let server = DevServer::start().unwrap();

        // Wrong token: server closes without a hello_ok.
        let mut bad = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        let hello = protocol::encode(&ClientMessage::Hello {
            proto: PROTO_VERSION,
            token: "nope".to_string(),
            project: "test".to_string(),
            pid: 1,
            platform: "test".to_string(),
            asset_reload: false,
            runtime_version: None,
            gpui_version: None,
            capabilities: Vec::new(),
        })
        .unwrap();
        protocol::write_frame(&mut bad, &hello).unwrap();
        let mut probe = [0u8; 1];
        assert_eq!(bad.read(&mut probe).unwrap(), 0);

        // Right token: hello_ok comes back.
        let (stream, reply) = connect(&server, &server.token);
        let ok: ServerMessage = protocol::decode(&reply).unwrap();
        assert!(matches!(ok, ServerMessage::HelloOk { proto: 2 }));
        drop(stream);
    }

    #[test]
    fn valid_token_with_unknown_protocol_gets_an_upgrade_error() {
        let server = DevServer::start().unwrap();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let hello = protocol::encode(&ClientMessage::Hello {
            proto: PROTO_VERSION + 1,
            token: server.token.clone(),
            project: "test".to_string(),
            pid: 1,
            platform: "test".to_string(),
            asset_reload: false,
            runtime_version: None,
            gpui_version: None,
            capabilities: Vec::new(),
        })
        .unwrap();
        protocol::write_frame(&mut stream, &hello).unwrap();

        let reply = protocol::read_frame(&mut stream).unwrap();
        let error: ServerMessage = protocol::decode(&reply).unwrap();
        assert!(matches!(
            error,
            ServerMessage::HelloError {
                code,
                current_proto,
                ..
            } if code == "unsupported_version" && current_proto == PROTO_VERSION
        ));
    }

    #[test]
    fn broadcast_reaches_connected_apps() {
        let server = DevServer::start().unwrap();
        let (mut stream, _reply) = connect(&server, &server.token);
        // Let the server finish registering the connection.
        std::thread::sleep(Duration::from_millis(100));

        server.broadcast(&ServerMessage::AssetChanged {
            transfer_id: "t1".into(),
            path: "assets/x.png".to_string(),
            asset_revision: 1,
        });

        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_be_bytes(len_bytes) as usize;
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).unwrap();
        let message: ServerMessage = protocol::decode(&body).unwrap();
        assert!(
            matches!(message, ServerMessage::AssetChanged { path, .. } if path == "assets/x.png")
        );
    }

    #[test]
    fn new_connections_receive_the_current_asset_manifest() {
        let server = DevServer::start().unwrap();
        assert!(server.set_asset_manifest(
            4,
            vec![protocol::AssetManifestEntry {
                path: "assets/logo.png".into(),
                hash: "hash-4".into(),
            }]
        ));
        let (mut stream, reply) = connect(&server, &server.token);
        let hello: ServerMessage = protocol::decode(&reply).unwrap();
        assert!(matches!(hello, ServerMessage::HelloOk { .. }));
        let manifest: ServerMessage =
            protocol::decode(&protocol::read_frame(&mut stream).unwrap()).unwrap();
        assert!(matches!(
            manifest,
            ServerMessage::AssetManifest {
                transfer_id,
                asset_revision: 4,
                entries
            } if entries == vec![protocol::AssetManifestEntry {
                path: "assets/logo.png".into(),
                hash: "hash-4".into(),
            }] && transfer_id.starts_with("asset-t")
        ));
    }

    #[test]
    fn hello_ready_implies_the_client_is_registered() {
        let server = DevServer::start().unwrap();
        let (mut stream, _reply) = connect(&server, &server.token);
        // The hello is the live loop's signal to broadcast (e.g. replaying
        // assets); no settle sleep here — the broadcast that answers the
        // hello must reach the connection it was announced for.
        assert!(server.wait_for_client(Duration::from_secs(1)));

        server.broadcast(&ServerMessage::AssetChanged {
            transfer_id: "t1".into(),
            path: "assets/x.png".to_string(),
            asset_revision: 1,
        });
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_be_bytes(len_bytes) as usize;
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).unwrap();
        let message: ServerMessage = protocol::decode(&body).unwrap();
        assert!(matches!(message, ServerMessage::AssetChanged { .. }));
    }

    #[test]
    fn write_failure_tears_down_the_connection_and_is_reported() {
        let server = DevServer::start().unwrap();
        // Handshake but never read a byte: the hello_ok sits unread, and
        // closing a socket with unread receive data forces an RST.
        let mut raw = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        let hello = protocol::encode(&ClientMessage::Hello {
            proto: PROTO_VERSION,
            token: server.token.clone(),
            project: "test".to_string(),
            pid: 1,
            platform: "test".to_string(),
            asset_reload: true,
            runtime_version: None,
            gpui_version: None,
            capabilities: Vec::new(),
        })
        .unwrap();
        protocol::write_frame(&mut raw, &hello).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        // Queue far more than the socket buffers can take so the writer is
        // still mid-write when the RST lands — the failure must happen on
        // the write path, not the reader noticing the reset first.
        for _ in 0..12 {
            server.broadcast(&ServerMessage::AssetData {
                transfer_id: "t1".into(),
                path: "assets/x.png".to_string(),
                data: "x".repeat(256 * 1024),
                asset_revision: 1,
            });
        }
        std::thread::sleep(Duration::from_millis(100));
        drop(raw);

        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(100));
            if !server.has_clients() {
                break;
            }
        }
        assert!(!server.has_clients());
        assert!(server.take_write_error());
        assert!(!server.take_write_error()); // reported exactly once per failure
    }
}
