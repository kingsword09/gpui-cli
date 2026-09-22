use gpui_dev_protocol::{decode, encode, write_frame, ClientMessage, ServerMessage, PROTO_VERSION};

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "legacy-hello" => include_str!("fixtures/legacy-hello.json")
            .trim_end()
            .as_bytes(),
        "current-hello" => include_str!("fixtures/current-hello.json")
            .trim_end()
            .as_bytes(),
        "unsupported-version-error" => include_str!("fixtures/unsupported-version-error.json")
            .trim_end()
            .as_bytes(),
        _ => panic!("unknown fixture {name}"),
    }
}

#[test]
fn legacy_hello_roundtrips_without_new_metadata_fields() {
    let bytes = fixture("legacy-hello");
    let message: ClientMessage = decode(bytes).unwrap();
    assert!(matches!(
        message,
        ClientMessage::Hello {
            proto: PROTO_VERSION,
            runtime_version: None,
            gpui_version: None,
            ref capabilities,
            ..
        } if capabilities.is_empty()
    ));
    assert_eq!(encode(&message).unwrap(), bytes);
}

#[test]
fn current_hello_has_stable_metadata_wire_shape() {
    let bytes = fixture("current-hello");
    let message: ClientMessage = decode(bytes).unwrap();
    assert!(matches!(
        message,
        ClientMessage::Hello {
            proto: PROTO_VERSION,
            runtime_version: Some(ref runtime),
            gpui_version: Some(ref gpui),
            ref capabilities,
            ..
        } if runtime == "agent-native-dev-runtime-v1"
            && gpui == "0.3.5"
            && *capabilities == vec!["logs".to_string(), "state".to_string()]
    ));
    assert_eq!(encode(&message).unwrap(), bytes);
}

#[test]
fn unsupported_version_reply_keeps_structured_error_shape() {
    let bytes = fixture("unsupported-version-error");
    let message: ServerMessage = decode(bytes).unwrap();
    assert!(matches!(
        message,
        ServerMessage::HelloError {
            ref code,
            supported_proto: PROTO_VERSION,
            ..
        } if code == "unsupported_version"
    ));
    assert_eq!(encode(&message).unwrap(), bytes);
}

#[test]
fn golden_payload_uses_the_bounded_big_endian_frame() {
    let payload = fixture("current-hello");
    let mut frame = Vec::new();
    write_frame(&mut frame, payload).unwrap();
    assert_eq!(frame[..4], (payload.len() as u32).to_be_bytes());
    assert_eq!(&frame[4..], payload);
}
