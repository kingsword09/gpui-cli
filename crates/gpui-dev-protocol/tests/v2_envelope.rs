use gpui_dev_protocol::{
    decode, encode, valid_request_id, Capability, V2Envelope, V2Error, CONTROL_SCHEMA_VERSION,
};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn success_envelope_has_only_result_branch() {
    let envelope = V2Envelope::success("session-1", "req.observe_1", json!({"state": "ready"}));
    assert!(envelope.is_valid());
    let value = serde_json::to_value(&envelope).unwrap();
    assert_eq!(value["schema_version"], CONTROL_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    assert!(value.get("error").is_none());
    assert_eq!(value["result"]["state"], "ready");
}

#[test]
fn failure_envelope_has_retryable_structured_error() {
    let envelope = V2Envelope::<serde_json::Value>::failure(
        "session-1",
        "req-2",
        V2Error {
            code: "unavailable".into(),
            message: "capture backend is unavailable".into(),
            details: Some(json!({"capability": "capture.scene"})),
            retryable: true,
        },
    );
    let bytes = encode(&envelope).unwrap();
    let decoded: V2Envelope<serde_json::Value> = decode(&bytes).unwrap();
    assert_eq!(decoded, envelope);
    assert!(decoded.is_valid());
}

#[test]
fn capability_preserves_provider_reason_and_constraints() {
    let mut constraints = BTreeMap::new();
    constraints.insert("foreground_only".into(), json!(true));
    let capability = Capability {
        available: false,
        reason: Some("backend_unsupported".into()),
        provider: "platform-adapter".into(),
        constraints,
    };
    let decoded: Capability = decode(&encode(&capability).unwrap()).unwrap();
    assert_eq!(decoded, capability);
}

#[test]
fn request_ids_are_bounded_and_ascii_scoped() {
    assert!(valid_request_id("req.observe-1_2"));
    assert!(!valid_request_id(""));
    assert!(!valid_request_id("req/with/slash"));
    assert!(!valid_request_id(&"x".repeat(129)));
    assert!(!valid_request_id("请求"));
}

#[test]
fn invalid_envelope_invariants_are_detectable_after_decode() {
    let mut value = serde_json::to_value(V2Envelope::success("s", "req", json!(null))).unwrap();
    value["schema_version"] = json!(1);
    value["error"] = json!({"code": "bad", "message": "bad"});
    let envelope: V2Envelope<serde_json::Value> =
        decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!envelope.is_valid());
}
