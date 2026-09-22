use gpui_dev_protocol::{
    decode, encode, read_frame, write_frame, ClientMessage, ServerMessage, MAX_FRAME_LEN,
};
use std::io::{Cursor, Write};

#[test]
fn known_messages_ignore_unknown_fields_for_forward_compatibility() {
    let message: ClientMessage = decode(
        br#"{"type":"log","level":"info","target":"app","message":"hello","future_field":{"v":2}}"#,
    )
    .unwrap();
    assert!(matches!(
        message,
        ClientMessage::Log {
            level,
            target,
            message
        } if level == "info" && target == "app" && message == "hello"
    ));
}

#[test]
fn unknown_message_types_are_rejected() {
    let error =
        decode::<ClientMessage>(br#"{"type":"future_message","request_id":"req-1"}"#).unwrap_err();
    assert!(error.to_string().contains("unknown variant"));
}

#[test]
fn malformed_json_is_rejected_without_a_partial_message() {
    assert!(decode::<ServerMessage>(br#"{"type":"asset_changed","path":"x""#).is_err());
}

#[test]
fn truncated_frame_header_and_body_are_rejected() {
    assert!(read_frame(&mut Cursor::new([0, 0, 0])).is_err());

    let mut truncated_body = Vec::new();
    truncated_body.extend_from_slice(&4u32.to_be_bytes());
    truncated_body.extend_from_slice(b"abc");
    assert!(read_frame(&mut Cursor::new(truncated_body)).is_err());
}

#[test]
fn exactly_maximum_payload_is_allowed() {
    let payload = vec![b'x'; MAX_FRAME_LEN as usize];
    let mut frame = Vec::new();
    write_frame(&mut frame, &payload).unwrap();
    assert_eq!(&frame[..4], &MAX_FRAME_LEN.to_be_bytes());
    assert_eq!(read_frame(&mut Cursor::new(frame)).unwrap(), payload);
}

#[test]
fn oversized_write_is_rejected_before_touching_the_stream() {
    let payload = vec![b'x'; MAX_FRAME_LEN as usize + 1];
    let mut sink = ProbeWriter::default();
    assert!(write_frame(&mut sink, &payload).is_err());
    assert!(sink.bytes.is_empty());
}

#[derive(Default)]
struct ProbeWriter {
    bytes: Vec<u8>,
}

impl Write for ProbeWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn encoded_known_message_stays_json_and_bounded() {
    let payload = encode(&ServerMessage::AssetData {
        path: "assets/icon.png".into(),
        data: "aGVsbG8=".into(),
    })
    .unwrap();
    assert!(payload.starts_with(br#"{"type":"asset_data""#));
    assert!(payload.len() < MAX_FRAME_LEN as usize);
}
