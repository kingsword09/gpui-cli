# gpui-dev-protocol

Shared, GPUI-independent wire types and bounded framing for the GPUI
development channel.

The crate owns the versioned v1 JSON messages and 4-byte big-endian
length-prefixed frames used between a CLI supervisor and a generated app
runtime. It deliberately does not depend on GPUI, windowing, platform, or
credential-management code.

```rust
use gpui_dev_protocol::{decode, encode, ClientMessage};

let hello = ClientMessage::Hello {
    proto: gpui_dev_protocol::PROTO_VERSION,
    token: "token".into(),
    project: "example".into(),
    pid: 7,
    platform: "macos".into(),
    asset_reload: false,
    runtime_version: None,
    gpui_version: None,
    capabilities: Vec::new(),
};
let bytes = encode(&hello)?;
let decoded: ClientMessage = decode(&bytes)?;
# Ok::<(), serde_json::Error>(())
```

The 1 MiB frame limit is enforced by `read_frame` and `write_frame`.
