//! Runner-facing execution boundary for the local matrix scheduler.
//!
//! The adapter is intentionally sequential for now. It exercises the same
//! scheduler/report contract that the future cross-target executor will use,
//! while keeping cleanup and artifact ownership explicit and testable without
//! requiring a simulator or emulator in CI.

use anyhow::{Result, anyhow};
use std::time::Instant;

use super::matrix::{
    MatrixCellError, MatrixCellSpec, MatrixCellState, MatrixPlan, MatrixReport, MatrixScheduler,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatrixCellExecution {
    pub status: MatrixCellState,
    pub error: Option<MatrixCellError>,
    pub artifact_ids: Vec<String>,
}

impl MatrixCellExecution {
    pub fn passed(artifact_ids: Vec<String>) -> Self {
        Self {
            status: MatrixCellState::Passed,
            error: None,
            artifact_ids,
        }
    }

    pub fn unavailable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: MatrixCellState::Unavailable,
            error: Some(MatrixCellError {
                code: code.into(),
                message: message.into(),
            }),
            artifact_ids: Vec::new(),
        }
    }
}

pub trait MatrixCellRunner {
    fn run_cell(&mut self, cell: &MatrixCellSpec, deadline: Instant)
    -> Result<MatrixCellExecution>;

    fn cleanup_cell(&mut self, cell: &MatrixCellSpec, deadline: Instant) -> Result<()>;
}

/// Runs admitted cells through one runner adapter and produces the same report
/// used by the future parallel executor. The adapter receives each cell's
/// effective deadline and must bound its own external work to it.
pub fn execute_matrix<R: MatrixCellRunner>(
    plan: MatrixPlan,
    runner: &mut R,
    now: Instant,
) -> Result<MatrixReport> {
    let mut scheduler = MatrixScheduler::new(plan.clone(), now)?;
    loop {
        let dispatch = scheduler.dispatch_ready(Instant::now());
        for cell_id in dispatch {
            if scheduler.state(&cell_id) == Some(MatrixCellState::CancelRequested) {
                scheduler.complete(
                    &cell_id,
                    MatrixCellState::Cancelled,
                    Instant::now(),
                    Some(MatrixCellError {
                        code: "cancelled_before_execution".into(),
                        message: "cell was cancelled before the adapter started it".into(),
                    }),
                    Vec::new(),
                )?;
                continue;
            }
            let cell = plan
                .cells
                .iter()
                .find(|cell| cell.cell_id == cell_id)
                .ok_or_else(|| anyhow!("matrix cell '{cell_id}' disappeared from the plan"))?;
            let deadline = scheduler
                .cell_deadline(&cell_id)
                .ok_or_else(|| anyhow!("matrix cell '{cell_id}' has no effective deadline"))?;
            let execution = match runner.run_cell(cell, deadline) {
                Ok(execution) => execution,
                Err(error) => MatrixCellExecution {
                    status: MatrixCellState::Failed,
                    error: Some(MatrixCellError {
                        code: "cell_runner_error".into(),
                        message: format!("{error:#}"),
                    }),
                    artifact_ids: Vec::new(),
                },
            };
            let cleanup_error = runner.cleanup_cell(cell, deadline).err();
            let (status, error) = if let Some(cleanup_error) = cleanup_error {
                (
                    MatrixCellState::Failed,
                    Some(MatrixCellError {
                        code: "cell_cleanup_failed".into(),
                        message: format!("{cleanup_error:#}"),
                    }),
                )
            } else {
                (execution.status, execution.error)
            };
            scheduler.complete(
                &cell_id,
                status,
                Instant::now(),
                error,
                execution.artifact_ids,
            )?;
        }
        if scheduler.is_complete() {
            return scheduler.report();
        }
        if dispatch_is_empty(&scheduler, &plan) {
            scheduler.request_cancel("executor_no_dispatch");
        }
    }
}

fn dispatch_is_empty(scheduler: &MatrixScheduler, plan: &MatrixPlan) -> bool {
    plan.cells.iter().all(|cell| {
        matches!(
            scheduler.state(&cell.cell_id),
            Some(MatrixCellState::CancelRequested)
                | Some(MatrixCellState::Cancelled)
                | Some(MatrixCellState::Passed)
                | Some(MatrixCellState::Failed)
                | Some(MatrixCellState::Inconclusive)
                | Some(MatrixCellState::Unavailable)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::matrix::{MatrixCellSpec, MatrixConfig, MatrixStatus};
    use std::collections::BTreeMap;

    struct FakeRunner {
        outcomes: BTreeMap<String, MatrixCellExecution>,
        cleanup_failures: BTreeMap<String, String>,
        cleaned: Vec<String>,
    }

    impl MatrixCellRunner for FakeRunner {
        fn run_cell(
            &mut self,
            cell: &MatrixCellSpec,
            _deadline: Instant,
        ) -> Result<MatrixCellExecution> {
            Ok(self
                .outcomes
                .get(&cell.cell_id)
                .cloned()
                .unwrap_or_else(|| {
                    MatrixCellExecution::unavailable("missing_fake", "not configured")
                }))
        }

        fn cleanup_cell(&mut self, cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
            self.cleaned.push(cell.cell_id.clone());
            if let Some(message) = self.cleanup_failures.get(&cell.cell_id) {
                anyhow::bail!("{message}");
            }
            Ok(())
        }
    }

    fn plan() -> MatrixPlan {
        MatrixPlan {
            plan_id: "matrix-executor-test".into(),
            config: MatrixConfig {
                max_parallel: 1,
                fail_fast: false,
                timeout_ms: 1_000,
            },
            cells: vec![
                MatrixCellSpec {
                    cell_id: "required".into(),
                    target_id: "macos".into(),
                    scenario_id: "smoke".into(),
                    required: true,
                    timeout_ms: None,
                },
                MatrixCellSpec {
                    cell_id: "optional".into(),
                    target_id: "android".into(),
                    scenario_id: "smoke".into(),
                    required: false,
                    timeout_ms: None,
                },
            ],
        }
    }

    #[test]
    fn executor_preserves_artifacts_and_optional_unavailable_is_partial() {
        let mut runner = FakeRunner {
            outcomes: BTreeMap::from([
                (
                    "required".into(),
                    MatrixCellExecution::passed(vec!["capture-a".into()]),
                ),
                (
                    "optional".into(),
                    MatrixCellExecution::unavailable("no_android", "emulator unavailable"),
                ),
            ]),
            cleanup_failures: BTreeMap::new(),
            cleaned: Vec::new(),
        };
        let report = execute_matrix(plan(), &mut runner, Instant::now()).unwrap();
        assert_eq!(report.status, MatrixStatus::Partial);
        assert_eq!(report.cells[0].artifact_ids, vec!["capture-a"]);
        assert_eq!(runner.cleaned, vec!["required", "optional"]);
    }

    #[test]
    fn cleanup_failure_overrides_a_passing_cell() {
        let mut runner = FakeRunner {
            outcomes: BTreeMap::from([(
                "required".into(),
                MatrixCellExecution::passed(Vec::new()),
            )]),
            cleanup_failures: BTreeMap::from([("required".into(), "cleanup failed".into())]),
            cleaned: Vec::new(),
        };
        let mut one_cell_plan = plan();
        one_cell_plan.cells.truncate(1);
        let report = execute_matrix(one_cell_plan, &mut runner, Instant::now()).unwrap();
        assert_eq!(report.status, MatrixStatus::Failed);
        assert_eq!(
            report.cells[0].error.as_ref().unwrap().code,
            "cell_cleanup_failed"
        );
    }

    #[test]
    fn runner_errors_become_failed_cell_evidence() {
        struct ErrorRunner;
        impl MatrixCellRunner for ErrorRunner {
            fn run_cell(
                &mut self,
                _cell: &MatrixCellSpec,
                _deadline: Instant,
            ) -> Result<MatrixCellExecution> {
                anyhow::bail!("launch failed")
            }

            fn cleanup_cell(&mut self, _cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
                Ok(())
            }
        }
        let mut runner = ErrorRunner;
        let mut one_cell_plan = plan();
        one_cell_plan.cells.truncate(1);
        let report = execute_matrix(one_cell_plan, &mut runner, Instant::now()).unwrap();
        assert_eq!(report.status, MatrixStatus::Failed);
        assert_eq!(
            report.cells[0].error.as_ref().unwrap().code,
            "cell_runner_error"
        );
    }

    #[test]
    fn deadline_is_passed_to_the_runner_as_a_bounded_contract() {
        struct DeadlineRunner;
        impl MatrixCellRunner for DeadlineRunner {
            fn run_cell(
                &mut self,
                _cell: &MatrixCellSpec,
                deadline: Instant,
            ) -> Result<MatrixCellExecution> {
                assert!(deadline > Instant::now());
                Ok(MatrixCellExecution::passed(Vec::new()))
            }

            fn cleanup_cell(&mut self, _cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
                Ok(())
            }
        }
        let mut runner = DeadlineRunner;
        let mut one_cell_plan = plan();
        one_cell_plan.cells.truncate(1);
        one_cell_plan.config.timeout_ms = 1000;
        let report = execute_matrix(one_cell_plan, &mut runner, Instant::now()).unwrap();
        assert_eq!(report.status, MatrixStatus::Passed);
    }
}
