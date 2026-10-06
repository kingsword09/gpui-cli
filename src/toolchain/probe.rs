//! Bounded external tool probes. No probe may wait indefinitely or retain an
//! unbounded stdout/stderr buffer.

use super::report::{CheckReport, CheckStatus};
use super::requirements::{CommandSpec, Requirement, RequirementKind};
use serde_json::{Value, json};
use std::fs;
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
            RequirementKind::ProjectMetadata => probe_project_metadata(&mut report),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VersionExpectation {
    Parseable,
    Reported,
    ProjectSelected,
    AgpCompatible,
}

impl VersionExpectation {
    fn from_expected(expected: &Value) -> Option<Self> {
        match expected["version"].as_str()? {
            "parseable" => Some(Self::Parseable),
            "reported" => Some(Self::Reported),
            "project-selected" => Some(Self::ProjectSelected),
            "AGP-compatible" => Some(Self::AgpCompatible),
            _ => None,
        }
    }

    fn minimum_parts(self) -> usize {
        match self {
            Self::Parseable => 3,
            Self::Reported | Self::ProjectSelected | Self::AgpCompatible => 1,
        }
    }
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
    let version_expectation = VersionExpectation::from_expected(&report.expected);
    let reported_version = version_expectation.and_then(|expectation| {
        extract_reported_version(
            &result.stdout.bytes,
            &result.stderr.bytes,
            expectation.minimum_parts(),
        )
    });
    if version_expectation.is_some() {
        report.actual["version"] = json!(reported_version);
    }
    match state {
        CommandState::Passed => match (version_expectation, reported_version) {
            (Some(_), None) => {
                report.status = optional_status(report.required, CheckStatus::Fail);
                report.reason =
                    "command succeeded but did not report a recognizable version".into();
            }
            (Some(VersionExpectation::ProjectSelected), Some(version)) => {
                match report.expected["selected_version"].as_str() {
                    Some(selected) if selected == version => {
                        report.status = CheckStatus::Pass;
                        report.reason = format!("project-selected version {version} is active");
                    }
                    Some(selected) => {
                        report.status = optional_status(report.required, CheckStatus::Fail);
                        report.reason = format!(
                            "reported version {version} does not match project-selected version {selected}"
                        );
                    }
                    None => {
                        report.status = optional_status(report.required, CheckStatus::Unknown);
                        report.reason = format!(
                            "reported version {version}; project-selected version is unavailable"
                        );
                    }
                }
            }
            (Some(VersionExpectation::AgpCompatible), Some(version)) => {
                match report.expected["minimum_java_major"].as_u64() {
                    Some(minimum) => match numeric_version_at_least(&version, &minimum.to_string())
                    {
                        Some(true) => {
                            report.actual["minimum_java_major"] = json!(minimum);
                            report.status = CheckStatus::Pass;
                            report.reason = format!(
                                "Java {version} meets the modeled minimum {minimum} for this AGP/Gradle pair"
                            );
                        }
                        Some(false) => {
                            report.actual["minimum_java_major"] = json!(minimum);
                            report.status = optional_status(report.required, CheckStatus::Fail);
                            report.reason = format!(
                                "Java {version} is below the modeled minimum {minimum} for this AGP/Gradle pair"
                            );
                        }
                        None => {
                            report.status = optional_status(report.required, CheckStatus::Unknown);
                            report.reason = format!(
                                "reported Java version {version} could not be compared with the modeled minimum {minimum}"
                            );
                        }
                    },
                    None => {
                        report.status = optional_status(report.required, CheckStatus::Unknown);
                        report.reason = format!(
                            "reported version {version}; AGP/Gradle compatibility rule is unavailable"
                        );
                    }
                }
            }
            (Some(expectation), Some(version)) => match report.expected["minimum"].as_str() {
                Some(minimum) => match numeric_version_at_least(&version, minimum) {
                    Some(true) => {
                        report.status = CheckStatus::Pass;
                        report.reason =
                            format!("reported version {version} meets project minimum {minimum}");
                    }
                    Some(false) => {
                        report.status = optional_status(report.required, CheckStatus::Fail);
                        report.reason = format!(
                            "reported version {version} is below project minimum {minimum}"
                        );
                    }
                    None => {
                        report.status = optional_status(report.required, CheckStatus::Unknown);
                        report.reason = format!(
                            "reported version {version}; project minimum {minimum} could not be compared"
                        );
                    }
                },
                None => {
                    report.status = CheckStatus::Pass;
                    let description = if expectation == VersionExpectation::Parseable {
                        "parseable version"
                    } else {
                        "reported version"
                    };
                    report.reason =
                        format!("command completed successfully; {description} {version}");
                }
            },
            (None, _) => {
                report.status = CheckStatus::Pass;
                report.reason = "command completed successfully".into();
            }
        },
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

fn numeric_version_at_least(actual: &str, minimum: &str) -> Option<bool> {
    fn components(value: &str) -> Option<Vec<u64>> {
        let value = value.split(['-', '+']).next()?;
        value
            .split('.')
            .map(|component| component.parse::<u64>().ok())
            .collect()
    }

    let actual = components(actual)?;
    let minimum = components(minimum)?;
    let width = actual.len().max(minimum.len());
    for index in 0..width {
        let actual_part = actual.get(index).copied().unwrap_or_default();
        let minimum_part = minimum.get(index).copied().unwrap_or_default();
        match actual_part.cmp(&minimum_part) {
            std::cmp::Ordering::Greater => return Some(true),
            std::cmp::Ordering::Less => return Some(false),
            std::cmp::Ordering::Equal => {}
        }
    }
    Some(true)
}

fn extract_reported_version(stdout: &[u8], stderr: &[u8], minimum_parts: usize) -> Option<String> {
    const VERSION_MARKERS: &[&str] = &[
        "cargo-ndk",
        "xcodegen ",
        "xcode ",
        "rustc ",
        "rustup ",
        "cargo ",
        "gradle ",
        "gradle version ",
        "release:",
        "clang ",
        "gcc ",
        "cc ",
        "xcrun version",
        "openjdk version",
        "java version",
        "android debug bridge version",
        "version:",
    ];

    for bytes in [stdout, stderr] {
        for line in String::from_utf8_lossy(bytes).lines() {
            let lower = line.to_ascii_lowercase();
            let Some(marker_end) = VERSION_MARKERS
                .iter()
                .filter_map(|marker| lower.find(marker).map(|index| index + marker.len()))
                .min()
            else {
                continue;
            };
            if let Some(version) = numeric_version_after(&line[marker_end..], minimum_parts) {
                return Some(version);
            }
        }
    }
    if let Some(version) = extract_gradle_version_from_report(stdout, stderr, minimum_parts) {
        return Some(version);
    }
    None
}

fn extract_gradle_version_from_report(
    stdout: &[u8],
    stderr: &[u8],
    minimum_parts: usize,
) -> Option<String> {
    [stdout, stderr]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            if !lower.contains("gradle") {
                return None;
            }
            numeric_version_after(&line, minimum_parts)
        })
}

fn numeric_version_after(text: &str, minimum_parts: usize) -> Option<String> {
    for (start, character) in text.char_indices() {
        if !character.is_ascii_digit() {
            continue;
        }
        let suffix = &text[start..];
        let end = suffix
            .char_indices()
            .find_map(|(index, character)| {
                (!character.is_ascii_digit() && character != '.').then_some(index)
            })
            .unwrap_or(suffix.len());
        let candidate = suffix[..end].trim_end_matches('.');
        let parts = candidate.split('.').collect::<Vec<_>>();
        if parts.len() >= minimum_parts
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Some(candidate.to_owned());
        }
    }
    None
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
    let matches = if let Some(values) = report.expected.as_array() {
        values.iter().any(|value| value == &host)
    } else {
        report.expected.as_str().is_some_and(|value| value == host)
    };
    report.actual = json!({"host_os": host});
    if matches {
        report.status = CheckStatus::Pass;
        report.reason = "host platform is supported".into();
    } else {
        report.status = optional_status(report.required, CheckStatus::Unavailable);
        report.reason = "selected target requires a different host platform".into();
    }
}

fn probe_project_metadata(report: &mut CheckReport) {
    let version = report.expected["version"].as_str();
    report.actual = json!({"version": version, "source": "project"});
    if version.is_some_and(|version| numeric_version_after(version, 3).as_deref() == Some(version))
    {
        report.status = CheckStatus::Pass;
        report.reason = format!(
            "project declares Android Gradle Plugin version {}; dependency resolution is not checked",
            version.unwrap_or_default()
        );
    } else {
        report.status = optional_status(report.required, CheckStatus::Unknown);
        report.reason = "project Android Gradle Plugin version could not be parsed".into();
    }
}

fn probe_environment(report: &mut CheckReport) {
    let variables = report.expected["variables"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut first_source = None;
    let mut any_exists = false;
    let mut selected = None;
    for variable in variables.iter().filter_map(Value::as_str) {
        if let Ok(value) = std::env::var(variable)
            && !value.trim().is_empty()
        {
            let path = PathBuf::from(value);
            first_source.get_or_insert_with(|| variable.to_string());
            any_exists |= path.exists();
            if path.is_dir() {
                selected = Some((variable.to_string(), path));
                break;
            }
        }
    }
    if let Some((source, path)) = selected {
        match report.id.as_str() {
            "android.sdk" => probe_android_sdk(report, &source, &path),
            "android.ndk" => probe_android_ndk(report, &source, &path),
            _ => {
                report.actual = json!({
                    "present": true,
                    "source": source,
                    "exists": true,
                    "directory": true,
                });
                report.status = CheckStatus::Pass;
                report.reason = "environment directory is available".into();
            }
        }
        return;
    }
    report.actual = json!({
        "present": first_source.is_some(),
        "source": first_source,
        "exists": any_exists,
        "directory": false,
    });
    report.status = optional_status(report.required, CheckStatus::Unavailable);
    report.reason = "none of the configured environment paths is an existing directory".into();
}

fn probe_android_sdk(report: &mut CheckReport, source: &str, root: &Path) {
    let requirements = &report.expected["requirements"];
    let expected_platform = requirements["platform"].as_str();
    let expected_build_tools = requirements["build_tools"].as_str();

    let platform_root = root.join("platforms");
    let available_platforms = package_directories(&platform_root, "android-")
        .into_iter()
        .map(|version| format!("android-{version}"))
        .filter(|package| platform_root.join(package).join("android.jar").is_file())
        .collect::<Vec<_>>();
    let platform = expected_platform.and_then(|expected| {
        let package = if expected.starts_with("android-") {
            expected.to_owned()
        } else {
            format!("android-{expected}")
        };
        let path = platform_root.join(&package);
        (path.join("android.jar").is_file()).then_some(package)
    });

    let build_tools_root = root.join("build-tools");
    let installed_build_tools = package_directories(&build_tools_root, "")
        .into_iter()
        .filter(|version| build_tools_package_is_valid(&build_tools_root.join(version)))
        .collect::<Vec<_>>();
    let selected_build_tools = if let Some(expected) = expected_build_tools {
        let path = build_tools_root.join(expected);
        build_tools_package_is_valid(&path).then(|| expected.to_owned())
    } else if expected_platform.is_some() {
        installed_build_tools.last().cloned()
    } else {
        None
    };
    let platform_ok = if expected_platform.is_some() {
        platform.is_some()
    } else {
        !available_platforms.is_empty()
    };
    let build_tools_ok = if expected_build_tools.is_some() {
        selected_build_tools.is_some()
    } else {
        !installed_build_tools.is_empty()
    };
    let platform_selection_unknown =
        expected_platform.is_none() && requirements["project_configured"] == true;
    report.actual = json!({
        "present": true,
        "source": source,
        "exists": true,
        "directory": true,
        "platform": platform,
        "platform_candidates": available_platforms,
        "build_tools": selected_build_tools,
        "build_tools_candidates": installed_build_tools,
    });
    if platform_selection_unknown && build_tools_ok && !available_platforms.is_empty() {
        report.status = optional_status(report.required, CheckStatus::Unknown);
        report.reason = "project compileSdk could not be resolved to a numeric SDK package".into();
        return;
    }
    match (platform_ok, build_tools_ok) {
        (true, true) => {
            report.status = CheckStatus::Pass;
            report.reason = match (expected_platform.is_some(), expected_build_tools.is_some()) {
                (true, true) => "selected Android platform and build-tools are available".into(),
                (true, false) => {
                    "selected Android platform and build-tools packages are available".into()
                }
                (false, true) => {
                    "Android platform packages and selected build-tools are available".into()
                }
                (false, false) => "Android platform and build-tools packages are available".into(),
            };
        }
        (false, false) => {
            report.status = optional_status(report.required, CheckStatus::Fail);
            report.reason =
                "selected Android platform and build-tools are missing or incomplete".into();
        }
        (false, true) => {
            report.status = optional_status(report.required, CheckStatus::Fail);
            report.reason = "selected Android platform is missing or incomplete".into();
        }
        (true, false) => {
            report.status = optional_status(report.required, CheckStatus::Fail);
            report.reason = "selected Android build-tools are missing or incomplete".into();
        }
    }
}

fn probe_android_ndk(report: &mut CheckReport, source: &str, root: &Path) {
    let properties = root.join("source.properties");
    let contents = match fs::read_to_string(&properties) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            report.actual = json!({
                "present": true,
                "source": source,
                "exists": true,
                "directory": true,
                "source_properties": false,
            });
            report.status = optional_status(report.required, CheckStatus::Fail);
            report.reason = "Android NDK source.properties is missing".into();
            return;
        }
        Err(_) => {
            report.actual = json!({
                "present": true,
                "source": source,
                "exists": true,
                "directory": true,
                "source_properties": true,
            });
            report.status = optional_status(report.required, CheckStatus::Unknown);
            report.reason = "Android NDK version metadata could not be read".into();
            return;
        }
    };
    let version = contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "Pkg.Revision").then(|| value.trim().to_owned())
    });
    let version = version.filter(|value| numeric_version_after(value, 2).as_deref() == Some(value));
    report.actual = json!({
        "present": true,
        "source": source,
        "exists": true,
        "directory": true,
        "source_properties": true,
        "version": version,
    });
    if report.actual["version"].is_string() {
        report.status = CheckStatus::Pass;
        report.reason = format!(
            "Android NDK version {} is readable",
            report.actual["version"].as_str().unwrap_or_default()
        );
    } else {
        report.status = optional_status(report.required, CheckStatus::Fail);
        report.reason = "Android NDK Pkg.Revision is missing or invalid".into();
    }
}

fn package_directories(root: &Path, prefix: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            let version = name.strip_prefix(prefix)?;
            numeric_version_after(version, 1)
                .as_deref()
                .filter(|parsed| *parsed == version)?;
            Some(version.to_owned())
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| compare_numeric_versions(left, right));
    versions
}

fn compare_numeric_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let components = |value: &str| {
        value
            .split('.')
            .filter_map(|part| part.parse::<u64>().ok())
            .collect::<Vec<_>>()
    };
    let left = components(left);
    let right = components(right);
    for index in 0..left.len().max(right.len()) {
        match left
            .get(index)
            .copied()
            .unwrap_or_default()
            .cmp(&right.get(index).copied().unwrap_or_default())
        {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn build_tools_package_is_valid(path: &Path) -> bool {
    let exists =
        |tool: &str| path.join(tool).is_file() || path.join(format!("{tool}.exe")).is_file();
    path.is_dir() && exists("aapt2") && exists("zipalign")
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

    fn subprocess_requirement(test_name: &str, expected: Value) -> Requirement {
        Requirement {
            id: "test.command".into(),
            target: Target::Desktop,
            kind: RequirementKind::Command,
            label: "deterministic command shim".into(),
            required: true,
            expected,
            command: Some(CommandSpec {
                program: std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                args: vec![
                    "--ignored".into(),
                    "--exact".into(),
                    test_name.into(),
                    "--nocapture".into(),
                ],
            }),
            remediation: vec![],
        }
    }

    fn environment_report(id: &str, expected: Value) -> CheckReport {
        CheckReport {
            id: id.into(),
            required: true,
            status: CheckStatus::Unknown,
            expected,
            actual: json!({}),
            reason: String::new(),
            remediation: vec![],
            duration_ms: 0,
            command: None,
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
    fn host_platform_probe_accepts_a_single_expected_platform() {
        let mut report = CheckReport {
            id: "ios.host_platform".into(),
            required: true,
            status: CheckStatus::Unknown,
            expected: json!(normalized_host()),
            actual: json!({}),
            reason: String::new(),
            remediation: vec![],
            duration_ms: 0,
            command: None,
        };

        probe_host(&mut report);

        assert_eq!(report.status, CheckStatus::Pass);
        assert_eq!(report.actual["host_os"], normalized_host());
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

    #[test]
    fn successful_command_with_unparseable_expected_version_fails() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_unparseable_version",
            json!({"executable": true, "version": "parseable"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Fail);
        assert!(reports[0].reason.contains("version"));
    }

    #[test]
    fn successful_command_reports_a_parseable_expected_version() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_valid_version",
            json!({"executable": true, "version": "parseable"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["version"], "1.97.1");
    }

    #[test]
    fn rustc_version_below_project_minimum_fails_with_actual_and_expected() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_valid_version",
            json!({"executable": true, "version": "parseable", "minimum": "1.98"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Fail);
        assert!(reports[0].reason.contains("below project minimum 1.98"));
        assert_eq!(reports[0].actual["version"], "1.97.1");
    }

    #[test]
    fn successful_tool_version_is_read_from_stderr() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_java_version_stderr",
            json!({"executable": true, "version": "reported"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["version"], "21.0.8");
    }

    #[test]
    fn cc_version_with_distribution_prefix_is_parseable() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_cc_ubuntu_version",
            json!({"executable": true, "version": "reported"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["version"], "13.3.0");
    }

    #[test]
    fn compatibility_marker_is_unknown_until_a_project_rule_exists() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_java_version_stderr",
            json!({"executable": true, "version": "AGP-compatible"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Unknown);
        assert!(reports[0].reason.contains("compatibility rule"));
        assert_eq!(reports[0].actual["version"], "21.0.8");
    }

    #[test]
    fn known_java_compatibility_rule_passes_supported_version() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_java_version_stderr",
            json!({"version": "AGP-compatible", "minimum_java_major": 17}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["minimum_java_major"], 17);
    }

    #[test]
    fn known_java_compatibility_rule_rejects_older_version() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_java_11_stderr",
            json!({"version": "AGP-compatible", "minimum_java_major": 17}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Fail);
        assert_eq!(reports[0].actual["version"], "11.0.22");
    }

    #[test]
    fn agp_requirement_reports_declared_project_metadata() {
        let requirement = Requirement {
            id: "android.agp".into(),
            target: Target::Android,
            kind: RequirementKind::ProjectMetadata,
            label: "Android Gradle Plugin".into(),
            required: true,
            expected: json!({"version": "9.1.0"}),
            command: None,
            remediation: vec![],
        };
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["version"], "9.1.0");
        assert!(
            reports[0]
                .reason
                .contains("dependency resolution is not checked")
        );
    }

    #[test]
    fn project_selected_gradle_version_must_match_wrapper_output() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_gradle_version",
            json!({"wrapper": true, "version": "project-selected", "selected_version": "9.4.1"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["version"], "9.4.1");
    }

    #[test]
    fn project_selected_gradle_version_mismatch_fails() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_gradle_version",
            json!({"wrapper": true, "version": "project-selected", "selected_version": "9.7.0"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Fail);
        assert!(
            reports[0]
                .reason
                .contains("does not match project-selected version")
        );
    }

    #[test]
    fn reported_tool_version_can_have_two_numeric_components() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_xcode_version",
            json!({"executable": true, "version": "reported"}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["version"], "26.2");
    }

    #[test]
    fn successful_non_version_command_does_not_require_version_output() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_quiet_success",
            json!({"subcommand": "simctl", "usable": true}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
    }

    #[test]
    fn failing_shim_is_reported_as_nonzero_exit() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_nonzero_exit",
            json!({"executable": true}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig::default());
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Fail);
        assert!(reports[0].reason.contains("exited unsuccessfully"));
    }

    #[test]
    fn hanging_shim_is_terminated_at_the_per_check_deadline() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_hangs",
            json!({"usable": true}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig {
            per_check: Duration::from_millis(100),
            total: Duration::from_secs(2),
            max_output_bytes: 16,
        });
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Unknown);
        assert!(reports[0].reason.contains("timed out"));
        assert!(reports[0].duration_ms < 2_000);
    }

    #[test]
    fn large_stderr_shim_is_drained_and_marked_truncated() {
        let requirement = subprocess_requirement(
            "toolchain::probe::tests::shim_large_stderr",
            json!({"usable": true}),
        );
        let mut runner = ProbeRunner::new(ProbeConfig {
            per_check: Duration::from_secs(5),
            total: Duration::from_secs(6),
            max_output_bytes: 16,
        });
        let reports = runner.probe(&[requirement], None);

        assert_eq!(reports[0].status, CheckStatus::Pass);
        assert_eq!(reports[0].actual["stderr"]["bytes"], 128 * 1024);
        assert_eq!(reports[0].actual["stderr"]["truncated"], true);
    }

    #[test]
    fn selected_android_sdk_packages_must_be_present() {
        let dir = tempfile::tempdir().unwrap();
        let platform = dir.path().join("platforms/android-34");
        let build_tools = dir.path().join("build-tools/35.0.0");
        fs::create_dir_all(&platform).unwrap();
        fs::create_dir_all(&build_tools).unwrap();
        fs::write(platform.join("android.jar"), []).unwrap();
        fs::write(build_tools.join("aapt2"), []).unwrap();
        fs::write(build_tools.join("zipalign"), []).unwrap();
        let mut report = environment_report(
            "android.sdk",
            json!({
                "variables": ["ANDROID_HOME"],
                "requirements": {
                    "platform": "34",
                    "build_tools": "35.0.0",
                    "project_configured": true
                }
            }),
        );

        probe_android_sdk(&mut report, "ANDROID_HOME", dir.path());

        assert_eq!(report.status, CheckStatus::Pass);
        assert_eq!(report.actual["platform"], "android-34");
        assert_eq!(report.actual["build_tools"], "35.0.0");
    }

    #[test]
    fn dynamic_project_compile_sdk_remains_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let platform = dir.path().join("platforms/android-34");
        let build_tools = dir.path().join("build-tools/35.0.0");
        fs::create_dir_all(&platform).unwrap();
        fs::create_dir_all(&build_tools).unwrap();
        fs::write(platform.join("android.jar"), []).unwrap();
        fs::write(build_tools.join("aapt2"), []).unwrap();
        fs::write(build_tools.join("zipalign"), []).unwrap();
        let mut report = environment_report(
            "android.sdk",
            json!({
                "variables": ["ANDROID_HOME"],
                "requirements": {
                    "platform": null,
                    "build_tools": null,
                    "project_configured": true
                }
            }),
        );

        probe_android_sdk(&mut report, "ANDROID_HOME", dir.path());

        assert_eq!(report.status, CheckStatus::Unknown);
        assert!(report.reason.contains("compileSdk could not be resolved"));
    }

    #[test]
    fn selected_android_sdk_fails_when_build_tools_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let platform = dir.path().join("platforms/android-34");
        fs::create_dir_all(&platform).unwrap();
        fs::write(platform.join("android.jar"), []).unwrap();
        let mut report = environment_report(
            "android.sdk",
            json!({
                "variables": ["ANDROID_HOME"],
                "requirements": {
                    "platform": "34",
                    "build_tools": "35.0.0",
                    "project_configured": true
                }
            }),
        );

        probe_android_sdk(&mut report, "ANDROID_HOME", dir.path());

        assert_eq!(report.status, CheckStatus::Fail);
        assert!(report.reason.contains("build-tools"));
    }

    #[test]
    fn selected_android_sdk_fails_when_compile_sdk_platform_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let platform = dir.path().join("platforms/android-35");
        let build_tools = dir.path().join("build-tools/35.0.0");
        fs::create_dir_all(&platform).unwrap();
        fs::create_dir_all(&build_tools).unwrap();
        fs::write(platform.join("android.jar"), []).unwrap();
        fs::write(build_tools.join("aapt2"), []).unwrap();
        fs::write(build_tools.join("zipalign"), []).unwrap();
        let mut report = environment_report(
            "android.sdk",
            json!({
                "variables": ["ANDROID_HOME"],
                "requirements": {
                    "platform": "34",
                    "build_tools": null,
                    "project_configured": true
                }
            }),
        );

        probe_android_sdk(&mut report, "ANDROID_HOME", dir.path());

        assert_eq!(report.status, CheckStatus::Fail);
        assert!(report.reason.contains("platform is missing"));
        assert_eq!(report.actual["platform"], Value::Null);
    }

    #[test]
    fn android_ndk_reports_revision_and_rejects_non_ndk_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("source.properties"),
            "Pkg.Revision = 27.2.12479018\n",
        )
        .unwrap();
        let mut report = environment_report(
            "android.ndk",
            json!({
                "variables": ["ANDROID_NDK_HOME"],
                "requirements": {"directory": true, "version": "reported"}
            }),
        );

        probe_android_ndk(&mut report, "ANDROID_NDK_HOME", dir.path());

        assert_eq!(report.status, CheckStatus::Pass);
        assert_eq!(report.actual["version"], "27.2.12479018");

        let wrong = tempfile::tempdir().unwrap();
        let mut invalid = environment_report(
            "android.ndk",
            json!({
                "variables": ["ANDROID_NDK_HOME"],
                "requirements": {"directory": true, "version": "reported"}
            }),
        );
        probe_android_ndk(&mut invalid, "ANDROID_NDK_HOME", wrong.path());
        assert_eq!(invalid.status, CheckStatus::Fail);
        assert!(invalid.reason.contains("source.properties"));
    }

    #[test]
    fn missing_required_rust_target_fails_with_expected_target_in_report() {
        let expected_target = "x86_64-linux-android";
        let requirement = Requirement {
            id: "android.rust_target.x86_64".into(),
            target: Target::Android,
            kind: RequirementKind::RustTarget,
            label: format!("Rust target {expected_target}"),
            required: true,
            expected: json!({"installed": true, "target": expected_target}),
            command: Some(CommandSpec::new(
                "rustup",
                &["target", "list", "--installed"],
            )),
            remediation: vec![],
        };
        let mut report = CheckReport::from_requirement(requirement);
        let installed_targets = b"aarch64-apple-darwin\naarch64-linux-android\n";
        let result = CommandResult {
            state: CommandState::Passed,
            path: Some(PathBuf::from("/fixture/rustup")),
            stdout: Captured {
                bytes: installed_targets.to_vec(),
                total_bytes: installed_targets.len(),
                truncated: false,
            },
            stderr: empty_capture(),
            duration_ms: 1,
        };

        apply_rust_target_result(&mut report, result);

        assert_eq!(report.status, CheckStatus::Fail);
        assert_eq!(report.expected["target"], expected_target);
        assert_eq!(report.actual["target"], expected_target);
        assert_eq!(report.actual["installed"], false);
        assert!(report.reason.contains("is not installed"));
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_unparseable_version() {
        println!("rustc definitely-not-a-version");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_valid_version() {
        println!("rustc 1.97.1 (fixture)");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_java_version_stderr() {
        eprintln!("openjdk version \"21.0.8\"");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_java_11_stderr() {
        eprintln!("openjdk version \"11.0.22\"");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_cc_ubuntu_version() {
        println!("cc (Ubuntu 13.3.0-6ubuntu2~24.04) 13.3.0");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_xcode_version() {
        println!("Xcode 26.2");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_gradle_version() {
        println!("Gradle 9.4.1");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_quiet_success() {}

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_nonzero_exit() {
        panic!("deterministic shim failure");
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_hangs() {
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore = "invoked as a deterministic child process by probe tests"]
    fn shim_large_stderr() {
        use std::io::Write;

        std::io::stderr()
            .write_all(&vec![b'x'; 128 * 1024])
            .unwrap();
    }
}
