//! Strict, observation-bound input action contracts.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_ACTION_ID_BYTES: usize = 256;
pub const MAX_ACTION_TEXT_BYTES: usize = 32 * 1024;
pub const MAX_ACTION_KEY_BYTES: usize = 64;
pub const MAX_ACTION_DELTA: f32 = 1_000_000.0;
pub const MAX_ACTION_DURATION_MS: u64 = 120_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Click {
        #[serde(default = "default_button")]
        button: String,
    },
    TypeText {
        text: String,
        #[serde(default = "default_text_mode")]
        mode: String,
    },
    Key {
        key: String,
    },
    Scroll {
        delta_x: f32,
        delta_y: f32,
        #[serde(default)]
        duration_ms: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub observation_id: String,
    pub window_id: String,
    pub logical_id: String,
    pub action: Action,
}

impl ActionRequest {
    pub fn validate(&self) -> Result<(), ActionError> {
        validate_id("observation_id", &self.observation_id, 128)?;
        validate_id("window_id", &self.window_id, 128)?;
        validate_id("logical_id", &self.logical_id, MAX_ACTION_ID_BYTES)?;
        self.action.validate()
    }

    pub fn required_capability(&self) -> &'static str {
        self.action.required_capability()
    }

    pub fn target(&self) -> Value {
        json!({
            "observation_id": self.observation_id,
            "window_id": self.window_id,
            "logical_id": self.logical_id,
            "action": self.action,
            "input_consistency": "observation_bound",
            "dispatch": "normal_event_path",
        })
    }
}

impl Action {
    pub fn validate(&self) -> Result<(), ActionError> {
        match self {
            Self::Click { button } => {
                if !matches!(button.as_str(), "left" | "middle" | "right") {
                    return Err(ActionError::new(
                        "invalid_button",
                        "click button must be left, middle, or right",
                    ));
                }
            }
            Self::TypeText { text, mode } => {
                if text.len() > MAX_ACTION_TEXT_BYTES {
                    return Err(ActionError::with_details(
                        "text_too_large",
                        "type_text payload exceeds the bounded input size",
                        json!({"max_bytes": MAX_ACTION_TEXT_BYTES, "actual_bytes": text.len()}),
                    ));
                }
                if text.contains('\0') {
                    return Err(ActionError::new(
                        "invalid_text",
                        "type_text payload must not contain NUL characters",
                    ));
                }
                if !matches!(mode.as_str(), "replace" | "append") {
                    return Err(ActionError::new(
                        "invalid_text_mode",
                        "type_text mode must be replace or append",
                    ));
                }
            }
            Self::Key { key } => {
                if key.is_empty() || key.len() > MAX_ACTION_KEY_BYTES || key.contains('\0') {
                    return Err(ActionError::new(
                        "invalid_key",
                        "key must be non-empty and at most 64 bytes",
                    ));
                }
            }
            Self::Scroll {
                delta_x,
                delta_y,
                duration_ms,
            } => {
                if !delta_x.is_finite()
                    || !delta_y.is_finite()
                    || delta_x.abs() > MAX_ACTION_DELTA
                    || delta_y.abs() > MAX_ACTION_DELTA
                    || (*delta_x == 0.0 && *delta_y == 0.0)
                {
                    return Err(ActionError::new(
                        "invalid_scroll_delta",
                        "scroll delta must be finite, bounded, and non-zero",
                    ));
                }
                if *duration_ms > MAX_ACTION_DURATION_MS {
                    return Err(ActionError::with_details(
                        "duration_too_large",
                        "scroll duration exceeds the bounded action deadline",
                        json!({"max_ms": MAX_ACTION_DURATION_MS}),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn required_capability(&self) -> &'static str {
        match self {
            Self::Click { .. } => "input.pointer",
            Self::TypeText { .. } | Self::Key { .. } => "input.keyboard",
            Self::Scroll { .. } => "input.pointer",
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Click { .. } => "click",
            Self::TypeText { .. } => "type_text",
            Self::Key { .. } => "key",
            Self::Scroll { .. } => "scroll",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl ActionError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    fn with_details(code: &str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Some(details),
        }
    }
}

fn validate_id(name: &str, value: &str, max_bytes: usize) -> Result<(), ActionError> {
    if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
        return Err(ActionError::with_details(
            "invalid_action_target",
            format!("{name} must be non-empty and at most {max_bytes} bytes"),
            json!({"field": name, "max_bytes": max_bytes}),
        ));
    }
    Ok(())
}

/// Ensures a logical selector resolves to one currently instantiated and
/// minimally inspectable node in the bound semantics observation.
pub fn validate_target_query(result: &Value) -> Result<(), ActionError> {
    let nodes = result["nodes"].as_array().ok_or_else(|| {
        ActionError::new(
            "invalid_semantics_result",
            "semantic target query did not return a node list",
        )
    })?;
    if nodes.is_empty() {
        return Err(ActionError::new(
            "selector_not_found",
            "logical_id did not resolve in the selected observation",
        ));
    }
    if nodes.len() != 1 {
        return Err(ActionError::with_details(
            "selector_ambiguous",
            "logical_id resolved to more than one semantic node",
            json!({"matches": nodes.len()}),
        ));
    }
    let unsupported = result["unsupported_fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let node = &nodes[0];
    if unsupported.contains("enabled") || node.get("enabled").is_none_or(Value::is_null) {
        return Err(ActionError::new(
            "element_state_unavailable",
            "the observation does not expose the target's enabled state",
        ));
    }
    if node["enabled"] == false {
        return Err(ActionError::new(
            "element_disabled",
            "the selected semantic node is disabled",
        ));
    }
    if unsupported.contains("bounds") || node.get("bounds").and_then(Value::as_object).is_none() {
        return Err(ActionError::new(
            "element_bounds_unavailable",
            "the observation does not expose target bounds for hit testing",
        ));
    }
    Ok(())
}

/// Returns a bounded click point at the center of the uniquely resolved
/// semantic node. Coordinates use thousandths of a GPUI pixel so the wire
/// protocol can remain integer-only.
pub fn click_center_milli(result: &Value) -> Result<(u32, u32), ActionError> {
    let node = result["nodes"]
        .as_array()
        .and_then(|nodes| (nodes.len() == 1).then(|| &nodes[0]))
        .ok_or_else(|| {
            ActionError::new(
                "invalid_semantics_result",
                "click target query did not return exactly one node",
            )
        })?;
    let bounds = node["bounds"].as_object().ok_or_else(|| {
        ActionError::new(
            "element_bounds_unavailable",
            "the observation does not expose target bounds for hit testing",
        )
    })?;
    let number = |key: &str| bounds.get(key).and_then(Value::as_f64);
    let (Some(x), Some(y), Some(width), Some(height)) =
        (number("x"), number("y"), number("width"), number("height"))
    else {
        return Err(ActionError::new(
            "invalid_element_bounds",
            "target bounds must contain numeric x, y, width, and height",
        ));
    };
    let center_x = x + width / 2.0;
    let center_y = y + height / 2.0;
    let to_milli = |value: f64| {
        let scaled = (value * 1000.0).round();
        (scaled.is_finite() && (0.0..=u32::MAX as f64).contains(&scaled)).then_some(scaled as u32)
    };
    if !x.is_finite()
        || !y.is_finite()
        || !width.is_finite()
        || !height.is_finite()
        || x < 0.0
        || y < 0.0
        || width <= 0.0
        || height <= 0.0
    {
        return Err(ActionError::new(
            "invalid_element_bounds",
            "target bounds must be finite, non-negative, and non-empty",
        ));
    }
    let (Some(x_milli), Some(y_milli)) = (to_milli(center_x), to_milli(center_y)) else {
        return Err(ActionError::new(
            "invalid_element_bounds",
            "target bounds exceed the supported coordinate range",
        ));
    };
    Ok((x_milli, y_milli))
}

fn default_button() -> String {
    "left".into()
}

fn default_text_mode() -> String {
    "replace".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(action: Action) -> ActionRequest {
        ActionRequest {
            observation_id: "observation-1".into(),
            window_id: "main".into(),
            logical_id: "counter.increment".into(),
            action,
        }
    }

    #[test]
    fn action_contracts_select_the_normal_input_capability() {
        assert_eq!(
            request(Action::Click {
                button: "left".into()
            })
            .required_capability(),
            "input.pointer"
        );
        assert_eq!(
            request(Action::TypeText {
                text: "hello".into(),
                mode: "replace".into()
            })
            .required_capability(),
            "input.keyboard"
        );
        assert_eq!(
            request(Action::Scroll {
                delta_x: 0.0,
                delta_y: 10.0,
                duration_ms: 0
            })
            .required_capability(),
            "input.pointer"
        );
    }

    #[test]
    fn invalid_action_payloads_fail_before_admission() {
        let invalid_button = request(Action::Click {
            button: "double".into(),
        });
        assert_eq!(
            invalid_button.validate().unwrap_err().code,
            "invalid_button"
        );

        let invalid_mode = request(Action::TypeText {
            text: "hello".into(),
            mode: "merge".into(),
        });
        assert_eq!(
            invalid_mode.validate().unwrap_err().code,
            "invalid_text_mode"
        );

        let invalid_scroll = request(Action::Scroll {
            delta_x: 0.0,
            delta_y: 0.0,
            duration_ms: 0,
        });
        assert_eq!(
            invalid_scroll.validate().unwrap_err().code,
            "invalid_scroll_delta"
        );

        let unknown_field = serde_json::from_value::<Action>(json!({
            "type": "click",
            "button": "left",
            "double_click": true
        }));
        assert!(unknown_field.is_err());
    }

    #[test]
    fn action_target_is_observation_bound_and_explicitly_not_direct_handler_dispatch() {
        let target = request(Action::Click {
            button: "left".into(),
        })
        .target();
        assert_eq!(target["observation_id"], "observation-1");
        assert_eq!(target["logical_id"], "counter.increment");
        assert_eq!(target["dispatch"], "normal_event_path");
        assert_eq!(target["action"]["type"], "click");
    }

    #[test]
    fn action_request_retries_reuse_the_operation_record_and_conflicts_are_rejected() {
        let store = crate::devserver::operations::OperationStore::new(4);
        let now = 10_000;
        let first_request = request(Action::Click {
            button: "left".into(),
        });
        let first = store
            .submit(
                "act.1",
                "action",
                crate::devserver::events::Scope::default(),
                first_request.target(),
                now + 1_000,
                now,
            )
            .unwrap();
        let operation_id = match first {
            crate::devserver::operations::SubmitResult::Created(snapshot) => snapshot.operation_id,
            crate::devserver::operations::SubmitResult::Existing(_) => {
                panic!("first action admission must create an operation")
            }
        };
        let _ = store.start(&operation_id, now + 1).unwrap();
        let _ = store
            .finish(
                &operation_id,
                crate::devserver::operations::OperationState::Failed,
                None,
                Some(crate::devserver::operations::OperationError::new(
                    "input_adapter_unavailable",
                    "test admission boundary",
                )),
                now + 2,
            )
            .unwrap();

        let replay = store
            .submit(
                "act.1",
                "action",
                crate::devserver::events::Scope::default(),
                first_request.target(),
                now + 1_000,
                now + 3,
            )
            .unwrap();
        assert!(matches!(
            replay,
            crate::devserver::operations::SubmitResult::Existing(_)
        ));

        let conflict = store.submit(
            "act.1",
            "action",
            crate::devserver::events::Scope::default(),
            request(Action::Click {
                button: "right".into(),
            })
            .target(),
            now + 1_000,
            now + 3,
        );
        assert_eq!(conflict.unwrap_err().code, "idempotency_conflict");
    }

    #[test]
    fn target_preflight_rejects_missing_ambiguous_disabled_and_unbounded_nodes() {
        assert_eq!(
            validate_target_query(&json!({"nodes": [], "unsupported_fields": []}))
                .unwrap_err()
                .code,
            "selector_not_found"
        );
        assert_eq!(
            validate_target_query(&json!({
                "nodes": [{}, {}],
                "unsupported_fields": []
            }))
            .unwrap_err()
            .code,
            "selector_ambiguous"
        );
        assert_eq!(
            validate_target_query(&json!({
                "nodes": [{"enabled": false, "bounds": {}}],
                "unsupported_fields": []
            }))
            .unwrap_err()
            .code,
            "element_disabled"
        );
        assert_eq!(
            validate_target_query(&json!({
                "nodes": [{"enabled": true, "bounds": null}],
                "unsupported_fields": []
            }))
            .unwrap_err()
            .code,
            "element_bounds_unavailable"
        );
        assert!(
            validate_target_query(&json!({
                "nodes": [{"enabled": true, "bounds": {"x": 1, "y": 2}}],
                "unsupported_fields": []
            }))
            .is_ok()
        );
    }

    #[test]
    fn click_point_uses_bounded_center_coordinates() {
        let query = json!({"nodes": [{"bounds": {"x": 12.25, "y": 4.0,
            "width": 20.0, "height": 11.5}}]});
        assert_eq!(click_center_milli(&query).unwrap(), (22_250, 9_750));

        let invalid = json!({"nodes": [{"bounds": {"x": 0, "y": 0,
            "width": 0, "height": 10}}]});
        assert_eq!(
            click_center_milli(&invalid).unwrap_err().code,
            "invalid_element_bounds"
        );
    }
}
