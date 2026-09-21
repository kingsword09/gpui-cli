//! Bounded external tool probes. No probe may wait indefinitely or retain an
//! unbounded stdout/stderr buffer.

use super::report::{CheckReport, CheckStatus};
use super::requirements::{CommandSpec, Requirement, RequirementKind};
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ProbeConfig {
    pub per_check: Duration,
    pub total: Duration,
    pub max_output_bytes: usize,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            per_check: DEFAULT_CHECK_TIMEOUT,
            total: DEFAULT_TOTAL_TIMEOUT,
            max_output_bytes: DEFAULT_OUTPUT_BYTES,
        }
    }
}

pub struct ProbeRunner {
    config: ProbeConfig,
    deadline: Instant,
}

impl ProbeRunner {
    pub fn new(config: ProbeConfig) -> Self {
        Self {
            deadline: Instant::now() + config.total,
            config,
        }
    }

    pub fn probe(&mut self, requirements: &[Requirement], cwd: Option<&Path>) -> Vec<CheckReport> {
        requirements
            .iter()
            .map(|requirement| self.probe_one(requirement, cwd))
            .collect()
    }

    fn probe_one(&mut self, requirement: &Requirement, cwd: Option<&Path>) -> CheckReport {
        let mut report = CheckReport::from_requirement(requirement.clone());
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            report.status = optional_status(requirement.required, CheckStatus::Unknown);
            report.reason = "total probe deadline exceeded before this check ran".into();
            report.actual = json!({"timeout": true, "scope": "total"});
            return report;
        }

        match requirement.kind {
            RequirementKind::Command => {
                let Some(command) = &requirement.command else {
                    report.reason = "no command is available for this project-derived check".into();
                    report.actual = json!({"status": "not_implemented"});
                    return report;
                };
                let result = run_command(
                    command,
                    cwd,
                    self.config.per_check.min(remaining),
                    self.config.max_output_bytes,
                );
                apply_command_result(&mut report, command, result);
            }
            RequirementKind::HostPlatform => probe_host(&mut report),
            RequirementKind::Environment | RequirementKind::Directory => {
                probe_environment(&mut report)
            }
            RequirementKind::RustTarget => {
                let Some(command) = &requirement.command else {
                    report.reason = "Rust target probe has no rustup command".into();
                    return report;
                };
                let result = run_command(
                    command,
                    cwd,
                    self.config.per_check.min(remaining),
                    self.config.max_output_bytes,
                );
                apply_rust_target_result(&mut report, result);
            }
        }
        report
    }
}

#[derive(Debug)]
struct Captured {
    bytes: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
}

#[derive(Debug)]
enum CommandState {
    Passed,
    NonZero(Option<i32>),
    NotFound,
    TimedOut,
    SpawnFailed(String),
}

#[derive(Debug)]
struct CommandResult {
    state: CommandState,
    path: Option<PathBuf>,
    stdout: Captured,
    stderr: Captured,
    duration_ms: u64,
}

fn run_command(
    spec: &CommandSpec,
    cwd: Option<&Path>,
    timeout: Duration,
    max_output_bytes: usize,
) -> CommandResult {
    let path = resolve_program(spec, cwd);
    let Some(path) = path else {
        return CommandResult {
            state: CommandState::NotFound,
            path: None,
            stdout: empty_capture(),
            stderr: empty_capture(),
            duration_ms: 0,
        };
    };

    let started = Instant::now();
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandResult {
                state: CommandState::SpawnFailed(error.to_string()),
                path: Some(path),
                stdout: empty_capture(),
                stderr: empty_capture(),
                duration_ms: elapsed_ms(started),
            };
        }
    };
    let stdout = child
        .stdout
        .take()
        .map(|reader| thread::spawn(move || capture(reader, max_output_bytes)));
    let stderr = child
        .stderr
        .take()
        .map(|reader| thread::spawn(move || capture(reader, max_output_bytes)));

    let deadline = Instant::now() + timeout;
    let (state, status) = wait_for_child(&mut child, deadline);
    let stdout = join_capture(stdout);
    let stderr = join_capture(stderr);
    let state = match state {
        WaitState::Exited => {
            if status.is_some_and(|status| status.success()) {
                CommandState::Passed
            } else {
                CommandState::NonZero(status.and_then(|status| status.code()))
            }
        }
        WaitState::TimedOut => CommandState::TimedOut,
        WaitState::Failed(error) => CommandState::SpawnFailed(error),
    };
    CommandResult {
        state,
        path: Some(path),
        stdout,
        stderr,
        duration_ms: elapsed_ms(started),
    }
}

enum WaitState {
    Exited,
    TimedOut,
    Failed(String),
}

fn wait_for_child(
    child: &mut Child,
    deadline: Instant,
) -> (WaitState, Option<std::process::ExitStatus>) {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (WaitState::Exited, Some(status)),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let status = child.wait().ok();
                return (WaitState::TimedOut, status);
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return (WaitState::Failed(error.to_string()), None);
            }
        }
    }
}

fn capture(mut reader: impl Read, max_output_bytes: usize) -> Captured {
    let mut bytes = Vec::with_capacity(max_output_bytes.min(4096));
    let mut total_bytes: usize = 0;
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                total_bytes = total_bytes.saturating_add(count);
                let remaining = max_output_bytes.saturating_sub(bytes.len());
                bytes.extend_from_slice(&buffer[..count.min(remaining)]);
            }
            Err(_) => break,
        }
    }
    Captured {
        bytes,
        total_bytes,
        truncated: total_bytes > max_output_bytes,
    }
}

fn join_capture(handle: Option<thread::JoinHandle<Captured>>) -> Captured {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(empty_capture)
}

fn empty_capture() -> Captured {
    Captured {
        bytes: Vec::new(),
        total_bytes: 0,
        truncated: false,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn resolve_program(spec: &CommandSpec, cwd: Option<&Path>) -> Option<PathBuf> {
    let program = Path::new(&spec.program);
    if program.is_absolute() || program.components().count() > 1 {
        let path = if program.is_absolute() {
            program.to_owned()
        } else {
            cwd.unwrap_or_else(|| Path::new(".")).join(program)
        };
        return path.is_file().then_some(path);
    }
    which::which(&spec.program).ok()
}

fn apply_command_result(report: &mut CheckReport, command: &CommandSpec, result: CommandResult) {
    let state = result.state;
    report.duration_ms = result.duration_ms;
    report.actual = json!({
        "program": command.program,
        "path": result.path,
        "stdout": output_summary(&result.stdout),
        "stderr": output_summary(&result.stderr),
    });
    match state {
        CommandState::Passed => {
            report.status = CheckStatus::Pass;
            report.reason = "command completed successfully".into();
        }
        CommandState::NonZero(code) => {
            report.status = optional_status(report.required, CheckStatus::Fail);
            report.reason = format!("command exited unsuccessfully (code {code:?})");
        }
        CommandState::NotFound => {
            report.status = optional_status(report.required, CheckStatus::Unavailable);
            report.reason = "executable was not found".into();
        }
        CommandState::TimedOut => {
            report.status = optional_status(report.required, CheckStatus::Unknown);
            report.reason = "probe timed out".into();
        }
        CommandState::SpawnFailed(error) => {
            report.status = optional_status(report.required, CheckStatus::Unknown);
            report.reason = format!("could not start command: {error}");
        }
    }
}

fn apply_rust_target_result(report: &mut CheckReport, result: CommandResult) {
    let expected = report.expected["target"].as_str().unwrap_or_default();
    let output = String::from_utf8_lossy(&result.stdout.bytes);
    let installed = output.lines().any(|line| line.trim() == expected);
    let mut actual = match serde_json::to_value(&result.path) {
        Ok(path) => json!({"path": path}),
        Err(_) => json!({}),
    };
    actual["target"] = json!(expected);
    actual["installed"] = json!(installed);
    actual["stdout"] = json!(output_summary(&result.stdout));
    actual["stderr"] = json!(output_summary(&result.stderr));
    report.actual = actual;
    report.duration_ms = result.duration_ms;
    match result.state {
        CommandState::Passed if installed => {
            report.status = CheckStatus::Pass;
            report.reason = "Rust target is installed".into();
        }
        CommandState::Passed => {
            report.status = optional_status(report.required, CheckStatus::Fail);
            report.reason = format!("Rust target {expected} is not installed");
        }
        CommandState::NotFound => {
            report.status = optional_status(report.required, CheckStatus::Unavailable);
            report.reason = "rustup was not found".into();
        }
        CommandState::TimedOut => {
            report.status = optional_status(report.required, CheckStatus::Unknown);
            report.reason = "Rust target probe timed out".into();
        }
        CommandState::NonZero(code) => {
            report.status = optional_status(report.required, CheckStatus::Fail);
            report.reason = format!("rustup exited unsuccessfully (code {code:?})");
        }
        CommandState::SpawnFailed(error) => {
            report.status = optional_status(report.required, CheckStatus::Unknown);
            report.reason = format!("could not start rustup: {error}");
        }
    }
}

fn probe_host(report: &mut CheckReport) {
    let host = normalized_host();
    let expected = report.expected.as_array();
    let matches = expected.is_some_and(|values| values.iter().any(|value| value == &host));
    report.actual = json!({"host_os": host});
    if matches {
        report.status = CheckStatus::Pass;
        report.reason = "host platform is supported".into();
    } else {
        report.status = optional_status(report.required, CheckStatus::Unavailable);
        report.reason = "selected target requires a different host platform".into();
    }
}

fn probe_environment(report: &mut CheckReport) {
    let variables = report.expected["variables"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut source = None;
    let mut exists = false;
    let mut directory = false;
    for variable in variables.iter().filter_map(Value::as_str) {
        if let Ok(value) = std::env::var(variable) {
            let path = Path::new(&value);
            source = Some(variable.to_string());
            exists = path.exists();
            directory = path.is_dir();
            if exists {
                break;
            }
        }
    }
    report.actual = json!({
        "present": source.is_some(),
        "source": source,
        "exists": exists,
        "directory": directory,
    });
    if exists && directory {
        report.status = CheckStatus::Pass;
        report.reason = "environment directory is available".into();
    } else {
        report.status = optional_status(report.required, CheckStatus::Unavailable);
        report.reason = "none of the configured environment paths is an existing directory".into();
    }
}

fn output_summary(output: &Captured) -> Value {
    json!({
        "text": clip(&String::from_utf8_lossy(&output.bytes), 2048),
        "bytes": output.total_bytes,
        "truncated": output.truncated,
        "first_line": first_line(&output.bytes),
    })
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn normalized_host() -> String {
    match std::env::consts::OS {
        "macos" => "macos".into(),
        "windows" => "windows".into(),
        "linux" => "linux".into(),
        other => other.into(),
    }
}

fn optional_status(required: bool, status: CheckStatus) -> CheckStatus {
    if required {
        status
    } else if matches!(status, CheckStatus::Pass) {
        CheckStatus::Pass
    } else {
        CheckStatus::Warning
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::Target;
    use crate::toolchain::requirements::{CommandSpec, Requirement, RequirementKind};
    use std::io::Cursor;

    fn command_requirement(program: &str, args: &[&str], required: bool) -> Requirement {
        Requirement {
            id: "test.command".into(),
            target: Target::Desktop,
            kind: RequirementKind::Command,
            label: program.into(),
            required,
            expected: json!({"executable": true}),
            command: Some(CommandSpec::new(program, args)),
            remediation: vec![],
        }
    }

    #[test]
    fn distinguishes_success_nonzero_and_missing_commands() {
        let requirements = vec![
            command_requirement("rustc", &["--version"], true),
            command_requirement("rustc", &["--definitely-not-a-real-option"], true),
            command_requirement("gpui-command-that-does-not-exist", &[], false),
        ];
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&requirements, None);
        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[1].status, CheckStatus::Fail);
        assert_eq!(reports[2].status, CheckStatus::Warning);
        assert!(reports[1].actual["stderr"]["bytes"].as_u64().is_some());
    }

    #[test]
    fn total_deadline_marks_later_checks_unknown_without_spawning() {
        let requirements = vec![command_requirement("rustc", &["--version"], true)];
        let mut runner = ProbeRunner::new(ProbeConfig {
            per_check: Duration::from_secs(5),
            total: Duration::ZERO,
            max_output_bytes: 16,
        });
        let reports = runner.probe(&requirements, None);
        assert_eq!(reports[0].status, CheckStatus::Unknown);
        assert!(reports[0].reason.contains("deadline"));
    }

    #[test]
    fn output_capture_is_bounded_but_drains_the_reader() {
        let output = capture(Cursor::new(vec![b'x'; 32]), 8);
        assert_eq!(output.bytes.len(), 8);
        assert_eq!(output.total_bytes, 32);
        assert!(output.truncated);
    }

    #[test]
    fn required_and_optional_timeouts_have_different_exit_severity() {
        let required = command_requirement("rustc", &["--version"], true);
        let optional = command_requirement("rustc", &["--version"], false);
        let mut runner = ProbeRunner::new(ProbeConfig {
            per_check: Duration::ZERO,
            total: Duration::from_secs(1),
            max_output_bytes: 16,
        });
        let reports = runner.probe(&[required, optional], None);
        assert_eq!(reports[0].status, CheckStatus::Unknown);
        assert_eq!(reports[1].status, CheckStatus::Warning);
    }
}
