//! Concurrent pipe draining, raw output retention and owned process lifetimes.

use super::events::{Kind, Scope};
use super::process::OwnedChild;
use super::session::{Build, Session};
use super::timing;
use crate::commands::error::{self, CargoMessage, CargoOutcome};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const CHUNK_BYTES: usize = 64 * 1024;

pub fn exit_data(status: ExitStatus, expected: bool) -> Value {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    json!({"success": status.success(), "exit_code": status.code(), "signal": signal, "expected": expected})
}

pub fn run(
    cmd: &mut Command,
    build: &Build,
    stage: &str,
    cargo_json: bool,
) -> Result<CargoOutcome> {
    let span = build.session.start_span(
        timing::stage_name(stage),
        &build.scope,
        Some(build.span_id()),
        json!({
            "stage": stage,
            "program": cmd.get_program().to_string_lossy(),
            "args": cmd.get_args().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>()
        }),
    );
    build.session.emit(
        Kind::StageStarted,
        &build.scope,
        json!({"stage": stage,
        "program": cmd.get_program().to_string_lossy(),
        "args": cmd.get_args().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>() }),
    );
    let result = run_inner(cmd, build, stage, cargo_json);
    match &result {
        Ok((status, _)) => {
            let mut data = exit_data(*status, build.session.stopping.load(Ordering::SeqCst));
            data["stage"] = json!(stage);
            build.session.emit(Kind::StageFinished, &build.scope, data);
            span.finish(
                if status.success() {
                    "ok"
                } else if build.session.stopping.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "failed"
                },
                None,
            );
        }
        Err(error) => {
            build.session.emit(
                Kind::StageFinished,
                &build.scope,
                json!({"stage": stage, "success": false, "error": format!("{error:#}")}),
            );
            let message = format!("{error:#}");
            span.finish("failed", Some(&message));
        }
    }
    let (status, executable) = result?;
    Ok(CargoOutcome {
        success: status.success(),
        executable,
    })
}

fn run_inner(
    cmd: &mut Command,
    build: &Build,
    stage: &str,
    cargo_json: bool,
) -> Result<(ExitStatus, Option<PathBuf>)> {
    if build.session.stopping.load(Ordering::SeqCst) {
        anyhow::bail!("live session is stopping");
    }
    let mut child = OwnedChild::spawn(cmd).with_context(|| format!("starting {stage}"))?;
    let stdout = child.stdout().context("capturing stdout")?;
    let stderr = child.stderr().context("capturing stderr")?;
    thread::scope(|threads| {
        let out = threads.spawn(|| {
            drain(
                stdout,
                &build.session,
                &build.scope,
                stage,
                "stdout",
                cargo_json,
            )
        });
        let err =
            threads.spawn(|| drain(stderr, &build.session, &build.scope, stage, "stderr", false));
        let status = loop {
            if build.session.stopping.load(Ordering::SeqCst) {
                let _ = child.terminate();
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    let _ = child.terminate();
                    break Err(error);
                }
            }
        };
        let executable = out
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
        err.join()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
        Ok((status?, executable))
    })
}

pub fn step(label: &str, cmd: &mut Command, build: &Build) -> Result<()> {
    if !run(cmd, build, label, false)?.success {
        anyhow::bail!("{label} failed");
    }
    Ok(())
}

fn read_chunk(reader: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let mut result = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(result);
        }
        let end = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        let take = end.min(CHUNK_BYTES - result.len());
        result.extend_from_slice(&available[..take]);
        reader.consume(take);
        if result.last() == Some(&b'\n') || result.len() == CHUNK_BYTES {
            return Ok(result);
        }
    }
}

pub fn terminal(bytes: &[u8], stderr: bool) {
    if stderr {
        let mut writer = std::io::stderr().lock();
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    } else {
        let mut writer = std::io::stdout().lock();
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }
}

fn drain(
    reader: impl Read,
    session: &Session,
    scope: &Scope,
    stage: &str,
    stream: &str,
    cargo_json: bool,
) -> Result<Option<PathBuf>> {
    let mut reader = BufReader::new(reader);
    let mut executable = None;
    let mut fragmented = false;
    loop {
        let bytes = read_chunk(&mut reader)?;
        if bytes.is_empty() {
            break;
        }
        let reference = session.output(scope, stage, stream, &bytes, fragmented);
        let complete = bytes.last() == Some(&b'\n') || bytes.len() < CHUNK_BYTES;
        if cargo_json && !fragmented && complete {
            match error::parse_line(&bytes) {
                Some(CargoMessage::Diagnostic(diagnostic)) => {
                    let mut data = serde_json::to_value(&diagnostic)?;
                    data["log"] = json!(reference);
                    session.emit(Kind::Diagnostic, scope, data);
                    if diagnostic.rendered.is_empty() {
                        let location = match (&diagnostic.file, diagnostic.line, diagnostic.col) {
                            (Some(file), Some(line), Some(col)) => format!("{file}:{line}:{col}: "),
                            (Some(file), Some(line), None) => format!("{file}:{line}: "),
                            (Some(file), None, _) => format!("{file}: "),
                            _ => String::new(),
                        };
                        terminal(
                            format!("{}{}: {}\n", location, diagnostic.level, diagnostic.message)
                                .as_bytes(),
                            true,
                        );
                    } else {
                        terminal(diagnostic.rendered.as_bytes(), true);
                    }
                }
                Some(CargoMessage::Executable(path)) => executable = Some(path),
                None if serde_json::from_slice::<Value>(&bytes).is_err() => terminal(&bytes, true),
                _ => {}
            }
        } else if !cargo_json {
            terminal(&bytes, stream == "stderr");
        }
        fragmented = !complete;
    }
    Ok(executable)
}

pub struct AppProcess {
    child: Arc<Mutex<OwnedChild>>,
    expected: Arc<AtomicBool>,
    monitor: Option<JoinHandle<()>>,
    readers: Vec<JoinHandle<()>>,
}

impl AppProcess {
    pub fn spawn(cmd: &mut Command, session: Arc<Session>, scope: Scope) -> Result<Self> {
        let launch_span = session.start_span("app.launch", &scope, None, json!({}));
        let mut child = match OwnedChild::spawn(cmd) {
            Ok(child) => child,
            Err(error) => {
                session.emit(
                    Kind::AppLaunchFailed,
                    &scope,
                    json!({"error": error.to_string()}),
                );
                let message = error.to_string();
                launch_span.finish("failed", Some(&message));
                return Err(error).context("launching desktop app");
            }
        };
        let stdout = child.stdout().context("app stdout")?;
        let stderr = child.stderr().context("app stderr")?;
        session.emit(Kind::AppStarted, &scope, json!({"pid": child.id()}));
        launch_span.finish("ok", None);
        let mut process = Self {
            child: Arc::new(Mutex::new(child)),
            expected: Arc::new(AtomicBool::new(false)),
            monitor: None,
            readers: Vec::new(),
        };
        for (reader, stream) in [
            (Box::new(stdout) as Box<dyn Read + Send>, "stdout"),
            (Box::new(stderr) as Box<dyn Read + Send>, "stderr"),
        ] {
            let session = session.clone();
            let scope = scope.clone();
            process.readers.push(thread::Builder::new().name(format!("gpui-app-{stream}")).spawn(move || {
                if let Err(error) = drain(reader, &session, &scope, "app", stream, false) {
                    session.emit(Kind::AppLog, &scope, json!({"level": "error", "target": "capture", "message": error.to_string()}));
                }
            })?);
        }
        let child = process.child.clone();
        let expected = process.expected.clone();
        process.monitor = Some(thread::Builder::new().name("gpui-app-exit".into()).spawn(move || loop {
            let status = child.lock().unwrap_or_else(|e| e.into_inner()).try_wait();
            match status {
                Ok(Some(status)) => {
                    session.emit(Kind::AppExited, &scope, exit_data(status, expected.load(Ordering::SeqCst)));
                    terminal(format!("[live] app {} exited: {status}\n", scope.run_id.as_deref().unwrap_or("unknown")).as_bytes(), true);
                    break;
                }
                Err(error) => {
                    session.emit(Kind::AppLog, &scope, json!({"level": "error", "target": "process_monitor", "message": error.to_string()}));
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
            }
        })?);
        Ok(process)
    }
}

impl Drop for AppProcess {
    fn drop(&mut self) {
        self.expected.store(true, Ordering::SeqCst);
        let _ = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .terminate();
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}
