use crate::devserver::control::{self, ApiError, Command, Reply};
use crate::devserver::events::{self, Event, Kind, Page, SCHEMA_VERSION};
use anyhow::Result;
use clap::{Args, Subcommand};
use gpui_dev_protocol::ARTIFACT_CHUNK_BYTES;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

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
    /// List windows registered by the current run and their UI heartbeat state
    Windows,
    /// Request a fresh build through the live supervisor
    Build,
    /// Submit a version-bound UI observation
    Observe {
        /// Ensure the actively watched inputs are rebuilt before observing
        #[arg(long)]
        sync: bool,
        /// Select one registered window when multiple windows are open
        #[arg(long, value_name = "WINDOW_ID")]
        window: Option<String>,
        /// Required observation capability or alias; may be repeated or comma-separated
        #[arg(long, value_name = "CAPABILITY")]
        require: Vec<String>,
        /// Total operation deadline, from 1ms through 120s
        #[arg(long, default_value = "30s", value_parser = parse_operation_timeout)]
        timeout: u64,
        /// Return after submission rather than waiting for the operation terminal state
        #[arg(long = "async")]
        asynchronous: bool,
    },
    /// Query a bounded semantics tree attached to a completed observation
    Query {
        /// Observation id returned by a successful observe operation
        #[arg(long)]
        observation: String,
        /// Exact temporary node reference from this observation
        #[arg(long = "node-ref")]
        node_ref: Option<String>,
        /// Stable logical id, when the runtime exports one
        #[arg(long = "id")]
        logical_id: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        /// Project only these fields; may be repeated or comma-separated
        #[arg(long = "field")]
        fields: Vec<String>,
        /// Opaque cursor returned by a previous query
        #[arg(long)]
        cursor: Option<String>,
        /// Maximum nodes to return, capped at 200
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// Summarize semantic changes between two completed observations
    Diff {
        /// Earlier observation id
        #[arg(long)]
        before: String,
        /// Later observation id
        #[arg(long)]
        after: String,
        /// Maximum change records to return, capped at 200
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// Query or cancel an asynchronous supervisor operation
    Operation {
        #[command(subcommand)]
        command: OperationCommand,
    },
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
    /// Inspect, download or pin a verified artifact
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
}

#[derive(Subcommand)]
pub enum OperationCommand {
    /// Read one operation snapshot
    Get {
        operation_id: String,
        /// Wait for a terminal transition or until this bounded duration elapses
        #[arg(long, default_value = "0", value_parser = parse_timeout)]
        wait: u64,
    },
    /// Cancel a queued or running operation
    Cancel { operation_id: String },
}

#[derive(Subcommand)]
pub enum ArtifactCommand {
    /// Show artifact metadata and publication state
    Info { artifact_id: String },
    /// Download a published artifact by its opaque id
    Get {
        artifact_id: String,
        /// Output file to create
        #[arg(long, value_name = "PATH")]
        output: PathBuf,
        /// Replace an existing regular file
        #[arg(long)]
        overwrite: bool,
    },
    /// Keep an artifact beyond the normal retention period
    Pin { artifact_id: String },
    /// Remove the retention pin from an artifact
    Unpin { artifact_id: String },
}

pub fn parse_timeout(value: &str) -> std::result::Result<u64, String> {
    parse_duration(
        value,
        control::MAX_WAIT_MS,
        "timeout must be between 0 and 30s (e.g. 500ms or 30s)",
    )
}

pub fn parse_operation_timeout(value: &str) -> std::result::Result<u64, String> {
    parse_duration(
        value,
        120_000,
        "timeout must be between 1ms and 120s (e.g. 500ms or 30s)",
    )
    .and_then(|timeout| {
        if timeout == 0 {
            Err("timeout must be between 1ms and 120s (e.g. 500ms or 30s)".into())
        } else {
            Ok(timeout)
        }
    })
}

fn parse_duration(value: &str, maximum: u64, message: &str) -> std::result::Result<u64, String> {
    let (number, multiplier) = if let Some(n) = value.strip_suffix("ms") {
        (n, 1)
    } else {
        (value.strip_suffix('s').unwrap_or(value), 1000)
    };
    let ms = number
        .parse::<u64>()
        .ok()
        .and_then(|v| v.checked_mul(multiplier))
        .filter(|v| *v <= maximum);
    ms.ok_or_else(|| message.into())
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
                    &Reply::failure(
                        args.session.as_deref().unwrap_or(""),
                        control::next_request_id("cli.error"),
                        gpui_dev_protocol::V2Error {
                            code: error.code.clone(),
                            message: error.message.clone(),
                            details: error.details,
                            retryable: false,
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
    if let DevCommand::Artifact { command } = &args.command {
        let result = execute_artifact(&registration, command)?;
        if args.json {
            write_value(
                &Reply::success(
                    &registration.session_id,
                    control::next_request_id("dev.artifact"),
                    result,
                ),
                false,
            )?;
        } else {
            write_value(&result, true)?;
        }
        return Ok(());
    }
    if let DevCommand::Operation { command } = &args.command {
        let result = execute_operation(&registration, command)?;
        if args.json {
            write_value(
                &Reply::success(
                    &registration.session_id,
                    control::next_request_id("dev.operation"),
                    result,
                ),
                false,
            )?;
        } else {
            write_value(&result, true)?;
        }
        return Ok(());
    }
    if let DevCommand::Observe {
        sync,
        window,
        require,
        timeout,
        asynchronous,
    } = &args.command
    {
        let result = execute_observe(
            &registration,
            *sync,
            window.clone(),
            require.clone(),
            *timeout,
            *asynchronous,
        )?;
        if args.json {
            write_value(
                &Reply::success(
                    &registration.session_id,
                    control::next_request_id("dev.observe"),
                    result,
                ),
                false,
            )?;
        } else {
            write_value(&result, true)?;
        }
        return Ok(());
    }
    if let DevCommand::Query {
        observation,
        node_ref,
        logical_id,
        role,
        name,
        parent,
        fields,
        cursor,
        limit,
    } = &args.command
    {
        let result = query_request(
            &registration,
            observation,
            node_ref.clone(),
            logical_id.clone(),
            role.clone(),
            name.clone(),
            parent.clone(),
            fields.clone(),
            cursor.clone(),
            *limit,
        )?;
        if args.json {
            write_value(
                &Reply::success(
                    &registration.session_id,
                    control::next_request_id("dev.query"),
                    result,
                ),
                false,
            )?;
        } else {
            write_value(&result, true)?;
        }
        return Ok(());
    }
    if let DevCommand::Diff {
        before,
        after,
        limit,
    } = &args.command
    {
        let result = diff_request(&registration, before, after, *limit)?;
        if args.json {
            write_value(
                &Reply::success(
                    &registration.session_id,
                    control::next_request_id("dev.diff"),
                    result,
                ),
                false,
            )?;
        } else {
            write_value(&result, true)?;
        }
        return Ok(());
    }
    let request_id = control::next_request_id("dev");
    let (mut after, timeout, follow) = match &args.command {
        DevCommand::Events {
            after,
            timeout,
            follow,
        } => (*after, *timeout, *follow),
        _ => (0, 0, false),
    };
    loop {
        let command = match &args.command {
            DevCommand::Status => Command::Status,
            DevCommand::Diagnostics => Command::Diagnostics,
            DevCommand::Windows => Command::Windows,
            DevCommand::Build => Command::Build,
            DevCommand::Observe { .. } => unreachable!("observe commands return above"),
            DevCommand::Query { .. } => unreachable!("query commands return above"),
            DevCommand::Diff { .. } => unreachable!("diff commands return above"),
            DevCommand::Operation { .. } => unreachable!("operation commands return above"),
            DevCommand::Events { .. } => Command::Events {
                after,
                timeout_ms: if follow && timeout == 0 {
                    30_000
                } else {
                    timeout
                },
            },
            DevCommand::Artifact { .. } => unreachable!("artifact commands return above"),
        };
        let reply = match control::request(&registration, &request_id, command) {
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
                .map(Into::into)
                .unwrap_or_else(|| ApiError::new("request_failed", "Control request failed")));
        }
        if matches!(&args.command, DevCommand::Events { .. }) {
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

fn execute_observe(
    registration: &control::Registration,
    sync: bool,
    window_id: Option<String>,
    require: Vec<String>,
    timeout_ms: u64,
    asynchronous: bool,
) -> std::result::Result<serde_json::Value, ApiError> {
    let request_id = control::next_request_id("dev.observe");
    let reply = control::request(
        registration,
        &request_id,
        Command::Observe {
            sync,
            window_id,
            require,
            deadline_ms: timeout_ms,
        },
    )
    .map_err(|error| ApiError::new("connection_failed", error.to_string()))?;
    if !reply.ok {
        return Err(reply
            .error
            .map(Into::into)
            .unwrap_or_else(|| ApiError::new("request_failed", "Observe request failed")));
    }
    let submitted = reply.result.unwrap_or_default();
    if asynchronous {
        return Ok(submitted);
    }
    let operation_id = submitted["operation_id"].as_str().ok_or_else(|| {
        ApiError::new("invalid_response", "Observe submission has no operation_id")
    })?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
            .min(control::MAX_WAIT_MS);
        let current = operation_request(
            registration,
            Command::OperationGet {
                operation_id: operation_id.to_owned(),
                wait_ms: remaining_ms,
            },
        )?;
        let state = current["state"].as_str().unwrap_or("unknown");
        if matches!(
            state,
            "succeeded" | "failed" | "cancelled" | "timed_out" | "superseded" | "unknown"
        ) {
            if state == "succeeded" {
                return Ok(current);
            }
            let error = current["error"].clone();
            return Err(ApiError {
                code: error["code"].as_str().unwrap_or("operation_failed").into(),
                message: error["message"]
                    .as_str()
                    .unwrap_or("observe operation did not succeed")
                    .into(),
                details: Some(json!({"operation_id": operation_id, "operation": current})),
            });
        }
        if Instant::now() >= deadline {
            return Err(ApiError {
                code: "timed_out".into(),
                message: "observe operation did not reach a terminal state before its deadline"
                    .into(),
                details: Some(json!({"operation_id": operation_id})),
            });
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[allow(clippy::too_many_arguments)]
fn query_request(
    registration: &control::Registration,
    observation_id: &str,
    node_ref: Option<String>,
    logical_id: Option<String>,
    role: Option<String>,
    name: Option<String>,
    parent: Option<String>,
    fields: Vec<String>,
    cursor: Option<String>,
    limit: u32,
) -> std::result::Result<serde_json::Value, ApiError> {
    let reply = control::request(
        registration,
        &control::next_request_id("dev.query"),
        Command::Query {
            observation_id: observation_id.to_owned(),
            node_ref,
            logical_id,
            role_name: role,
            name,
            parent,
            fields,
            cursor,
            limit,
        },
    )
    .map_err(|error| ApiError::new("connection_failed", error.to_string()))?;
    if reply.ok {
        Ok(reply.result.unwrap_or_default())
    } else {
        Err(reply
            .error
            .map(Into::into)
            .unwrap_or_else(|| ApiError::new("request_failed", "Query request failed")))
    }
}

fn diff_request(
    registration: &control::Registration,
    before_observation_id: &str,
    after_observation_id: &str,
    limit: u32,
) -> std::result::Result<serde_json::Value, ApiError> {
    let reply = control::request(
        registration,
        &control::next_request_id("dev.diff"),
        Command::Diff {
            before_observation_id: before_observation_id.to_owned(),
            after_observation_id: after_observation_id.to_owned(),
            limit,
        },
    )
    .map_err(|error| ApiError::new("connection_failed", error.to_string()))?;
    if reply.ok {
        Ok(reply.result.unwrap_or_default())
    } else {
        Err(reply
            .error
            .map(Into::into)
            .unwrap_or_else(|| ApiError::new("request_failed", "Diff request failed")))
    }
}

fn execute_operation(
    registration: &control::Registration,
    command: &OperationCommand,
) -> std::result::Result<serde_json::Value, ApiError> {
    let request = match command {
        OperationCommand::Get { operation_id, wait } => Command::OperationGet {
            operation_id: operation_id.clone(),
            wait_ms: *wait,
        },
        OperationCommand::Cancel { operation_id } => Command::OperationCancel {
            operation_id: operation_id.clone(),
        },
    };
    operation_request(registration, request)
}

fn operation_request(
    registration: &control::Registration,
    request: Command,
) -> std::result::Result<serde_json::Value, ApiError> {
    let reply = control::request(
        registration,
        &control::next_request_id("dev.operation"),
        request,
    )
    .map_err(|error| ApiError::new("connection_failed", error.to_string()))?;
    if !reply.ok {
        return Err(reply
            .error
            .map(Into::into)
            .unwrap_or_else(|| ApiError::new("request_failed", "Operation request failed")));
    }
    Ok(reply.result.unwrap_or_default())
}

fn execute_artifact(
    registration: &control::Registration,
    command: &ArtifactCommand,
) -> std::result::Result<serde_json::Value, ApiError> {
    match command {
        ArtifactCommand::Info { artifact_id } => artifact_request(
            registration,
            Command::ArtifactInfo {
                artifact_id: artifact_id.clone(),
            },
        ),
        ArtifactCommand::Get {
            artifact_id,
            output,
            overwrite,
        } => download_artifact(registration, artifact_id, output, *overwrite),
        ArtifactCommand::Pin { artifact_id } => artifact_request(
            registration,
            Command::ArtifactPin {
                artifact_id: artifact_id.clone(),
                pinned: true,
            },
        ),
        ArtifactCommand::Unpin { artifact_id } => artifact_request(
            registration,
            Command::ArtifactPin {
                artifact_id: artifact_id.clone(),
                pinned: false,
            },
        ),
    }
}

fn artifact_request(
    registration: &control::Registration,
    command: Command,
) -> std::result::Result<serde_json::Value, ApiError> {
    let request_id = control::next_request_id("dev.artifact");
    let reply = control::request(registration, &request_id, command)
        .map_err(|error| ApiError::new("connection_failed", error.to_string()))?;
    if reply.ok {
        Ok(reply.result.unwrap_or_default())
    } else {
        Err(reply
            .error
            .map(Into::into)
            .unwrap_or_else(|| ApiError::new("request_failed", "Artifact request failed")))
    }
}

fn download_artifact(
    registration: &control::Registration,
    artifact_id: &str,
    output: &Path,
    overwrite: bool,
) -> std::result::Result<serde_json::Value, ApiError> {
    let info = artifact_request(
        registration,
        Command::ArtifactInfo {
            artifact_id: artifact_id.to_owned(),
        },
    )?;
    if info["status"] != "published" {
        return Err(ApiError::new(
            "invalid_artifact_state",
            "Only published artifacts can be downloaded",
        ));
    }
    let declared_bytes = info["declared_bytes"]
        .as_u64()
        .ok_or_else(|| ApiError::new("invalid_response", "Artifact size is missing"))?;
    let expected_hash = info["sha256"]
        .as_str()
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ApiError::new("invalid_response", "Artifact SHA-256 is invalid"))?;

    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(ApiError::new(
            "invalid_output_path",
            "Output parent directory does not exist",
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(output) {
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return Err(ApiError::new(
                "unsafe_output_path",
                "Output must not be a symbolic link or directory",
            ));
        }
        if !overwrite {
            return Err(ApiError::new(
                "destination_exists",
                "Output file already exists; pass --overwrite to replace it",
            ));
        }
    }

    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| ApiError::new("output_failed", error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut offset = 0u64;
    let mut saw_eof = false;
    while offset < declared_bytes {
        let length = (declared_bytes - offset).min(ARTIFACT_CHUNK_BYTES as u64) as u32;
        let chunk = artifact_request(
            registration,
            Command::ArtifactRead {
                artifact_id: artifact_id.to_owned(),
                offset,
                length,
            },
        )?;
        let actual_offset = chunk["offset"]
            .as_u64()
            .ok_or_else(|| ApiError::new("invalid_response", "Artifact chunk has no offset"))?;
        if actual_offset != offset {
            return Err(ApiError::new(
                "invalid_response",
                format!("Expected artifact offset {offset}, received {actual_offset}"),
            ));
        }
        let encoded = chunk["data"]
            .as_str()
            .ok_or_else(|| ApiError::new("invalid_response", "Artifact chunk has no data"))?;
        let bytes = crate::devserver::protocol::b64::decode(encoded)
            .ok_or_else(|| ApiError::new("invalid_response", "Artifact chunk is invalid base64"))?;
        if bytes.is_empty()
            || bytes.len() as u64 > declared_bytes - offset
            || chunk["bytes"].as_u64() != Some(bytes.len() as u64)
        {
            return Err(ApiError::new(
                "invalid_response",
                "Artifact chunk length does not match its response metadata",
            ));
        }
        temporary
            .write_all(&bytes)
            .map_err(|error| ApiError::new("output_failed", error.to_string()))?;
        hasher.update(&bytes);
        offset += bytes.len() as u64;
        saw_eof = chunk["eof"].as_bool().unwrap_or(false);
        if saw_eof != (offset == declared_bytes) {
            return Err(ApiError::new(
                "invalid_response",
                "Artifact stream ended before or after its declared size",
            ));
        }
    }
    if offset != declared_bytes || !saw_eof {
        return Err(ApiError::new(
            "invalid_response",
            "Artifact stream did not terminate at its declared size",
        ));
    }
    let actual_hash = format!("{:x}", hasher.finalize());
    if actual_hash != expected_hash {
        return Err(ApiError::new(
            "checksum_mismatch",
            "Downloaded artifact does not match its declared SHA-256",
        ));
    }
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| ApiError::new("output_failed", error.to_string()))?;
    if overwrite {
        temporary
            .persist(output)
            .map_err(|error| ApiError::new("output_failed", error.error.to_string()))?;
    } else {
        temporary.persist_noclobber(output).map_err(|error| {
            let code = if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                "destination_exists"
            } else {
                "output_failed"
            };
            ApiError::new(code, error.error.to_string())
        })?;
    }
    Ok(json!({
        "artifact_id": artifact_id,
        "output": output,
        "bytes": declared_bytes,
        "sha256": format!("sha256:{actual_hash}"),
        "downloaded": true,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devserver::control::ControlServer;
    use crate::devserver::session::Session;
    use gpui_dev_protocol::ArtifactKind;

    #[test]
    fn operation_timeout_requires_a_nonzero_value_within_the_total_deadline() {
        assert_eq!(parse_operation_timeout("500ms").unwrap(), 500);
        assert_eq!(parse_operation_timeout("120s").unwrap(), 120_000);
        assert!(parse_operation_timeout("0").is_err());
        assert!(parse_operation_timeout("121s").is_err());
    }

    #[test]
    fn artifact_get_downloads_chunks_verifies_hash_and_respects_overwrite() {
        let project = tempfile::tempdir().unwrap();
        let session = Session::start(project.path(), "test", "desktop:test").unwrap();
        let server = ControlServer::start(session.clone()).unwrap();
        let bytes = vec![42u8; ARTIFACT_CHUNK_BYTES + 31];
        let digest = Sha256::digest(&bytes);
        let transfer = "cli-artifact-transfer";
        session
            .artifacts
            .begin(gpui_dev_protocol::ArtifactManifest {
                artifact_id: "cli-artifact".into(),
                transfer_id: transfer.into(),
                run_id: None,
                kind: ArtifactKind::Blob,
                mime: "application/octet-stream".into(),
                declared_bytes: bytes.len() as u64,
                sha256: format!("sha256:{digest:x}"),
            })
            .unwrap();
        for chunk in bytes.chunks(ARTIFACT_CHUNK_BYTES) {
            let offset = session
                .artifacts
                .info("cli-artifact")
                .unwrap()
                .received_bytes;
            session
                .artifacts
                .write_chunk("cli-artifact", transfer, offset, chunk)
                .unwrap();
        }
        session.artifacts.finish("cli-artifact", transfer).unwrap();

        let output = project.path().join("download.bin");
        let result =
            download_artifact(&server.registration, "cli-artifact", &output, false).unwrap();
        assert_eq!(result["bytes"], bytes.len() as u64);
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(
            download_artifact(&server.registration, "cli-artifact", &output, false)
                .unwrap_err()
                .code,
            "destination_exists"
        );
        assert!(download_artifact(&server.registration, "cli-artifact", &output, true).is_ok());
        assert_eq!(fs::read(&output).unwrap(), bytes);
    }
}
