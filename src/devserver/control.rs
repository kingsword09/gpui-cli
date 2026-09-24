//! Authenticated read-only control connections, separate from app connections.

use super::artifacts::{ArtifactError, ArtifactErrorCode};
use super::events::{atomic_json, now_ms};
use super::protocol;
use super::session::{Session, random_token};
use anyhow::{Context, Result, bail};
use gpui_dev_protocol::{CONTROL_SCHEMA_VERSION, V2Envelope, V2Error, valid_request_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    Windows,
    Build,
    Events {
        after: u64,
        timeout_ms: u64,
    },
    ArtifactInfo {
        artifact_id: String,
    },
    ArtifactRead {
        artifact_id: String,
        offset: u64,
        length: u32,
    },
    ArtifactPin {
        artifact_id: String,
        pinned: bool,
    },
}

#[derive(Serialize, Deserialize)]
struct Request {
    schema_version: u32,
    request_id: String,
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

impl From<ApiError> for V2Error {
    fn from(error: ApiError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            details: error.details,
            retryable: false,
        }
    }
}

impl From<V2Error> for ApiError {
    fn from(error: V2Error) -> Self {
        Self {
            code: error.code,
            message: error.message,
            details: error.details,
        }
    }
}

pub type Reply = V2Envelope<Value>;

const INVALID_REQUEST_ID: &str = "control.invalid";
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_request_id(prefix: &str) -> String {
    let sequence = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}.{}.{}", std::process::id(), sequence)
}

fn reply_id(request_id: &str) -> &str {
    if valid_request_id(request_id) {
        request_id
    } else {
        INVALID_REQUEST_ID
    }
}

fn error_reply(session: &str, request_id: &str, error: ApiError) -> Reply {
    V2Envelope::failure(session, reply_id(request_id), error.into())
}

fn artifact_error_reply(session: &Session, request_id: &str, error: ArtifactError) -> Reply {
    let retryable = matches!(
        error.code,
        ArtifactErrorCode::Busy | ArtifactErrorCode::QuotaExceeded
    );
    let mut details = None;
    if error.code == ArtifactErrorCode::QuotaExceeded {
        details = Some(json!({"retry_after_cleanup": true}));
    }
    V2Envelope::failure(
        &session.id,
        reply_id(request_id),
        V2Error {
            code: error.code.as_str().into(),
            message: error.message,
            details,
            retryable,
        },
    )
}

fn success_reply(session: &str, request_id: &str, result: Value) -> Reply {
    V2Envelope::success(session, reply_id(request_id), result)
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
            schema_version: CONTROL_SCHEMA_VERSION,
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
                                let reply = error_reply(
                                    &auth.session_id,
                                    "control.busy",
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
            return error_reply(
                &session.id,
                INVALID_REQUEST_ID,
                ApiError::new("invalid_request", "Expected a framed control request"),
            );
        }
    };
    if request.role != "control"
        || request.token != registration.token
        || request.session_id != registration.session_id
    {
        return error_reply(
            &session.id,
            &request.request_id,
            ApiError::new(
                "unauthorized",
                "Invalid control credentials or session identity",
            ),
        );
    }
    if request.schema_version != CONTROL_SCHEMA_VERSION {
        return error_reply(
            &session.id,
            &request.request_id,
            ApiError::new(
                "invalid_schema",
                "Control requests must use schema version 2",
            ),
        );
    }
    if !valid_request_id(&request.request_id) {
        return error_reply(
            &session.id,
            INVALID_REQUEST_ID,
            ApiError::new(
                "invalid_request_id",
                "request_id must be 1-128 ASCII identifier characters",
            ),
        );
    }
    let request_id = request.request_id.clone();
    let result = match request.command {
        Command::Ping => json!({"lifecycle": session.store.state().lifecycle}),
        Command::Status => status_json(session),
        Command::Diagnostics => {
            let state = session.store.state();
            json!({"seq": state.seq, "build": state.build, "desired": state.desired,
                "diagnostics": state.diagnostics, "diagnostics_omitted": state.diagnostics_omitted,
                "runtime_issues": state.runtime_issues, "runtime_issues_omitted": state.runtime_issues_omitted,
                "storage_error": state.storage_error, "watcher_error": state.watcher_error})
        }
        Command::Windows => {
            let state = session.store.state();
            let run_id = state
                .running
                .as_ref()
                .and_then(|run| run.scope.run_id.clone());
            json!({"run_id": run_id, "windows": session.windows.snapshots(run_id.as_deref())})
        }
        Command::Build => serde_json::to_value(session.request_build(&request_id))
            .expect("serializable build request result"),
        Command::Events { after, timeout_ms } => {
            if timeout_ms > MAX_WAIT_MS || after > session.store.state().seq {
                return error_reply(
                    &session.id,
                    &request_id,
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
        Command::ArtifactInfo { artifact_id } => {
            let info = match session.artifacts.info(&artifact_id) {
                Ok(info) => info,
                Err(error) => return artifact_error_reply(session, &request_id, error),
            };
            serde_json::to_value(info).expect("serializable artifact info")
        }
        Command::ArtifactRead {
            artifact_id,
            offset,
            length,
        } => {
            let chunk = match session
                .artifacts
                .read_chunk(&artifact_id, offset, length as usize)
            {
                Ok(chunk) => chunk,
                Err(error) => return artifact_error_reply(session, &request_id, error),
            };
            json!({
                "artifact_id": chunk.artifact_id,
                "offset": chunk.offset,
                "bytes": chunk.data.len(),
                "eof": chunk.eof,
                "data": protocol::b64::encode(&chunk.data),
            })
        }
        Command::ArtifactPin {
            artifact_id,
            pinned,
        } => {
            let info = match session.artifacts.pin(&artifact_id, pinned) {
                Ok(info) => info,
                Err(error) => return artifact_error_reply(session, &request_id, error),
            };
            serde_json::to_value(info).expect("serializable artifact info")
        }
    };
    success_reply(&session.id, &request_id, result)
}

fn status_json(session: &Session) -> Value {
    let state = session.store.state();
    let run_id = state
        .running
        .as_ref()
        .and_then(|run| run.scope.run_id.clone());
    let windows = session.windows.snapshots(run_id.as_deref());
    let mut value = state.status_json();
    value["windows"] = serde_json::to_value(&windows).expect("serializable window state");
    let ui = if windows.iter().any(|window| window.ui == "unresponsive") {
        "unresponsive"
    } else if windows.iter().any(|window| window.ui == "responsive") {
        "responsive"
    } else if windows.iter().any(|window| window.ui == "unknown") {
        "unknown"
    } else {
        "unavailable"
    };
    value["ui"] = json!({"status": ui, "window_count": windows.len()});
    value
}

pub fn request(registration: &Registration, request_id: &str, command: Command) -> Result<Reply> {
    if !valid_request_id(request_id) {
        bail!("invalid control request id");
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, registration.port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
    let timeout = match command {
        Command::Events { timeout_ms, .. } => timeout_ms,
        _ => 0,
    };
    stream.set_read_timeout(Some(Duration::from_millis(timeout + 3000)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let payload = Request {
        schema_version: CONTROL_SCHEMA_VERSION,
        request_id: request_id.into(),
        role: "control".into(),
        token: registration.token.clone(),
        session_id: registration.session_id.clone(),
        command,
    };
    protocol::write_frame(&mut stream, &protocol::encode(&payload)?)?;
    let reply: Reply = protocol::decode(&protocol::read_frame(&mut stream)?)?;
    if reply.session_id != registration.session_id
        || reply.schema_version != CONTROL_SCHEMA_VERSION
        || reply.request_id != request_id
        || !reply.is_valid()
    {
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
        if let Ok(reply) = request(&registration, &next_request_id("discover"), Command::Ping)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_uses_the_current_control_schema() {
        let registration: Registration = serde_json::from_value(json!({
            "schema_version": 2,
            "session_id": "s1",
            "project_root": ".",
            "target_id": "desktop:test",
            "supervisor_pid": 1,
            "created_at_ms": 0,
            "port": 1234,
            "token": "token"
        }))
        .unwrap();
        assert_eq!(registration.schema_version, CONTROL_SCHEMA_VERSION);
    }

    #[test]
    fn generated_request_ids_are_unique_and_valid() {
        let first = next_request_id("test");
        let second = next_request_id("test");
        assert_ne!(first, second);
        assert!(valid_request_id(&first));
        assert!(valid_request_id(&second));
    }
}
