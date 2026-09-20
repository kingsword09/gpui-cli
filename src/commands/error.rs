//! Compiler diagnostics, published as soon as Cargo writes them.

use crate::devserver::{output, session::Build};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: String,
    pub code: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub message: String,
    pub rendered: String,
    pub spans: Vec<Value>,
    pub children: Vec<Value>,
    pub raw: Value,
}

pub struct CargoOutcome {
    pub success: bool,
    pub executable: Option<PathBuf>,
}
pub enum CargoMessage {
    Diagnostic(Box<Diagnostic>),
    Executable(PathBuf),
}

pub fn run_cargo_json(cmd: &mut Command, build: &Build, stage: &str) -> Result<CargoOutcome> {
    cmd.arg("--message-format=json");
    output::run(cmd, build, stage, true)
}

pub fn parse_line(line: &[u8]) -> Option<CargoMessage> {
    let value: Value = serde_json::from_slice(line).ok()?;
    match value["reason"].as_str()? {
        "compiler-message" => {
            let message = value.get("message")?;
            let level = message["level"].as_str()?.to_owned();
            let spans = message["spans"].as_array().cloned().unwrap_or_default();
            let span = spans
                .iter()
                .find(|s| s["is_primary"] == true)
                .or_else(|| spans.first());
            Some(CargoMessage::Diagnostic(Box::new(Diagnostic {
                level,
                code: message["code"]["code"].as_str().map(str::to_owned),
                file: span
                    .and_then(|s| s["file_name"].as_str())
                    .map(str::to_owned),
                line: span
                    .and_then(|s| s["line_start"].as_u64())
                    .and_then(|v| u32::try_from(v).ok()),
                col: span
                    .and_then(|s| s["column_start"].as_u64())
                    .and_then(|v| u32::try_from(v).ok()),
                message: message["message"].as_str().unwrap_or_default().into(),
                rendered: message["rendered"].as_str().unwrap_or_default().into(),
                spans,
                children: message["children"].as_array().cloned().unwrap_or_default(),
                raw: value,
            })))
        }
        "compiler-artifact" => {
            if value["target"]["kind"]
                .as_array()?
                .iter()
                .any(|kind| kind == "bin")
            {
                value["executable"]
                    .as_str()
                    .map(|p| CargoMessage::Executable(p.into()))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retains_codes_secondary_spans_suggestions_notes_and_warnings() {
        let raw = json!({"reason": "compiler-message", "package_id": "example", "message": {
            "level": "warning", "code": {"code": "unused_variables"}, "message": "unused variable",
            "spans": [{"file_name": "macro.rs", "is_primary": false},
                {"file_name": "src/lib.rs", "line_start": 4, "column_start": 9, "is_primary": true,
                    "suggested_replacement": "_value", "suggestion_applicability": "MachineApplicable"}],
            "children": [{"level": "note", "message": "prefix with an underscore"}], "rendered": "warning: unused variable" }});
        let Some(CargoMessage::Diagnostic(diagnostic)) =
            parse_line(&serde_json::to_vec(&raw).unwrap())
        else {
            panic!("missing diagnostic")
        };
        assert_eq!(diagnostic.raw, raw);
        assert_eq!(
            (diagnostic.file.as_deref(), diagnostic.line, diagnostic.col),
            (Some("src/lib.rs"), Some(4), Some(9))
        );
        assert_eq!(diagnostic.spans.len(), 2);
        assert_eq!(diagnostic.code.as_deref(), Some("unused_variables"));
        assert_eq!(diagnostic.children[0]["level"], "note");
    }

    #[test]
    fn selects_executable_from_bin_artifacts_only() {
        assert!(matches!(parse_line(br#"{"reason":"compiler-artifact","target":{"kind":["bin"]},"executable":"/tmp/app"}"#), Some(CargoMessage::Executable(_))));
        assert!(
            parse_line(
                br#"{"reason":"compiler-artifact","target":{"kind":["lib"]},"executable":null}"#
            )
            .is_none()
        );
        assert!(parse_line(b"unstructured linker output").is_none());
    }
}
