use crate::devserver::control::{self, ApiError, Command, Reply};
use crate::devserver::events::{self, Event, Kind, Page, SCHEMA_VERSION};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::json;
use std::io::Write;

#[derive(Args)]
pub struct DevArgs {
    /// Select a live session when multiple sessions are running
    #[arg(long, global = true)]
    pub session: Option<String>,
    /// Emit machine-readable JSON (NDJSON with events --follow)
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: DevCommand,
}

#[derive(Subcommand)]
pub enum DevCommand {
    /// Show desired/running versions, build state and available capabilities
    Status,
    /// Read current compiler diagnostics and unverified runtime problems
    Diagnostics,
    /// Replay events after a cursor, then optionally wait for new events
    Events {
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Wait for new events, e.g. 0, 500ms or 30s (maximum 30s)
        #[arg(long, default_value = "0", value_parser = parse_timeout)]
        timeout: u64,
        /// Continuously follow events; --json emits one event per line
        #[arg(long)]
        follow: bool,
    },
}

pub fn parse_timeout(value: &str) -> std::result::Result<u64, String> {
    let (number, multiplier) = if let Some(n) = value.strip_suffix("ms") {
        (n, 1)
    } else {
        (value.strip_suffix('s').unwrap_or(value), 1000)
    };
    let ms = number
        .parse::<u64>()
        .ok()
        .and_then(|v| v.checked_mul(multiplier))
        .filter(|v| *v <= control::MAX_WAIT_MS);
    ms.ok_or_else(|| "timeout must be between 0 and 30s (e.g. 500ms or 30s)".into())
}

pub fn handle_dev(args: DevArgs) -> Result<()> {
    match execute(&args) {
        Ok(()) => Ok(()),
        Err(error) => {
            if error.code == "output_closed" {
                return Ok(());
            }
            if args.json {
                let result = write_value(
                    &Reply::error(
                        args.session.as_deref().unwrap_or(""),
                        ApiError {
                            code: error.code.clone(),
                            message: error.message.clone(),
                            details: error.details,
                        },
                    ),
                    false,
                );
                if result.as_ref().is_err_and(|e| e.code == "output_closed") {
                    return Ok(());
                }
            }
            anyhow::bail!("{}: {}", error.code, error.message)
        }
    }
}

fn execute(args: &DevArgs) -> std::result::Result<(), ApiError> {
    let root =
        control::project_root().map_err(|e| ApiError::new("invalid_project", e.to_string()))?;
    let registration = control::discover(&root, args.session.as_deref())?;
    let (mut after, timeout, follow) = match args.command {
        DevCommand::Events {
            after,
            timeout,
            follow,
        } => (after, timeout, follow),
        _ => (0, 0, false),
    };
    loop {
        let command = match args.command {
            DevCommand::Status => Command::Status,
            DevCommand::Diagnostics => Command::Diagnostics,
            DevCommand::Events { .. } => Command::Events {
                after,
                timeout_ms: if follow && timeout == 0 {
                    30_000
                } else {
                    timeout
                },
            },
        };
        let reply = match control::request(&registration, command) {
            Ok(reply) => reply,
            // The supervisor is gone. If the session actually ended, following
            // ends here: its closing events are already journaled on disk, so
            // replay the ones the follower has not seen rather than reporting a
            // transport failure. A live supervisor that merely dropped one
            // connection still reports the error.
            Err(error) if follow => {
                let archived =
                    events::archived(&control::session_dir(&registration)).unwrap_or_default();
                if !archived
                    .iter()
                    .any(|event| event.kind == Kind::SessionEnded)
                {
                    return Err(ApiError::new("connection_failed", error.to_string()));
                }
                // Same contract as a live long poll: a cursor whose events were
                // already rotated away is reported, never silently skipped.
                if let Some(first) = archived.iter().find(|event| event.seq > after)
                    && first.seq > after + 1
                {
                    return Err(ApiError { code: "cursor_expired".into(), message: "Events were evicted. Read status/diagnostics and resume from earliest_seq - 1, or from the status seq.".into(),
                        details: Some(json!({"session_id": registration.session_id, "earliest_seq": first.seq,
                            "last_seq": archived.last().map(|event| event.seq).unwrap_or(0),
                            "requested_after": after, "schema_version": SCHEMA_VERSION})) });
                }
                for event in archived.iter().filter(|event| event.seq > after) {
                    emit_event(event, args.json)?;
                }
                return Ok(());
            }
            Err(error) => return Err(ApiError::new("connection_failed", error.to_string())),
        };
        if !reply.ok {
            return Err(reply
                .error
                .unwrap_or_else(|| ApiError::new("request_failed", "Control request failed")));
        }
        if matches!(args.command, DevCommand::Events { .. }) {
            let page: Page = serde_json::from_value(reply.result.clone().unwrap_or_default())
                .map_err(|e| ApiError::new("invalid_response", e.to_string()))?;
            if page.gap {
                return Err(ApiError { code: "cursor_expired".into(), message: "Events were evicted. Read status/diagnostics and resume from earliest_seq - 1, or from the status seq.".into(),
                    details: Some(json!({"session_id": registration.session_id, "earliest_seq": page.earliest_seq,
                        "last_seq": page.last_seq, "requested_after": after, "schema_version": SCHEMA_VERSION})) });
            }
            if follow {
                for event in &page.events {
                    emit_event(event, args.json)?;
                }
                after = page.next_seq;
                if page.ended && !page.has_more {
                    return Ok(());
                }
                continue;
            }
        }
        if args.json {
            write_value(&reply, false)?;
        } else {
            write_value(&reply.result, true)?;
        }
        return Ok(());
    }
}

/// One event, in the shape `--json` (one JSON object per line) or human mode emits.
fn emit_event(event: &Event, json: bool) -> std::result::Result<(), ApiError> {
    if json {
        write_value(event, false)
    } else {
        write_line(&format!("{} {:?} {}", event.seq, event.kind, event.data))
    }
}

fn write_value(value: &impl serde::Serialize, pretty: bool) -> std::result::Result<(), ApiError> {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(|e| ApiError::new("serialization_failed", e.to_string()))?;
    write_line(&text)
}

fn write_line(text: &str) -> std::result::Result<(), ApiError> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{text}")
        .and_then(|()| stdout.flush())
        .map_err(|e| {
            ApiError::new(
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    "output_closed"
                } else {
                    "output_failed"
                },
                e.to_string(),
            )
        })
}
