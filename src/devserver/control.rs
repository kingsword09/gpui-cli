//! Authenticated read-only control connections, separate from app connections.

use super::events::{SCHEMA_VERSION, atomic_json, now_ms};
use super::protocol;
use super::session::{Session, random_token};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_CONNECTIONS: usize = 32;
pub const MAX_WAIT_MS: u64 = 30_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registration {
    pub schema_version: u32,
    pub session_id: String,
    pub project_root: PathBuf,
    pub target_id: String,
    pub supervisor_pid: u32,
    pub created_at_ms: u64,
    pub port: u16,
    pub token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    Ping,
    Status,
    Diagnostics,
    Events { after: u64, timeout_ms: u64 },
}

#[derive(Serialize, Deserialize)]
struct Request {
    schema_version: u32,
    role: String,
    session_id: String,
    token: String,
    #[serde(flatten)]
    command: Command,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub schema_version: u32,
    pub session_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl Reply {
    pub fn error(session: &str, error: ApiError) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            session_id: session.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    fn success(session: &str, result: Value) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            session_id: session.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }
}

pub struct ControlServer {
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    session: Arc<Session>,
    listener: Option<JoinHandle<()>>,
    pub registration: Registration,
}

impl ControlServer {
    pub fn start(session: Arc<Session>) -> Result<Self> {
        let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        socket.set_nonblocking(true)?;
        let state = session.store.state();
        let registration = Registration {
            schema_version: SCHEMA_VERSION,
            session_id: session.id.clone(),
            project_root: session.root.clone(),
            target_id: state.target_id,
            supervisor_pid: std::process::id(),
            created_at_ms: now_ms(),
            port: socket.local_addr()?.port(),
            token: random_token()?,
        };
        atomic_json(&session.dir.join("session.json"), &registration)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let auth = registration.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let workers = connections.clone();
        let owner = session.clone();
        let listener = thread::Builder::new()
            .name("gpui-control".into())
            .spawn(move || {
                let connections = workers;
                while !worker_stop.load(Ordering::SeqCst) {
                    match socket.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                            if connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                                let reply = Reply::error(
                                    &auth.session_id,
                                    ApiError::new("busy", "Too many control connections"),
                                );
                                let _ = send_reply(&mut stream, &reply);
                                continue;
                            }
                            connections.fetch_add(1, Ordering::SeqCst);
                            let count = connections.clone();
                            let session = session.clone();
                            let auth = auth.clone();
                            if thread::Builder::new()
                                .name("gpui-control-request".into())
                                .spawn(move || {
                                    let reply = handle(&mut stream, &auth, &session);
                                    let _ = send_reply(&mut stream, &reply);
                                    count.fetch_sub(1, Ordering::SeqCst);
                                })
                                .is_err()
                            {
                                connections.fetch_sub(1, Ordering::SeqCst);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20))
                        }
                        Err(_) => thread::sleep(Duration::from_millis(100)),
                    }
                }
            })?;
        Ok(Self {
            stop,
            connections,
            session: owner,
            listener: Some(listener),
            registration,
        })
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.session.end();
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.listener.take() {
            let _ = handle.join();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while self.connections.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn send_reply(stream: &mut TcpStream, reply: &Reply) -> Result<()> {
    protocol::write_frame(stream, &protocol::encode(reply)?)
}

fn handle(stream: &mut TcpStream, registration: &Registration, session: &Session) -> Reply {
    let request =
        protocol::read_frame(stream).and_then(|bytes| protocol::decode::<Request>(&bytes));
    let request = match request {
        Ok(request) => request,
        Err(_) => {
            return Reply::error(
                &session.id,
                ApiError::new("invalid_request", "Expected a framed control request"),
            );
        }
    };
    if request.role != "control"
        || request.token != registration.token
        || request.session_id != registration.session_id
    {
        return Reply::error(
            &session.id,
            ApiError::new(
                "unauthorized",
                "Invalid control credentials or session identity",
            ),
        );
    }
    if request.schema_version != SCHEMA_VERSION {
        return Reply::error(
            &session.id,
            ApiError::new("unsupported_version", "Unsupported control schema version"),
        );
    }
    let result = match request.command {
        Command::Ping => json!({"lifecycle": session.store.state().lifecycle}),
        Command::Status => session.store.state().status_json(),
        Command::Diagnostics => {
            let state = session.store.state();
            json!({"seq": state.seq, "build": state.build, "desired": state.desired,
                "diagnostics": state.diagnostics, "diagnostics_omitted": state.diagnostics_omitted,
                "runtime_issues": state.runtime_issues, "runtime_issues_omitted": state.runtime_issues_omitted,
                "storage_error": state.storage_error, "watcher_error": state.watcher_error})
        }
        Command::Events { after, timeout_ms } => {
            if timeout_ms > MAX_WAIT_MS || after > session.store.state().seq {
                return Reply::error(
                    &session.id,
                    ApiError::new(
                        "invalid_cursor_or_timeout",
                        "Cursor exceeds this session's seq or timeout exceeds 30s",
                    ),
                );
            }
            let page = session
                .store
                .events(after, Duration::from_millis(timeout_ms));
            serde_json::to_value(page).expect("serializable event page")
        }
    };
    Reply::success(&session.id, result)
}

pub fn request(registration: &Registration, command: Command) -> Result<Reply> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, registration.port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
    let timeout = match command {
        Command::Events { timeout_ms, .. } => timeout_ms,
        _ => 0,
    };
    stream.set_read_timeout(Some(Duration::from_millis(timeout + 3000)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let payload = Request {
        schema_version: SCHEMA_VERSION,
        role: "control".into(),
        token: registration.token.clone(),
        session_id: registration.session_id.clone(),
        command,
    };
    protocol::write_frame(&mut stream, &protocol::encode(&payload)?)?;
    let reply: Reply = protocol::decode(&protocol::read_frame(&mut stream)?)?;
    if reply.session_id != registration.session_id || reply.schema_version != SCHEMA_VERSION {
        bail!("control endpoint identity no longer matches the selected session");
    }
    Ok(reply)
}

/// Where a session keeps its journal, raw output and registration file.
pub fn session_dir(registration: &Registration) -> PathBuf {
    registration
        .project_root
        .join(".gpui/live")
        .join(&registration.session_id)
}

pub fn discover(
    root: &Path,
    selected: Option<&str>,
) -> std::result::Result<Registration, ApiError> {
    let root = root
        .canonicalize()
        .map_err(|e| ApiError::new("invalid_project", e.to_string()))?;
    let entries = fs::read_dir(root.join(".gpui/live")).map_err(|_| {
        ApiError::new(
            "no_live_session",
            "No live session. Start `gpui run --live` in this project.",
        )
    })?;
    let mut active = Vec::new();
    for entry in entries.flatten() {
        let Ok(bytes) = fs::read(entry.path().join("session.json")) else {
            continue;
        };
        let Ok(registration) = serde_json::from_slice::<Registration>(&bytes) else {
            continue;
        };
        if registration.project_root != root
            || selected.is_some_and(|id| id != registration.session_id)
        {
            continue;
        }
        if let Ok(reply) = request(&registration, Command::Ping)
            && reply.ok
            && reply
                .result
                .as_ref()
                .is_some_and(|v| v["lifecycle"] == "running")
        {
            active.push(registration);
        }
    }
    match active.len() {
        1 => Ok(active.remove(0)),
        0 => Err(ApiError::new(
            "session_unavailable",
            "No matching live supervisor responded; saved session files may belong to an ended process.",
        )),
        _ => Err(ApiError {
            code: "ambiguous_session".into(),
            message: "Multiple live sessions; select one with --session.".into(),
            details: Some(
                json!({"sessions": active.iter().map(|r| json!({"session_id": r.session_id, "target_id": r.target_id})).collect::<Vec<_>>()}),
            ),
        }),
    }
}

pub fn project_root() -> Result<PathBuf> {
    let current = std::env::current_dir().context("resolving project directory")?;
    current
        .ancestors()
        .find(|p| p.join("gpui.toml").is_file() && p.join("Cargo.toml").is_file())
        .map(Path::to_owned)
        .context("Run inside a project created by `gpui init`.")
}
