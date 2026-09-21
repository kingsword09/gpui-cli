//! Versioned report model shared by human and JSON doctor renderers.

use super::Target;
use super::requirements::{CommandSpec, CommandSuggestion, Requirement};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Warning,
    Unavailable,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectSummary {
    pub root: Option<String>,
    pub name: Option<String>,
    pub targets: Vec<Target>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TargetSummary {
    pub id: Target,
    pub explicit: bool,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckReport {
    pub id: String,
    pub required: bool,
    pub status: CheckStatus,
    pub expected: Value,
    pub actual: Value,
    pub reason: String,
    pub remediation: Vec<CommandSuggestion>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandSpec>,
}

impl CheckReport {
    pub fn from_requirement(requirement: Requirement) -> Self {
        Self {
            id: requirement.id,
            required: requirement.required,
            status: CheckStatus::Unknown,
            expected: requirement.expected,
            actual: json!({"status": "not_run"}),
            reason: "probe not run".into(),
            remediation: requirement.remediation,
            duration_ms: 0,
            command: requirement.command,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub project: ProjectSummary,
    pub target: TargetSummary,
    pub overall: CheckStatus,
    pub checks: Vec<CheckReport>,
}

impl DoctorReport {
    pub fn new(project: ProjectSummary, target: TargetSummary, checks: Vec<CheckReport>) -> Self {
        let overall = overall_status(&checks);
        Self {
            schema_version: SCHEMA_VERSION,
            project,
            target,
            overall,
            checks,
        }
    }

    pub fn required_ok(&self) -> bool {
        self.checks
            .iter()
            .filter(|check| check.required)
            .all(|check| check.status == CheckStatus::Pass)
    }

    pub fn exit_code(&self) -> i32 {
        if self.required_ok() { 0 } else { 1 }
    }
}

pub fn overall_status(checks: &[CheckReport]) -> CheckStatus {
    if checks.iter().any(|check| {
        check.required
            && matches!(
                check.status,
                CheckStatus::Fail | CheckStatus::Unavailable | CheckStatus::Unknown
            )
    }) {
        if checks
            .iter()
            .any(|check| check.required && check.status == CheckStatus::Fail)
        {
            CheckStatus::Fail
        } else {
            CheckStatus::Unknown
        }
    } else if checks
        .iter()
        .any(|check| check.status == CheckStatus::Warning)
    {
        CheckStatus::Warning
    } else {
        CheckStatus::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::requirements::{Context, RequirementKind, requirements_for};

    fn check(required: bool, status: CheckStatus) -> CheckReport {
        CheckReport {
            id: "test".into(),
            required,
            status,
            expected: json!({}),
            actual: json!({}),
            reason: String::new(),
            remediation: Vec::new(),
            duration_ms: 1,
            command: None,
        }
    }

    #[test]
    fn optional_failure_is_a_warning_but_required_unknown_blocks() {
        assert_eq!(
            overall_status(&[check(false, CheckStatus::Fail)]),
            CheckStatus::Pass
        );
        assert_eq!(
            overall_status(&[check(false, CheckStatus::Warning)]),
            CheckStatus::Warning
        );
        assert_eq!(
            overall_status(&[check(true, CheckStatus::Unknown)]),
            CheckStatus::Unknown
        );
        assert_eq!(
            overall_status(&[check(true, CheckStatus::Fail)]),
            CheckStatus::Fail
        );
    }

    #[test]
    fn report_uses_schema_v2_and_required_exit_rules() {
        let requirements = requirements_for(&Context::for_host("linux"), Target::Desktop).unwrap();
        let checks = requirements
            .into_iter()
            .map(CheckReport::from_requirement)
            .collect();
        let report = DoctorReport::new(
            ProjectSummary {
                root: None,
                name: None,
                targets: vec![],
            },
            TargetSummary {
                id: Target::Desktop,
                explicit: true,
                source: "cli".into(),
            },
            checks,
        );
        assert_eq!(report.schema_version, 2);
        assert_eq!(report.exit_code(), 1);
        assert!(report.checks.iter().any(|check| check.command.is_some()));
    }

    #[test]
    fn report_remains_json_safe_for_command_and_path_data() {
        let report = DoctorReport::new(
            ProjectSummary {
                root: Some("/tmp/project".into()),
                name: Some("probe".into()),
                targets: vec![Target::Desktop],
            },
            TargetSummary {
                id: Target::Desktop,
                explicit: false,
                source: "project".into(),
            },
            vec![CheckReport {
                id: "desktop.c_compiler".into(),
                required: true,
                status: CheckStatus::Pass,
                expected: json!({"executable": true}),
                actual: json!({"path": "/usr/bin/cc"}),
                reason: "ok".into(),
                remediation: vec![],
                duration_ms: 2,
                command: Some(CommandSpec::new("cc", &["--version"])),
            }],
        );
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["checks"][0]["command"]["args"][0], "--version");
        assert_eq!(value["overall"], "pass");
        let _ = RequirementKind::Command;
    }
}
