//! CLI facade for the shared GPUI development-channel protocol.
//!
//! Wire types, JSON encoding, and bounded framing live in
//! `gpui-dev-protocol`. The CLI keeps this module as its local import surface
//! so the control server and live command retain their existing error context
//! and base64 helper without duplicating the protocol contract.

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{Read, Write};

pub use gpui_dev_protocol::{ClientMessage, MAX_FRAME_LEN, PROTO_VERSION, ServerMessage};

pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    gpui_dev_protocol::encode(message).context("encoding dev channel message")
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    gpui_dev_protocol::decode(bytes).context("decoding dev channel message")
}

pub fn write_frame(stream: &mut impl Write, payload: &[u8]) -> Result<()> {
    gpui_dev_protocol::write_frame(stream, payload).context("writing dev channel frame")
}

pub fn read_frame(stream: &mut impl Read) -> Result<Vec<u8>> {
    gpui_dev_protocol::read_frame(stream).context("reading dev channel frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_roundtrip_through_a_buffer() {
        let message = ServerMessage::AssetChanged {
            path: "assets/logo.png".to_string(),
            asset_revision: 3,
        };
        let payload = encode(&message).unwrap();
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &payload).unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded = read_frame(&mut cursor).unwrap();
        let back: ServerMessage = decode(&decoded).unwrap();
        assert!(
            matches!(back, ServerMessage::AssetChanged { path, asset_revision } if path == "assets/logo.png" && asset_revision == 3)
        );
    }

    #[test]
    fn explicit_asset_removal_uses_the_wire_tag() {
        let payload = encode(&ServerMessage::AssetRemoved {
            path: "assets/old.png".to_string(),
            asset_revision: 4,
        })
        .unwrap();
        let text = String::from_utf8(payload).unwrap();
        assert!(text.starts_with("{\"type\":\"asset_removed\""));
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
            current_proto: PROTO_VERSION,
        })
        .unwrap();
        let text = String::from_utf8(payload).unwrap();
        assert!(text.contains("\"type\":\"hello_error\""));
        assert!(text.contains("\"current_proto\":2"));
    }

    #[test]
    fn hello_metadata_is_optional_for_legacy_clients() {
        let legacy: ClientMessage = decode(
            br#"{"type":"hello","proto":2,"token":"t","project":"p","pid":1,"platform":"macos"}"#,
        )
        .unwrap();
        assert!(matches!(
            legacy,
            ClientMessage::Hello {
                runtime_version: None,
                gpui_version: None,
                capabilities,
                ..
            } if capabilities.is_empty()
        ));

        let current: ClientMessage = decode(
            br#"{"type":"hello","proto":2,"token":"t","project":"p","pid":1,"platform":"macos","runtime_version":"agent-native-dev-runtime-v1","gpui_version":"0.3.5","capabilities":["logs","state"]}"#,
        )
        .unwrap();
        assert!(matches!(
            current,
            ClientMessage::Hello {
                runtime_version: Some(runtime),
                gpui_version: Some(gpui),
                capabilities,
                ..
            } if runtime == "agent-native-dev-runtime-v1"
                && gpui == "0.3.5"
                && capabilities == vec!["logs".to_string(), "state".to_string()]
        ));
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
