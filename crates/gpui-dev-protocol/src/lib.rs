//! Shared, GPUI-independent wire types for the development channel.
//!
//! The crate deliberately owns only the bounded JSON framing and protocol
//! messages. GPUI adapters, supervisor state, credentials, and event storage
//! remain outside it so old and new runtimes can share a stable transport.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{self, Read, Write};

/// Current app-channel protocol version.
pub const PROTO_VERSION: u32 = 1;
/// Maximum payload for one length-prefixed frame.
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

#[derive(Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        proto: u32,
        token: String,
        project: String,
        pid: u32,
        platform: String,
        #[serde(default)]
        asset_reload: bool,
        #[serde(default)]
        runtime_version: Option<String>,
        #[serde(default)]
        gpui_version: Option<String>,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    Log {
        level: String,
        target: String,
        message: String,
    },
    Panic {
        message: String,
        location: String,
        backtrace: String,
    },
    StateSaved {
        session: String,
        data: String,
    },
}

#[derive(Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    HelloOk {
        proto: u32,
    },
    HelloError {
        code: String,
        message: String,
        supported_proto: u32,
    },
    AssetChanged {
        path: String,
    },
    AssetData {
        path: String,
        data: String,
    },
    PrepareRestart {
        session: String,
    },
}

pub fn encode<T: Serialize>(message: &T) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(message)
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> serde_json::Result<T> {
    serde_json::from_slice(bytes)
}

pub fn write_frame(stream: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload length overflows u32",
        )
    })?;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit"),
        ));
    }
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

pub fn read_frame(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let len = u32::from_be_bytes(length);
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit"),
        ));
    }
    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn legacy_hello_without_metadata_decodes() {
        let message: ClientMessage = decode(
            br#"{"type":"hello","proto":1,"token":"t","project":"p","pid":1,"platform":"macos","asset_reload":false}"#,
        )
        .unwrap();
        assert!(matches!(
            message,
            ClientMessage::Hello {
                runtime_version: None,
                gpui_version: None,
                capabilities,
                ..
            } if capabilities.is_empty()
        ));
    }

    #[test]
    fn current_hello_and_upgrade_error_roundtrip() {
        let hello = ClientMessage::Hello {
            proto: PROTO_VERSION,
            token: "token".into(),
            project: "project".into(),
            pid: 7,
            platform: "macos".into(),
            asset_reload: true,
            runtime_version: Some("agent-native-dev-runtime-v1".into()),
            gpui_version: Some("0.3.5".into()),
            capabilities: vec!["logs".into(), "state".into()],
        };
        let decoded: ClientMessage = decode(&encode(&hello).unwrap()).unwrap();
        assert_eq!(decoded, hello);

        let error = ServerMessage::HelloError {
            code: "unsupported_version".into(),
            message: "upgrade required".into(),
            supported_proto: PROTO_VERSION,
        };
        assert_eq!(
            decode::<ServerMessage>(&encode(&error).unwrap()).unwrap(),
            error
        );
    }

    #[test]
    fn frame_roundtrip_and_limit_are_bounded() {
        let payload = encode(&ServerMessage::AssetChanged { path: "a".into() }).unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &payload).unwrap();
        let decoded = read_frame(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(
            decode::<ServerMessage>(&decoded).unwrap(),
            ServerMessage::AssetChanged { path: "a".into() }
        );

        let mut oversized = Vec::new();
        oversized.extend_from_slice(&(MAX_FRAME_LEN + 1).to_be_bytes());
        assert!(read_frame(&mut Cursor::new(oversized)).is_err());
    }
}
