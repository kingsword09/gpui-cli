//! Structured compile diagnostics from `cargo build --message-format=json`.
//!
//! The live loop rebuilds with cargo's JSON output so that compile and link
//! errors arrive as data (file, line, column, rendered snippet) instead of
//! text that has to be scraped from stderr.

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// One compiler error, with the location the live loop can point at.
pub struct Diagnostic {
    pub level: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub message: String,
    /// The full rustc-rendered diagnostic, including the source snippet.
    pub rendered: String,
}

/// Everything the live loop needs from one cargo build.
pub struct CargoOutcome {
    pub success: bool,
    /// Path of the produced binary (reported only for `bin` targets).
    pub executable: Option<PathBuf>,
    pub errors: Vec<Diagnostic>,
}

/// Runs cargo with `--message-format=json`, collecting structured errors while
/// cargo's own progress output still streams to the terminal (stderr inherited).
pub fn run_cargo_json(cmd: &mut Command) -> Result<CargoOutcome> {
    cmd.arg("--message-format=json").stdout(Stdio::piped());
    let mut child = cmd.spawn().context("failed to spawn cargo")?;
    let stdout = child
        .stdout
        .take()
        .context("cargo stdout was not captured")?;

    let mut outcome = CargoOutcome {
        success: false,
        executable: None,
        errors: Vec::new(),
    };
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading cargo output")?;
        apply_json_line(&line, &mut outcome);
    }
    let status = child.wait().context("waiting for cargo")?;
    outcome.success = status.success();
    Ok(outcome)
}

fn apply_json_line(line: &str, outcome: &mut CargoOutcome) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    match value.get("reason").and_then(Value::as_str) {
        Some("compiler-message") => collect_message(&value, outcome),
        Some("compiler-artifact") => collect_artifact(&value, outcome),
        _ => {}
    }
}

fn collect_message(value: &Value, outcome: &mut CargoOutcome) {
    let Some(message) = value.get("message") else {
        return;
    };
    let level = message
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // `level` is `error`, `error: internal compiler error`, `warning`, …
    if !level.starts_with("error") {
        return;
    }
    let message_text = message
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let rendered = message
        .get("rendered")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (file, line, col) = primary_span(message);
    outcome.errors.push(Diagnostic {
        level: level.to_string(),
        file,
        line,
        col,
        message: message_text,
        rendered,
    });
}

/// The span to point at: the primary one if any, otherwise the first.
fn primary_span(message: &Value) -> (String, u32, u32) {
    let span = message
        .get("spans")
        .and_then(Value::as_array)
        .and_then(|spans| {
            spans
                .iter()
                .find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true))
                .or_else(|| spans.first())
        });
    let Some(span) = span else {
        return (String::new(), 0, 0);
    };
    (
        span.get("file_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        span.get("line_start").and_then(Value::as_u64).unwrap_or(0) as u32,
        span.get("column_start")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    )
}

fn collect_artifact(value: &Value, outcome: &mut CargoOutcome) {
    let Some(executable) = value.get("executable").and_then(Value::as_str) else {
        return;
    };
    let kinds = value.pointer("/target/kind").and_then(Value::as_array);
    let is_bin = kinds
        .map(|k| k.iter().any(|x| x.as_str() == Some("bin")))
        .unwrap_or(false);
    if is_bin {
        outcome.executable = Some(PathBuf::from(executable));
    }
}

/// Renders errors using rustc's own formatting (location, snippet, notes).
pub fn render_errors(errors: &[Diagnostic]) {
    if errors.is_empty() {
        return;
    }
    println!();
    for diagnostic in errors {
        if diagnostic.rendered.trim().is_empty() {
            println!(
                "{}",
                format!(
                    "{}:{}:{}: {}",
                    diagnostic.file, diagnostic.line, diagnostic.col, diagnostic.message
                )
                .red()
                .bold()
            );
        } else {
            println!("{}", diagnostic.rendered.trim_end());
        }
    }
    println!("{}", format!("{} error(s)", errors.len()).red().bold());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_error_with_its_primary_span() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","rendered":"error[E0308]: mismatched types\n --> src/lib.rs:4:9\n","spans":[{"file_name":"src/lib.rs","line_start":4,"column_start":9,"is_primary":true}]}}"#;
        let mut outcome = CargoOutcome {
            success: false,
            executable: None,
            errors: Vec::new(),
        };
        apply_json_line(line, &mut outcome);
        assert_eq!(outcome.errors.len(), 1);
        let diagnostic = &outcome.errors[0];
        assert_eq!(diagnostic.file, "src/lib.rs");
        assert_eq!(diagnostic.line, 4);
        assert_eq!(diagnostic.col, 9);
        assert_eq!(diagnostic.message, "mismatched types");
        assert!(diagnostic.rendered.contains("error[E0308]"));
    }

    #[test]
    fn ignores_warnings_and_other_reasons() {
        let mut outcome = CargoOutcome {
            success: false,
            executable: None,
            errors: Vec::new(),
        };
        apply_json_line(
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused","spans":[]}}"#,
            &mut outcome,
        );
        apply_json_line(
            r#"{"reason":"build-finished","success":true}"#,
            &mut outcome,
        );
        apply_json_line("not json at all", &mut outcome);
        assert!(outcome.errors.is_empty());
    }

    #[test]
    fn picks_up_bin_executable_only() {
        let mut outcome = CargoOutcome {
            success: false,
            executable: None,
            errors: Vec::new(),
        };
        apply_json_line(
            r#"{"reason":"compiler-artifact","package_id":"x","target":{"kind":["lib"]},"executable":null,"fresh":false}"#,
            &mut outcome,
        );
        assert!(outcome.executable.is_none());
        apply_json_line(
            r#"{"reason":"compiler-artifact","package_id":"x","target":{"kind":["bin"]},"executable":"/tmp/app","fresh":false}"#,
            &mut outcome,
        );
        assert_eq!(
            outcome.executable.as_deref(),
            Some(std::path::Path::new("/tmp/app"))
        );
    }
}
