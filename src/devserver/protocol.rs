//! Wire format for the CLI ↔ app dev channel.
//!
//! Frames are 4-byte big-endian length prefixes followed by one JSON message.
//! Both sides keep messages small; `MAX_FRAME_LEN` bounds a single frame so a
//! runaway sender cannot exhaust memory.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{Read, Write};

/// Protocol version, bumped on incompatible changes.
pub const PROTO_VERSION: u32 = 1;
/// Hard cap for one frame (panic reports carry a truncated backtrace).
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// Messages sent by the running app to the CLI.
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First frame on a new connection; the server closes on bad token/version.
    Hello {
        proto: u32,
        token: String,
        project: String,
        pid: u32,
        platform: String,
        /// The app installed a dev asset source, so asset changes can be
        /// hot-reloaded without a rebuild.
        #[serde(default)]
        asset_reload: bool,
    },
    /// A log record forwarded from the app.
    Log {
        level: String,
        target: String,
        message: String,
    },
    /// The app panicked; the process is about to die (or already has).
    Panic {
        message: String,
        location: String,
        backtrace: String,
    },
    /// The app saved its snapshot for the requested restart session.
    StateSaved { session: String, data: String },
}

/// Messages sent by the CLI to connected apps.
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Reply to a valid `hello`.
    HelloOk { proto: u32 },
    /// An asset file changed on disk; the app should drop its cache entry.
    AssetChanged { path: String },
    /// A rebuild succeeded; the app should save its snapshot for `session`.
    PrepareRestart { session: String },
}

pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(message).context("encoding dev channel message")
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).context("decoding dev channel message")
}

pub fn write_frame(stream: &mut impl Write, payload: &[u8]) -> Result<()> {
    let len = u32::try_from(payload.len()).context("frame payload too large")?;
    if len > MAX_FRAME_LEN {
        bail!("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit");
    }
    stream
        .write_all(&len.to_be_bytes())
        .and_then(|_| stream.write_all(payload))
        .and_then(|_| stream.flush())
        .context("writing dev channel frame")
}

pub fn read_frame(stream: &mut impl Read) -> Result<Vec<u8>> {
    let mut len_bytes = [0u8; 4];
    stream
        .read_exact(&mut len_bytes)
        .context("reading dev channel frame length")?;
    let len = u32::from_be_bytes(len_bytes);
    if len > MAX_FRAME_LEN {
        bail!("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit");
    }
    let mut body = vec![0u8; len as usize];
    stream
        .read_exact(&mut body)
        .context("reading dev channel frame body")?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_roundtrip_through_a_buffer() {
        let message = ServerMessage::AssetChanged {
            path: "assets/logo.png".to_string(),
        };
        let payload = encode(&message).unwrap();
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &payload).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded = read_frame(&mut cursor).unwrap();
        let back: ServerMessage = decode(&decoded).unwrap();
        assert!(matches!(back, ServerMessage::AssetChanged { path } if path == "assets/logo.png"));
    }

    #[test]
    fn oversized_frames_are_rejected() {
        let mut fake = Vec::new();
        let len = (MAX_FRAME_LEN + 1).to_be_bytes();
        fake.extend_from_slice(&len);
        let mut cursor = Cursor::new(fake);
        assert!(read_frame(&mut cursor).is_err());
    }

    #[test]
    fn tags_use_snake_case_type_field() {
        let payload = encode(&ClientMessage::Log {
            level: "info".to_string(),
            target: "main".to_string(),
            message: "hello".to_string(),
        })
        .unwrap();
        let text = String::from_utf8(payload).unwrap();
        assert!(text.starts_with("{\"type\":\"log\""));
    }
}
