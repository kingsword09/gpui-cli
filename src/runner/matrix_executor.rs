//! Runner-facing execution boundary for the local matrix scheduler.
//!
//! The module keeps a sequential adapter for simple callers and also exposes
//! a scoped parallel executor. The parallel path creates one runner per cell,
//! limits active workers through the scheduler, and holds matrix resource
//! guards through cleanup.

use anyhow::{Result, anyhow, bail};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use super::matrix::{
    MatrixCellError, MatrixCellSpec, MatrixCellState, MatrixPlan, MatrixReport, MatrixScheduler,
};
use super::matrix_admission::MatrixAdmission;
use super::matrix_resources::MatrixResourcePool;

#[derive(Clone, Debug, PartialEq)]
pub struct MatrixCellExecution {
    pub status: MatrixCellState,
    pub error: Option<MatrixCellError>,
    pub artifact_ids: Vec<String>,
    pub context: Option<crate::scenario::executor::CheckContext>,
    pub check_report: Option<crate::scenario::executor::CheckReport>,
}

impl MatrixCellExecution {
    pub fn passed(artifact_ids: Vec<String>) -> Self {
        Self {
            status: MatrixCellState::Passed,
            error: None,
            artifact_ids,
            context: None,
            check_report: None,
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
            context: None,
            check_report: None,
        }
    }

    pub fn with_context(mut self, context: crate::scenario::executor::CheckContext) -> Self {
        self.context = Some(context);
        self
    }

    pub fn with_check_report(
        mut self,
        check_report: crate::scenario::executor::CheckReport,
    ) -> Self {
        self.check_report = Some(check_report);
        self
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
                    context: None,
                    check_report: None,
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
            } else if Instant::now() >= deadline {
                (
                    MatrixCellState::Cancelled,
                    Some(MatrixCellError {
                        code: "cell_deadline_exceeded".into(),
                        message: "cell runner returned after its deadline".into(),
                    }),
                )
            } else {
                (execution.status, execution.error)
            };
            let context = execution.context;
            let check_report = execution.check_report;
            scheduler.complete_with_context(
                &cell_id,
                status,
                Instant::now(),
                error,
                execution.artifact_ids,
                context,
            )?;
            scheduler.attach_check_report(&cell_id, check_report)?;
        }
        if scheduler.is_complete() {
            return scheduler.report();
        }
        if dispatch_is_empty(&scheduler, &plan) {
            scheduler.request_cancel("executor_no_dispatch");
        }
    }
}

/// Executes admitted cells concurrently across independent targets.
///
/// The factory must create an isolated runner for each cell. Cells that name
/// the same resource wait behind one another, while cells on different
/// resources can run up to max_parallel at the same time. A resource guard is
/// held until cleanup has completed, so a later cell cannot start while the
/// previous cell still owns device-side state.
pub fn execute_matrix_parallel<R, F>(
    plan: MatrixPlan,
    runner_factory: F,
    resources: MatrixResourcePool,
    now: Instant,
) -> Result<MatrixReport>
where
    R: MatrixCellRunner + Send,
    F: Fn(&MatrixCellSpec) -> Result<R> + Sync,
{
    let scheduler = MatrixScheduler::new(plan.clone(), now)?;
    execute_matrix_parallel_with_scheduler(plan, scheduler, runner_factory, resources)
}

/// Executes a normalized admission result, preserving cells that were already
/// marked unavailable before any worker is created.
pub fn execute_admitted_matrix_parallel<R, F>(
    admission: &MatrixAdmission,
    runner_factory: F,
    resources: MatrixResourcePool,
    now: Instant,
) -> Result<MatrixReport>
where
    R: MatrixCellRunner + Send,
    F: Fn(&MatrixCellSpec) -> Result<R> + Sync,
{
    let mut scheduler = MatrixScheduler::new(admission.plan.clone(), now)?;
    admission.apply(&mut scheduler)?;
    execute_matrix_parallel_with_scheduler(
        admission.plan.clone(),
        scheduler,
        runner_factory,
        resources,
    )
}

fn execute_matrix_parallel_with_scheduler<R, F>(
    plan: MatrixPlan,
    mut scheduler: MatrixScheduler,
    runner_factory: F,
    resources: MatrixResourcePool,
) -> Result<MatrixReport>
where
    R: MatrixCellRunner + Send,
    F: Fn(&MatrixCellSpec) -> Result<R> + Sync,
{
    let (sender, receiver) = mpsc::channel::<ParallelWorkerResult>();

    thread::scope(|scope| -> Result<MatrixReport> {
        let mut active_workers = 0usize;
        loop {
            let dispatch = scheduler.dispatch_ready(Instant::now());
            for cell_id in dispatch {
                let cell = plan
                    .cells
                    .iter()
                    .find(|cell| cell.cell_id == cell_id)
                    .cloned()
                    .ok_or_else(|| anyhow!("matrix cell '{cell_id}' disappeared from the plan"))?;
                let deadline = scheduler
                    .cell_deadline(&cell_id)
                    .ok_or_else(|| anyhow!("matrix cell '{cell_id}' has no effective deadline"))?;
                let sender = sender.clone();
                let resources = resources.clone();
                let factory = &runner_factory;
                scope.spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        run_parallel_cell(factory, &cell, deadline, &resources)
                    }))
                    .unwrap_or_else(|_| ParallelWorkerResult {
                        cell_id: cell.cell_id.clone(),
                        status: MatrixCellState::Failed,
                        error: Some(MatrixCellError {
                            code: "cell_runner_panicked".into(),
                            message: "matrix worker panicked before it could report cell cleanup"
                                .into(),
                        }),
                        artifact_ids: Vec::new(),
                        context: None,
                        check_report: None,
                    });
                    let _ = sender.send(result);
                });
                active_workers += 1;
            }

            if scheduler.is_complete() {
                return scheduler.report();
            }
            if active_workers == 0 && dispatch_is_empty(&scheduler, &plan) {
                scheduler.request_cancel("executor_no_dispatch");
                continue;
            }

            match receiver.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(result) => {
                    active_workers = active_workers.saturating_sub(1);
                    complete_parallel_result(&mut scheduler, result)?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("matrix worker channel disconnected before completion");
                }
            }
        }
    })
}

#[derive(Debug)]
struct ParallelWorkerResult {
    cell_id: String,
    status: MatrixCellState,
    error: Option<MatrixCellError>,
    artifact_ids: Vec<String>,
    context: Option<crate::scenario::executor::CheckContext>,
    check_report: Option<crate::scenario::executor::CheckReport>,
}

fn run_parallel_cell<R, F>(
    factory: &F,
    cell: &MatrixCellSpec,
    deadline: Instant,
    resources: &MatrixResourcePool,
) -> ParallelWorkerResult
where
    R: MatrixCellRunner + Send,
    F: Fn(&MatrixCellSpec) -> Result<R> + Sync,
{
    let resource_guard = match resources.acquire(&cell.resource_ids, deadline) {
        Ok(guard) => guard,
        Err(error) => {
            return ParallelWorkerResult {
                cell_id: cell.cell_id.clone(),
                status: MatrixCellState::Cancelled,
                error: Some(MatrixCellError {
                    code: "resource_wait_deadline_exceeded".into(),
                    message: format!("{error:#}"),
                }),
                artifact_ids: Vec::new(),
                context: None,
                check_report: None,
            };
        }
    };
    let mut runner = match factory(cell) {
        Ok(runner) => runner,
        Err(error) => {
            drop(resource_guard);
            return ParallelWorkerResult {
                cell_id: cell.cell_id.clone(),
                status: MatrixCellState::Failed,
                error: Some(MatrixCellError {
                    code: "cell_runner_factory_error".into(),
                    message: format!("{error:#}"),
                }),
                artifact_ids: Vec::new(),
                context: None,
                check_report: None,
            };
        }
    };
    let execution = match catch_unwind(AssertUnwindSafe(|| runner.run_cell(cell, deadline))) {
        Ok(Ok(execution)) => execution,
        Ok(Err(error)) => MatrixCellExecution {
            status: MatrixCellState::Failed,
            error: Some(MatrixCellError {
                code: "cell_runner_error".into(),
                message: format!("{error:#}"),
            }),
            artifact_ids: Vec::new(),
            context: None,
            check_report: None,
        },
        Err(_) => MatrixCellExecution {
            status: MatrixCellState::Failed,
            error: Some(MatrixCellError {
                code: "cell_runner_panicked".into(),
                message: "matrix runner panicked; cleanup will still be attempted".into(),
            }),
            artifact_ids: Vec::new(),
            context: None,
            check_report: None,
        },
    };
    let cleanup_error = match catch_unwind(AssertUnwindSafe(|| runner.cleanup_cell(cell, deadline)))
    {
        Ok(result) => result.err(),
        Err(_) => Some(anyhow!("matrix runner cleanup panicked")),
    };
    let (status, error) = if let Some(cleanup_error) = cleanup_error {
        (
            MatrixCellState::Failed,
            Some(MatrixCellError {
                code: "cell_cleanup_failed".into(),
                message: format!("{cleanup_error:#}"),
            }),
        )
    } else if Instant::now() >= deadline {
        (
            MatrixCellState::Cancelled,
            Some(MatrixCellError {
                code: "cell_deadline_exceeded".into(),
                message: "cell runner returned after its deadline".into(),
            }),
        )
    } else {
        (execution.status, execution.error)
    };
    let context = execution.context;
    let check_report = execution.check_report;
    let artifact_ids = execution.artifact_ids;
    drop(resource_guard);
    ParallelWorkerResult {
        cell_id: cell.cell_id.clone(),
        status,
        error,
        artifact_ids,
        context,
        check_report,
    }
}

fn complete_parallel_result(
    scheduler: &mut MatrixScheduler,
    result: ParallelWorkerResult,
) -> Result<()> {
    let (status, error) =
        if scheduler.state(&result.cell_id) == Some(MatrixCellState::CancelRequested) {
            (
                MatrixCellState::Cancelled,
                result.error.or_else(|| {
                    Some(MatrixCellError {
                        code: "matrix_cancelled".into(),
                        message: "cell was cancelled while its runner was active".into(),
                    })
                }),
            )
        } else {
            (result.status, result.error)
        };
    scheduler.complete_with_context(
        &result.cell_id,
        status,
        Instant::now(),
        error,
        result.artifact_ids,
        result.context,
    )?;
    scheduler.attach_check_report(&result.cell_id, result.check_report)
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
    use crate::runner::matrix_resources::MatrixResourcePool;
    use crate::scenario::executor::{
        CheckContext, CheckError, CheckReport, CheckStatus, CleanupReport, StepReport, StepStatus,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

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
                    resource_ids: Vec::new(),
                },
                MatrixCellSpec {
                    cell_id: "optional".into(),
                    target_id: "android".into(),
                    scenario_id: "smoke".into(),
                    required: false,
                    timeout_ms: None,
                    resource_ids: Vec::new(),
                },
            ],
        }
    }

    fn check_report() -> CheckReport {
        CheckReport {
            schema_version: 1,
            scenario_id: "smoke".into(),
            component: "Counter".into(),
            fixture_hash: Some("fixture-hash".into()),
            context: None,
            status: CheckStatus::Failed,
            steps: vec![StepReport {
                id: "assert-count".into(),
                kind: "assert".into(),
                status: StepStatus::Failed,
                duration_ms: 7,
                before_observation_id: Some("obs-before".into()),
                after_observation_id: Some("obs-after".into()),
                before_log_seq: Some(10),
                after_log_seq: Some(11),
                action: None,
                assertion: None,
                capture: None,
                error: Some(CheckError {
                    code: "assertion_failed".into(),
                    message: "count did not match".into(),
                    details: None,
                }),
            }],
            primary_error: Some(CheckError {
                code: "assertion_failed".into(),
                message: "count did not match".into(),
                details: None,
            }),
            cleanup: CleanupReport {
                attempted: true,
                succeeded: false,
                error: Some(CheckError {
                    code: "cleanup_failed".into(),
                    message: "preview did not exit".into(),
                    details: None,
                }),
            },
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
    fn executor_preserves_runtime_context_in_the_matrix_report() {
        let context = CheckContext {
            reset_generation: Some(3),
            snapshot_hash: Some("snapshot-hash".into()),
            build_key: None,
            environment: Some(json!({"theme": "light"})),
            uncontrolled_inputs: vec!["network".into()],
            mobile_evidence: None,
        };
        let mut runner = FakeRunner {
            outcomes: BTreeMap::from([(
                "required".into(),
                MatrixCellExecution::passed(Vec::new()).with_context(context.clone()),
            )]),
            cleanup_failures: BTreeMap::new(),
            cleaned: Vec::new(),
        };
        let mut one_cell_plan = plan();
        one_cell_plan.cells.truncate(1);

        let report = execute_matrix(one_cell_plan, &mut runner, Instant::now()).unwrap();

        assert_eq!(report.cells[0].context, Some(context));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["cells"][0]["context"]["snapshot_hash"],
            "snapshot-hash"
        );
    }

    #[test]
    fn executor_preserves_complete_check_report_and_json_round_trips_it() {
        let check_report = check_report();
        let mut runner = FakeRunner {
            outcomes: BTreeMap::from([(
                "required".into(),
                MatrixCellExecution::passed(vec!["capture-a".into()])
                    .with_check_report(check_report.clone()),
            )]),
            cleanup_failures: BTreeMap::new(),
            cleaned: Vec::new(),
        };
        let mut one_cell_plan = plan();
        one_cell_plan.cells.truncate(1);

        let report = execute_matrix(one_cell_plan, &mut runner, Instant::now()).unwrap();

        assert_eq!(report.cells[0].check_report, Some(check_report.clone()));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["cells"][0]["check_report"]["steps"][0]["id"],
            "assert-count"
        );
        assert_eq!(
            json["cells"][0]["check_report"]["cleanup"]["succeeded"],
            false
        );
        let decoded: MatrixReport = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.cells[0].check_report, Some(check_report));
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
    fn parallel_runner_panic_becomes_failed_cell_evidence() {
        let mut one_cell_plan = plan();
        one_cell_plan.cells.truncate(1);
        let report = execute_matrix_parallel(
            one_cell_plan,
            |_cell: &MatrixCellSpec| -> Result<ParallelRunner> {
                panic!("runner construction panic")
            },
            MatrixResourcePool::new(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(report.status, MatrixStatus::Failed);
        assert_eq!(
            report.cells[0].error.as_ref().unwrap().code,
            "cell_runner_panicked"
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

    #[test]
    fn late_runner_success_is_cancelled_after_cleanup() {
        struct LateRunner {
            cleaned: bool,
        }
        impl MatrixCellRunner for LateRunner {
            fn run_cell(
                &mut self,
                _cell: &MatrixCellSpec,
                _deadline: Instant,
            ) -> Result<MatrixCellExecution> {
                std::thread::sleep(Duration::from_millis(20));
                Ok(MatrixCellExecution::passed(vec!["late-capture".into()]))
            }

            fn cleanup_cell(&mut self, _cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
                self.cleaned = true;
                Ok(())
            }
        }
        let mut runner = LateRunner { cleaned: false };
        let mut one_cell_plan = plan();
        one_cell_plan.cells.truncate(1);
        one_cell_plan.cells[0].timeout_ms = Some(1);
        let report = execute_matrix(one_cell_plan, &mut runner, Instant::now()).unwrap();
        assert!(runner.cleaned);
        assert_eq!(report.status, MatrixStatus::Cancelled);
        assert_eq!(
            report.cells[0].error.as_ref().unwrap().code,
            "cell_deadline_exceeded"
        );
        assert_eq!(report.cells[0].artifact_ids, vec!["late-capture"]);
    }

    struct ParallelRunner {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl MatrixCellRunner for ParallelRunner {
        fn run_cell(
            &mut self,
            _cell: &MatrixCellSpec,
            _deadline: Instant,
        ) -> Result<MatrixCellExecution> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(MatrixCellExecution::passed(Vec::new()))
        }

        fn cleanup_cell(&mut self, _cell: &MatrixCellSpec, _deadline: Instant) -> Result<()> {
            Ok(())
        }
    }

    fn parallel_plan(resource_ids: [Vec<String>; 2]) -> MatrixPlan {
        MatrixPlan {
            plan_id: "parallel-matrix".into(),
            config: MatrixConfig {
                max_parallel: 2,
                fail_fast: false,
                timeout_ms: 1_000,
            },
            cells: vec![
                MatrixCellSpec {
                    cell_id: "first".into(),
                    target_id: "macos".into(),
                    scenario_id: "smoke".into(),
                    required: true,
                    timeout_ms: None,
                    resource_ids: resource_ids[0].clone(),
                },
                MatrixCellSpec {
                    cell_id: "second".into(),
                    target_id: "android".into(),
                    scenario_id: "smoke".into(),
                    required: true,
                    timeout_ms: None,
                    resource_ids: resource_ids[1].clone(),
                },
            ],
        }
    }

    #[test]
    fn parallel_executor_overlaps_independent_targets() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let factory_active = active.clone();
        let factory_max = max_active.clone();
        let factory = move |_cell: &MatrixCellSpec| {
            Ok(ParallelRunner {
                active: factory_active.clone(),
                max_active: factory_max.clone(),
            })
        };
        let report = execute_matrix_parallel(
            parallel_plan([vec!["device:first".into()], vec!["device:second".into()]]),
            factory,
            MatrixResourcePool::new(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(report.status, MatrixStatus::Passed);
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parallel_executor_serializes_cells_sharing_a_resource() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let factory_active = active.clone();
        let factory_max = max_active.clone();
        let factory = move |_cell: &MatrixCellSpec| {
            Ok(ParallelRunner {
                active: factory_active.clone(),
                max_active: factory_max.clone(),
            })
        };
        let report = execute_matrix_parallel(
            parallel_plan([vec!["device:shared".into()], vec!["device:shared".into()]]),
            factory,
            MatrixResourcePool::new(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(report.status, MatrixStatus::Passed);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn admitted_parallel_executor_preserves_preflight_unavailable_cells() {
        let plan = parallel_plan([Vec::new(), Vec::new()]);
        let first = plan.cells[0].clone();
        let second = plan.cells[1].clone();
        let admission = MatrixAdmission {
            plan,
            scenario_hash: None,
            cells: vec![
                super::super::matrix_admission::MatrixCellAdmission {
                    cell: first,
                    state: super::super::matrix_admission::MatrixAdmissionState::Unavailable,
                    issues: vec![super::super::matrix_admission::MatrixAdmissionIssue {
                        code: "host_unavailable".into(),
                        message: "Windows runner is not registered".into(),
                    }],
                },
                super::super::matrix_admission::MatrixCellAdmission {
                    cell: second,
                    state: super::super::matrix_admission::MatrixAdmissionState::Ready,
                    issues: Vec::new(),
                },
            ],
        };
        let report = execute_admitted_matrix_parallel(
            &admission,
            |_cell| {
                Ok(ParallelRunner {
                    active: Arc::new(AtomicUsize::new(0)),
                    max_active: Arc::new(AtomicUsize::new(0)),
                })
            },
            MatrixResourcePool::new(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(report.status, MatrixStatus::Unavailable);
        assert_eq!(report.cells[0].status, MatrixCellState::Unavailable);
        assert_eq!(report.cells[1].status, MatrixCellState::Passed);
    }
}
