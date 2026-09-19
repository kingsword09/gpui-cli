//! Dev-channel client for `gpui run --live` (debug builds only).
//!
//! Connects back to the CLI's loopback dev server using credentials injected
//! at launch (environment variables, or a config file on platforms with no
//! environment to inherit). Forwards logs and panics to the CLI and receives
//! asset/snapshot instructions from it.
//!
//! Everything here is hand-rolled on `std` on purpose: generated projects get
//! no new dependencies, and release builds compile this module to nothing.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const PROTO_VERSION: u32 = 1;
/// Mirrors the CLI's `MAX_FRAME_LEN`.
const MAX_FRAME_LEN: u32 = 1024 * 1024;
/// Outbound queue bound: when the CLI is gone or slow, drop logs instead of
/// stalling the app (panics and logs are best-effort by design).
const OUTBOUND_BOUND: usize = 256;

struct LiveConfig {
    addr: String,
    token: String,
    project: String,
    /// Snapshot session to restore, injected by the CLI on relaunch
    /// (state snapshots). Unused until a snapshot provider registers.
    #[allow(dead_code)]
    session: Option<String>,
}

/// Sender half of the current connection; `None` while disconnected.
static OUTBOUND: Mutex<Option<mpsc::SyncSender<String>>> = Mutex::new(None);

/// Asset paths the CLI reported as changed, drained by the UI loop.
static ASSET_EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Resolves the dev-server credentials, or `None` when not run under
/// `gpui run --live`.
fn resolve_config(config_file: Option<&Path>) -> Option<LiveConfig> {
    let project = std::env::var("GPUI_LIVE_PROJECT").unwrap_or_default();
    let session = std::env::var("GPUI_LIVE_SESSION").ok().filter(|s| !s.is_empty());
    if let (Ok(addr), Ok(token)) = (
        std::env::var("GPUI_LIVE_ADDR"),
        std::env::var("GPUI_LIVE_TOKEN"),
    ) {
        return Some(LiveConfig {
            addr,
            token,
            project,
            session,
        });
    }

    // Android has no inheritable environment: the CLI stages a config file
    // into the app's internal files dir before launch.
    let content = std::fs::read_to_string(config_file?).ok()?;
    let mut addr = None;
    let mut token = None;
    let mut file_project = None;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("addr=") {
            addr = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("token=") {
            token = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("project=") {
            file_project = Some(value.trim().to_string());
        }
    }
    Some(LiveConfig {
        addr: addr?,
        token: token?,
        project: file_project.unwrap_or(project),
        session,
    })}

/// Installs the panic forwarding hook (chained on top of any existing one) and
/// the log forwarder, then spawns the connection thread.
pub fn init(config_file: Option<&Path>) {
    let Some(config) = resolve_config(config_file) else {
        return;
    };

    install_panic_hook();
    install_log_forwarder();

    let platform = if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    let _ = std::thread::Builder::new()
        .name("gpui-live".into())
        .spawn(move || client_loop(config, platform));
}

/// Asset paths reported changed by the CLI since the last call.
pub fn take_asset_events() -> Vec<String> {
    std::mem::take(&mut ASSET_EVENTS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn client_loop(config: LiveConfig, platform: &'static str) {
    loop {
        run_connection(&config, platform);
        // The CLI may be mid-rebuild; keep trying quietly.
        thread::sleep(Duration::from_secs(2));
    }
}

fn run_connection(config: &LiveConfig, platform: &'static str) {
    let Ok(mut stream) = connect_with_retry(&config.addr) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let Ok(mut reader) = stream.try_clone() else {
        return;
    };

    let hello = format!(
        "{{\"type\":\"hello\",\"proto\":{PROTO_VERSION},\"token\":\"{}\",\"project\":\"{}\",\"pid\":{},\"platform\":\"{platform}\"}}",
        json_escape(&config.token),
        json_escape(&config.project),
        std::process::id(),
    );
    if write_frame(&mut stream, hello.as_bytes()).is_err() {
        return;
    }
    match read_frame(&mut reader) {
        Ok(ref frame) if find_bytes(frame, b"hello_ok") => {}
        _ => return,
    }

    let (tx, rx) = mpsc::sync_channel::<String>(OUTBOUND_BOUND);
    {
        let mut guard = OUTBOUND.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(tx);
    }
    let writer_handle = thread::Builder::new()
        .name("gpui-live-write".into())
        .spawn(move || {
            for payload in rx {
                if write_frame(&mut stream, payload.as_bytes()).is_err() {
                    break;
                }
            }
        })
        .ok();

    loop {
        match read_frame(&mut reader) {
            Ok(frame) => dispatch(&frame, config),
            Err(_) => break,
        }
    }

    // Dropping the sender ends the writer thread; the CLI sees the disconnect.
    {
        let mut guard = OUTBOUND.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    if let Some(handle) = writer_handle {
        let _ = handle.join();
    }
}

fn connect_with_retry(addr: &str) -> std::io::Result<TcpStream> {
    // The app can start before `adb reverse` lands; retry briefly.
    for attempt in 0..8 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(400));
        }
        if let Ok(stream) = TcpStream::connect(addr) {
            return Ok(stream);
        }
    }
    TcpStream::connect(addr)
}

/// Handles one server message. Frames come from our own CLI, so a targeted
/// scan for the known fields is enough (no JSON parser).
fn dispatch(frame: &[u8], config: &LiveConfig) {
    if find_bytes(frame, b"\"asset_changed\"") {
        if let Some(path) = string_field(frame, "path") {
            ASSET_EVENTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(path);
        }
    }
    // `prepare_restart` (state snapshots) is handled by the snapshot module.
    let _ = config;
}

// ── panic + log forwarding ───────────────────────────────────────────────────

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let message = payload_message(info);
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        // Bounded, non-blocking: a dying process must not hang or allocate wildly.
        let backtrace = {
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            trace.chars().take(8 * 1024).collect::<String>()
        };
        let payload = format!(
            "{{\"type\":\"panic\",\"message\":\"{}\",\"location\":\"{}\",\"backtrace\":\"{}\"}}",
            json_escape(&message),
            json_escape(&location),
            json_escape(&backtrace),
        );
        send(payload);
    }));
}

fn payload_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "Box<dyn Any> panic payload".to_string()
    }
}

struct LiveLogger;

impl log::Log for LiveLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        eprintln!("[{}] {}", record.level(), record.args());
        let payload = format!(
            "{{\"type\":\"log\",\"level\":\"{}\",\"target\":\"{}\",\"message\":\"{}\"}}",
            record.level().to_string().to_lowercase(),
            json_escape(record.target()),
            json_escape(&record.args().to_string()),
        );
        send(payload);
    }

    fn flush(&self) {}
}

fn install_log_forwarder() {
    // No-op if a platform logger already exists (e.g. `android_logger` on
    // Android); those platforms keep their native log destination.
    if log::set_boxed_logger(Box::new(LiveLogger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

fn send(payload: String) {
    if let Ok(guard) = OUTBOUND.lock() {
        if let Some(sender) = guard.as_ref() {
            let _ = sender.try_send(payload);
        }
    }
}

// ── minimal JSON plumbing ────────────────────────────────────────────────────

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn write_frame(stream: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(payload.len() as u32).to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut len_bytes = [0u8; 4];
    stream.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len as u32 > MAX_FRAME_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(body)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// Extracts an escaped string field's value from a JSON frame.
fn string_field(frame: &[u8], key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"").into_bytes();
    let start = frame
        .windows(needle.len())
        .position(|window| window == &needle[..])?
        + needle.len();

    let mut out: Vec<u8> = Vec::new();
    let mut i = start;
    while i < frame.len() {
        match frame[i] {
            b'"' => break,
            b'\\' => {
                i += 1;
                if i >= frame.len() {
                    return None;
                }
                match frame[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        if i + 4 >= frame.len() {
                            return None;
                        }
                        let hex = std::str::from_utf8(&frame[i + 1..i + 5]).ok()?;
                        let code = u32::from_str_radix(hex, 16).unwrap_or(0xFFFD);
                        let mut buffer = [0u8; 4];
                        out.extend_from_slice(
                            char::from_u32(code)
                                .unwrap_or('\u{FFFD}')
                                .encode_utf8(&mut buffer)
                                .as_bytes(),
                        );
                        i += 4;
                    }
                    other => out.push(other),
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_field_unescapes_known_sequences() {
        let frame = b"{\"type\":\"asset_changed\",\"path\":\"assets/a b\\\"c.png\"}";
        assert_eq!(string_field(frame, "path").as_deref(), Some("assets/a b\"c.png"));
        assert_eq!(string_field(frame, "type").as_deref(), Some("asset_changed"));
        assert_eq!(string_field(frame, "missing"), None);
    }

    #[test]
    fn json_escape_is_round_trip_safe_for_controls() {
        let escaped = json_escape("line\nbreak \"quoted\" \\slash");
        assert_eq!(escaped, "line\\nbreak \\\"quoted\\\" \\\\slash");
    }
}
