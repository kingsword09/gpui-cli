//! Shared, GPUI-independent wire types for the development channel.
//!
//! The crate deliberately owns only the bounded JSON framing and protocol
//! messages. GPUI adapters, supervisor state, credentials, and event storage
//! remain outside it so old and new runtimes can share a stable transport.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};

/// Current app-channel protocol version.
pub const PROTO_VERSION: u32 = 2;
/// Maximum payload for one length-prefixed frame.
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;
/// Control API schema used by the planned v2 observation surface.
pub const CONTROL_SCHEMA_VERSION: u32 = 2;

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gpui_version: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    WindowRegistered {
        window_id: String,
        title: String,
        width: u32,
        height: u32,
        scale_milli: u32,
        foreground: bool,
    },
    WindowClosed {
        window_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    UiProbeResult {
        request_id: String,
        window_id: String,
        responsive: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        latency_ms: Option<u64>,
    },
    AssetsApplied {
        asset_revision: u64,
        applied: Vec<String>,
        failed: Vec<String>,
        cache_invalidated: bool,
    },
    AssetsReconciled {
        asset_revision: u64,
        present: Vec<String>,
        missing: Vec<String>,
        stale: Vec<String>,
        removed: Vec<String>,
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
        current_proto: u32,
    },
    AssetChanged {
        path: String,
        asset_revision: u64,
    },
    AssetData {
        path: String,
        data: String,
        asset_revision: u64,
    },
    AssetRemoved {
        path: String,
        asset_revision: u64,
    },
    AssetManifest {
        asset_revision: u64,
        entries: Vec<AssetManifestEntry>,
    },
    PrepareRestart {
        session: String,
    },
    ProbeUi {
        request_id: String,
        window_id: String,
    },
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AssetManifestEntry {
    pub path: String,
    pub hash: String,
}

/// Stable outer shape for v2 control responses. The CLI can carry this DTO
/// without coupling the transport crate to any observation implementation.
#[derive(Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct V2Envelope<T> {
    pub schema_version: u32,
    pub session_id: String,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<V2Error>,
}

impl<T> V2Envelope<T> {
    pub fn success(
        session_id: impl Into<String>,
        request_id: impl Into<String>,
        result: T,
    ) -> Self {
        Self {
            schema_version: CONTROL_SCHEMA_VERSION,
            session_id: session_id.into(),
            request_id: request_id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(
        session_id: impl Into<String>,
        request_id: impl Into<String>,
        error: V2Error,
    ) -> Self {
        Self {
            schema_version: CONTROL_SCHEMA_VERSION,
            session_id: session_id.into(),
            request_id: request_id.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// Checks the invariants that cannot be expressed by serde attributes.
    pub fn is_valid(&self) -> bool {
        self.schema_version == CONTROL_SCHEMA_VERSION
            && valid_request_id(&self.request_id)
            && if self.ok {
                self.result.is_some() && self.error.is_none()
            } else {
                self.result.is_none() && self.error.is_some()
            }
    }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct V2Error {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Capability {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub provider: String,
    #[serde(default)]
    pub constraints: BTreeMap<String, Value>,
}

pub fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
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
    fn hello_without_optional_metadata_decodes() {
        let message: ClientMessage = decode(
            br#"{"type":"hello","proto":2,"token":"t","project":"p","pid":1,"platform":"macos","asset_reload":false}"#,
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
            current_proto: PROTO_VERSION,
        };
        assert_eq!(
            decode::<ServerMessage>(&encode(&error).unwrap()).unwrap(),
            error
        );
    }

    #[test]
    fn frame_roundtrip_and_limit_are_bounded() {
        let payload = encode(&ServerMessage::AssetChanged {
            path: "a".into(),
            asset_revision: 1,
        })
        .unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &payload).unwrap();
        let decoded = read_frame(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(
            decode::<ServerMessage>(&decoded).unwrap(),
            ServerMessage::AssetChanged {
                path: "a".into(),
                asset_revision: 1,
            }
        );

        let mut oversized = Vec::new();
        oversized.extend_from_slice(&(MAX_FRAME_LEN + 1).to_be_bytes());
        assert!(read_frame(&mut Cursor::new(oversized)).is_err());
    }

    #[test]
    fn explicit_asset_removal_roundtrips() {
        let message = ServerMessage::AssetRemoved {
            path: "assets/old.png".into(),
            asset_revision: 4,
        };
        assert_eq!(
            decode::<ServerMessage>(&encode(&message).unwrap()).unwrap(),
            message
        );
    }

    #[test]
    fn asset_manifest_and_reconciliation_roundtrip() {
        let manifest = ServerMessage::AssetManifest {
            asset_revision: 9,
            entries: vec![AssetManifestEntry {
                path: "assets/logo.png".into(),
                hash: "abc".into(),
            }],
        };
        assert_eq!(
            decode::<ServerMessage>(&encode(&manifest).unwrap()).unwrap(),
            manifest
        );

        let reconciliation = ClientMessage::AssetsReconciled {
            asset_revision: 9,
            present: vec![],
            missing: vec!["assets/logo.png".into()],
            stale: vec![],
            removed: vec![],
        };
        assert_eq!(
            decode::<ClientMessage>(&encode(&reconciliation).unwrap()).unwrap(),
            reconciliation
        );
    }
}
