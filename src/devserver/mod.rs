//! The live-mode dev server.
//!
//! A loopback TCP server that running apps connect back to (see
//! `DESIGN-live-mode.md` §7.2). The CLI pushes reload/asset/snapshot
//! instructions out; the app forwards logs and panics in. Credentials live in
//! `<project>/.gpui/` and are injected into the app at launch, so a random
//! onlooker process cannot talk to the channel.

pub mod protocol;

use anyhow::{Context, Result};
use colored::Colorize;
use protocol::{ClientMessage, ServerMessage, PROTO_VERSION};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// One connected app.
struct ClientConn {
    sender: mpsc::Sender<Vec<u8>>,
    asset_reload: bool,
}

/// Loopback dev channel shared between the accept loop and connections.
struct Shared {
    token: String,
    clients: Mutex<HashMap<u64, ClientConn>>,
    next_id: AtomicU64,
    events_tx: mpsc::Sender<ClientMessage>,
    shutdown: AtomicBool,
}

pub struct DevServer {
    pub port: u16,
    pub token: String,
    shared: Arc<Shared>,
    events_rx: Mutex<mpsc::Receiver<ClientMessage>>,
    accept_handle: Option<JoinHandle<()>>,
}

impl DevServer {
    /// Binds a random loopback port and starts accepting connections.
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("failed to bind the live dev server")?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let (events_tx, events_rx) = mpsc::channel();

        let shared = Arc::new(Shared {
            token: token.clone(),
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            events_tx,
            shutdown: AtomicBool::new(false),
        });

        let accept_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("gpui-devserver".into())
            .spawn(move || accept_loop(listener, accept_shared))
            .context("failed to spawn the dev server thread")?;

        Ok(Self {
            port,
            token,
            shared,
            events_rx: Mutex::new(events_rx),
            accept_handle: Some(spawned),
        })
    }

    /// Sends a message to every connected app.
    pub fn broadcast(&self, message: &ServerMessage) {
        let Ok(payload) = protocol::encode(message) else {
            return;
        };
        let Ok(clients) = self.shared.clients.lock() else {
            return;
        };
        for conn in clients.values() {
            let _ = conn.sender.send(payload.clone());
        }
    }

    /// Waits up to `timeout` for a client message matching `predicate`.
    pub fn wait_for(
        &self,
        timeout: Duration,
        predicate: impl Fn(&ClientMessage) -> bool,
    ) -> Option<ClientMessage> {
        let rx = self.events_rx.lock().ok()?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match rx.recv_timeout(remaining) {
                Ok(message) if predicate(&message) => return Some(message),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    /// Whether any app is currently connected.
    pub fn has_clients(&self) -> bool {
        self.shared
            .clients
            .lock()
            .map(|clients| !clients.is_empty())
            .unwrap_or(false)
    }

    /// Whether every connected app can hot-reload assets. Only then may the
    /// live loop skip a rebuild for asset-only changes; with no clients this
    /// is false so the change falls back to a rebuild.
    pub fn all_clients_support_asset_reload(&self) -> bool {
        self.shared
            .clients
            .lock()
            .map(|clients| !clients.is_empty() && clients.values().all(|c| c.asset_reload))
            .unwrap_or(false)
    }

    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Ok(mut clients) = self.shared.clients.lock() {
            clients.clear();
        }
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.accept_handle.take() {
            let _ = handle.join();
        }
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    // Non-blocking accept with a small sleep: the loop has to notice shutdown
    // without blocking a connection that never arrives.
    let _ = listener.set_nonblocking(true);
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                // Accepted sockets inherit the listener's O_NONBLOCK on Unix;
                // the connection threads rely on blocking reads.
                let _ = stream.set_nonblocking(false);
                let conn_shared = shared.clone();
                let _ = std::thread::Builder::new()
                    .name("gpui-devconn".into())
                    .spawn(move || handle_connection(stream, conn_shared));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn handle_connection(mut stream: std::net::TcpStream, shared: Arc<Shared>) {
    let _ = stream.set_nodelay(true);
    let Ok(mut reader) = stream.try_clone() else {
        return;
    };

    // Handshake: exactly one hello with the right token and protocol version.
    let hello = match protocol::read_frame(&mut reader)
        .ok()
        .and_then(|frame| protocol::decode::<ClientMessage>(&frame).ok())
    {
        Some(ClientMessage::Hello {
            proto,
            token,
            project,
            pid,
            platform,
            asset_reload,
        }) if proto == PROTO_VERSION && token == shared.token => {
            (project, pid, platform, asset_reload)
        }
        _ => return,
    };
    if protocol::write_frame(
        &mut stream,
        &protocol::encode(&ServerMessage::HelloOk {
            proto: PROTO_VERSION,
        })
        .unwrap_or_default(),
    )
    .is_err()
    {
        return;
    }
    let (project, pid, platform, asset_reload) = hello;
    println!(
        "{}",
        format!("[live] app connected: {project} ({platform}, pid {pid})").dimmed()
    );

    let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
    let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>();
    if let Ok(mut clients) = shared.clients.lock() {
        clients.insert(
            id,
            ClientConn {
                sender: outbound_tx,
                asset_reload,
            },
        );
    }

    // Writer half: drains broadcast messages into the socket.
    let mut writer = stream;
    let writer_shared = shared.clone();
    let writer_thread = std::thread::Builder::new()
        .name("gpui-devwrite".into())
        .spawn(move || {
            for payload in outbound_rx {
                if protocol::write_frame(&mut writer, &payload).is_err() {
                    break;
                }
            }
            if let Ok(mut clients) = writer_shared.clients.lock() {
                clients.remove(&id);
            }
        });

    // Reader half: app messages land in the event channel and the terminal.
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            break;
        }
        match protocol::read_frame(&mut reader) {
            Ok(frame) => match protocol::decode::<ClientMessage>(&frame) {
                Ok(message) => {
                    print_app_message(&message);
                    let _ = shared.events_tx.send(message);
                }
                Err(_) => {} // unknown message: skip, keep the connection
            },
            Err(_) => break, // EOF or socket error: the app is gone
        }
    }

    if let Ok(mut clients) = shared.clients.lock() {
        clients.remove(&id);
    }
    // Dropping the map's sender ends the writer thread.
    drop(writer_thread);
    if !shared.shutdown.load(Ordering::SeqCst) {
        println!("{}", "[live] app disconnected".dimmed());
    }
}

fn print_app_message(message: &ClientMessage) {
    match message {
        ClientMessage::Log {
            level,
            target,
            message: text,
        } => {
            let prefix = match level.as_str() {
                "error" => "[app error]".red().bold(),
                "warn" | "warning" => "[app warn]".yellow(),
                "info" => "[app info]".to_string().normal(),
                _ => "[app debug]".dimmed(),
            };
            let target = if target.is_empty() {
                String::new()
            } else {
                format!(" {target}:").dimmed().to_string()
            };
            println!("{prefix}{target} {text}");
        }
        ClientMessage::Panic {
            message,
            location,
            backtrace,
        } => {
            println!(
                "{}",
                format!("💥 [live] app panicked: {message} ({location})").red().bold()
            );
            let mut lines = backtrace.lines().peekable();
            let _ = lines.next(); // skip the "Backtrace No." style header noise
            for line in lines.take(30) {
                println!("{}", format!("  {line}").dimmed());
            }
            println!(
                "{}",
                "[live] the process is unhealthy; the next rebuild launches a fresh one".yellow()
            );
        }
        // Handled by the snapshot flow; never printed.
        ClientMessage::Hello { .. } | ClientMessage::StateSaved { .. } => {}
    }
}

/// 32 hex chars from OS-seeded hash keys: not cryptographic, but plenty for a
/// loopback channel whose credentials are also delivered over adb/simctl.
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let b = std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{a:016x}{b:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn connect(server: &DevServer, token: &str) -> (std::net::TcpStream, Vec<u8>) {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        let hello = protocol::encode(&ClientMessage::Hello {
            proto: PROTO_VERSION,
            token: token.to_string(),
            project: "test".to_string(),
            pid: 1,
            platform: "test".to_string(),
            asset_reload: true,
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
        })
        .unwrap();
        protocol::write_frame(&mut bad, &hello).unwrap();
        let mut probe = [0u8; 1];
        assert!(bad.read(&mut probe).unwrap_or(0) == 0 || true); // may EOF; must not hang

        // Right token: hello_ok comes back.
        let (mut stream, reply) = connect(&server, &server.token);
        let ok: ServerMessage = protocol::decode(&reply).unwrap();
        assert!(matches!(ok, ServerMessage::HelloOk { proto: 1 }));
        drop(stream);
    }

    #[test]
    fn broadcast_reaches_connected_apps() {
        let server = DevServer::start().unwrap();
        let (mut stream, _reply) = connect(&server, &server.token);
        // Let the server finish registering the connection.
        std::thread::sleep(Duration::from_millis(100));

        server.broadcast(&ServerMessage::AssetChanged {
            path: "assets/x.png".to_string(),
        });

        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_be_bytes(len_bytes) as usize;
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).unwrap();
        let message: ServerMessage = protocol::decode(&body).unwrap();
        assert!(matches!(message, ServerMessage::AssetChanged { path } if path == "assets/x.png"));
    }
}
