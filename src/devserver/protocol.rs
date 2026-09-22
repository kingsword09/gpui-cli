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
    /// The token was valid, but this supervisor cannot speak the requested
    /// app-channel protocol. Authentication failures never receive this
    /// detail.
    HelloError {
        code: String,
        message: String,
        supported_proto: u32,
    },
    /// An asset file changed on disk; the app should drop its cache entry.
    AssetChanged { path: String },
    /// The new bytes for an asset (base64), used for platforms whose app
    /// sandbox cannot read the project directory (iOS simulator).
    AssetData { path: String, data: String },
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

    #[test]
    fn unsupported_protocol_replies_are_structured() {
        let payload = encode(&ServerMessage::HelloError {
            code: "unsupported_version".into(),
            message: "upgrade required".into(),
            supported_proto: PROTO_VERSION,
        })
        .unwrap();
        let text = String::from_utf8(payload).unwrap();
        assert!(text.contains("\"type\":\"hello_error\""));
        assert!(text.contains("\"supported_proto\":1"));
    }
}

/// Minimal standard base64; used for pushing asset bytes over the channel.
pub mod b64 {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                TABLE[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        fn value(c: u8) -> Option<u32> {
            match c {
                b'A'..=b'Z' => Some((c - b'A') as u32),
                b'a'..=b'z' => Some((c - b'a' + 26) as u32),
                b'0'..=b'9' => Some((c - b'0' + 52) as u32),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        let bytes: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks(4) {
            if chunk.len() < 2 {
                return None;
            }
            let mut n = 0u32;
            let mut count = 0;
            for (i, &c) in chunk.iter().enumerate() {
                if c == b'=' {
                    break;
                }
                n |= value(c)? << (18 - 6 * i);
                count += 1;
            }
            out.push((n >> 16) as u8);
            if count > 2 {
                out.push((n >> 8) as u8);
            }
            if count > 3 {
                out.push(n as u8);
            }
        }
        Some(out)
    }
}
#[cfg(test)]
mod b64_tests {
    use super::b64;

    #[test]
    fn roundtrips_arbitrary_bytes() {
        for len in 0..40usize {
            let data: Vec<u8> = (0..len as u8).map(|i| i.wrapping_mul(37)).collect();
            let encoded = b64::encode(&data);
            assert_eq!(b64::decode(&encoded).as_deref(), Some(data.as_slice()));
        }
    }
}
