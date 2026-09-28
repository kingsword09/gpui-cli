//! Deterministic local matrix scheduling and result aggregation.
//!
//! This module deliberately does not launch a platform runner yet. It owns the
//! bounded plan, cell lifecycle, cancellation/deadline semantics and the rule
//! that unavailable or uncertain required cells can never become a passing
//! matrix by omission.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub const MAX_MATRIX_CELLS: usize = 1024;
pub const MAX_MATRIX_PARALLEL: u16 = 64;
pub const MAX_MATRIX_TIMEOUT_MS: u64 = 30 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatrixConfig {
    pub max_parallel: u16,
    pub fail_fast: bool,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatrixCellSpec {
    pub cell_id: String,
    pub target_id: String,
    pub scenario_id: String,
    pub required: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatrixPlan {
    pub plan_id: String,
    pub config: MatrixConfig,
    pub cells: Vec<MatrixCellSpec>,
}

impl MatrixPlan {
    pub fn validate(&self) -> Result<()> {
        if self.plan_id.trim().is_empty() {
            bail!("matrix plan id must not be empty");
        }
        if self.cells.is_empty() {
            bail!("matrix plan must contain at least one cell");
        }
        if self.cells.len() > MAX_MATRIX_CELLS {
            bail!("matrix plan exceeds the {} cell limit", MAX_MATRIX_CELLS);
        }
        if self.config.max_parallel == 0 || self.config.max_parallel > MAX_MATRIX_PARALLEL {
            bail!(
                "matrix max_parallel must be between 1 and {}",
                MAX_MATRIX_PARALLEL
            );
        }
        validate_timeout(self.config.timeout_ms, "matrix timeout_ms")?;

        let mut cell_ids = BTreeSet::new();
        for cell in &self.cells {
            if cell.cell_id.trim().is_empty()
                || cell.target_id.trim().is_empty()
                || cell.scenario_id.trim().is_empty()
            {
                bail!("matrix cell ids and references must not be empty");
            }
            if !cell_ids.insert(&cell.cell_id) {
                bail!("duplicate matrix cell id '{}'", cell.cell_id);
            }
            if let Some(timeout_ms) = cell.timeout_ms {
                validate_timeout(timeout_ms, "matrix cell timeout_ms")?;
            }
        }
        Ok(())
    }

    fn cell(&self, cell_id: &str) -> Result<&MatrixCellSpec> {
        self.cells
            .iter()
            .find(|cell| cell.cell_id == cell_id)
            .ok_or_else(|| anyhow::anyhow!("unknown matrix cell '{cell_id}'"))
    }
}

fn validate_timeout(timeout_ms: u64, label: &str) -> Result<()> {
    if timeout_ms == 0 || timeout_ms > MAX_MATRIX_TIMEOUT_MS {
        bail!(
            "{label} must be between 1 and {} milliseconds",
            MAX_MATRIX_TIMEOUT_MS
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixCellState {
    Queued,
    Running,
    CancelRequested,
    Passed,
    Failed,
    Inconclusive,
    Unavailable,
    Cancelled,
}

impl MatrixCellState {
    fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::CancelRequested)
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed | Self::Inconclusive | Self::Unavailable | Self::Cancelled
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixStatus {
    Passed,
    Partial,
    Failed,
    Inconclusive,
    Unavailable,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatrixCellError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatrixCellResult {
    pub cell_id: String,
    pub target_id: String,
    pub scenario_id: String,
    pub required: bool,
    pub status: MatrixCellState,
    pub duration_ms: u64,
    pub error: Option<MatrixCellError>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MatrixReport {
    pub plan_id: String,
    pub status: MatrixStatus,
    pub cells: Vec<MatrixCellResult>,
    pub required_count: usize,
    pub required_passed: usize,
    pub optional_non_passed: usize,
}

pub fn summarize_cells(cells: &[MatrixCellResult]) -> Result<MatrixStatus> {
    if cells.is_empty() {
        bail!("cannot summarize an empty matrix");
    }
    if cells.iter().any(|cell| !cell.status.is_terminal()) {
        bail!("cannot summarize a matrix with non-terminal cells");
    }

    let required = cells.iter().filter(|cell| cell.required);
    let optional_non_passed = cells
        .iter()
        .filter(|cell| !cell.required && cell.status != MatrixCellState::Passed)
        .count();

    if required
        .clone()
        .any(|cell| cell.status == MatrixCellState::Failed)
    {
        return Ok(MatrixStatus::Failed);
    }
    if required
        .clone()
        .any(|cell| cell.status == MatrixCellState::Cancelled)
    {
        return Ok(MatrixStatus::Cancelled);
    }
    if required
        .clone()
        .any(|cell| cell.status == MatrixCellState::Inconclusive)
    {
        return Ok(MatrixStatus::Inconclusive);
    }
    if required
        .clone()
        .any(|cell| cell.status == MatrixCellState::Unavailable)
    {
        return Ok(MatrixStatus::Unavailable);
    }
    if optional_non_passed > 0 {
        return Ok(MatrixStatus::Partial);
    }
    Ok(MatrixStatus::Passed)
}

pub struct MatrixScheduler {
    plan: MatrixPlan,
    states: BTreeMap<String, MatrixCellState>,
    results: BTreeMap<String, MatrixCellResult>,
    started_at: BTreeMap<String, Instant>,
    cancel_reasons: BTreeMap<String, MatrixCellError>,
    deadline: Instant,
    deadline_exceeded: bool,
    fail_fast_triggered: bool,
}

impl MatrixScheduler {
    pub fn new(plan: MatrixPlan, now: Instant) -> Result<Self> {
        plan.validate()?;
        let states = plan
            .cells
            .iter()
            .map(|cell| (cell.cell_id.clone(), MatrixCellState::Queued))
            .collect();
        Ok(Self {
            deadline: now + Duration::from_millis(plan.config.timeout_ms),
            plan,
            states,
            results: BTreeMap::new(),
            started_at: BTreeMap::new(),
            cancel_reasons: BTreeMap::new(),
            deadline_exceeded: false,
            fail_fast_triggered: false,
        })
    }

    /// Admits deterministic queued cells while respecting max_parallel and
    /// marks expired/failed work for cancellation before dispatching more.
    pub fn dispatch_ready(&mut self, now: Instant) -> Vec<String> {
        self.apply_deadlines(now);
        if self.deadline_exceeded || self.fail_fast_triggered {
            return Vec::new();
        }
        let active = self
            .states
            .values()
            .filter(|state| state.is_active())
            .count();
        let capacity = usize::from(self.plan.config.max_parallel).saturating_sub(active);
        let mut dispatched = Vec::new();
        for cell in &self.plan.cells {
            if dispatched.len() >= capacity {
                break;
            }
            if self.states.get(&cell.cell_id) != Some(&MatrixCellState::Queued) {
                continue;
            }
            self.states
                .insert(cell.cell_id.clone(), MatrixCellState::Running);
            self.started_at.insert(cell.cell_id.clone(), now);
            dispatched.push(cell.cell_id.clone());
        }
        dispatched
    }

    /// Requests cancellation for queued work immediately and marks active
    /// work for cleanup. Active cells become terminal only after `complete`.
    pub fn request_cancel(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        let ids: Vec<String> = self.states.keys().cloned().collect();
        for cell_id in ids {
            match self.states.get(&cell_id).copied() {
                Some(MatrixCellState::Queued) => self.cancel_queued(&cell_id, &reason),
                Some(MatrixCellState::Running) => self.mark_cancel_requested(&cell_id, &reason),
                _ => {}
            }
        }
    }

    /// Completes one active cell after its runner has performed cleanup.
    pub fn complete(
        &mut self,
        cell_id: &str,
        status: MatrixCellState,
        now: Instant,
        error: Option<MatrixCellError>,
        artifact_ids: Vec<String>,
    ) -> Result<()> {
        if !matches!(
            status,
            MatrixCellState::Passed
                | MatrixCellState::Failed
                | MatrixCellState::Inconclusive
                | MatrixCellState::Unavailable
                | MatrixCellState::Cancelled
        ) {
            bail!("matrix cell '{cell_id}' completion must be terminal");
        }
        let current = self
            .states
            .get(cell_id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown matrix cell '{cell_id}'"))?;
        if !current.is_active() {
            bail!("matrix cell '{cell_id}' is not active");
        }
        if current == MatrixCellState::CancelRequested && status != MatrixCellState::Cancelled {
            bail!("cancel-requested matrix cell '{cell_id}' must complete as cancelled");
        }
        let cell = self.plan.cell(cell_id)?.clone();
        let started_at = self
            .started_at
            .remove(cell_id)
            .ok_or_else(|| anyhow::anyhow!("matrix cell '{cell_id}' has no start time"))?;
        let error = error.or_else(|| self.cancel_reasons.remove(cell_id));
        let result = MatrixCellResult {
            cell_id: cell.cell_id.clone(),
            target_id: cell.target_id,
            scenario_id: cell.scenario_id,
            required: cell.required,
            status,
            duration_ms: now
                .saturating_duration_since(started_at)
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            error,
            artifact_ids,
        };
        self.states.insert(cell_id.to_string(), status);
        self.results.insert(cell_id.to_string(), result);

        if status == MatrixCellState::Failed && self.plan.config.fail_fast {
            self.fail_fast_triggered = true;
            self.request_cancel("fail_fast_after_failure");
        }
        Ok(())
    }

    /// Records a cell that was rejected by the pre-dispatch admission pass.
    ///
    /// Admission failures are terminal before a runner is created, so there
    /// is no cleanup or start timestamp to account for. Keeping the cell in
    /// the scheduler report is important: an unavailable required target must
    /// remain visible and can never disappear through omission.
    pub fn mark_unavailable(&mut self, cell_id: &str, error: MatrixCellError) -> Result<()> {
        if self.states.get(cell_id) != Some(&MatrixCellState::Queued) {
            bail!("matrix cell '{cell_id}' is not queued for admission");
        }
        let cell = self.plan.cell(cell_id)?.clone();
        self.states
            .insert(cell_id.to_string(), MatrixCellState::Unavailable);
        self.results.insert(
            cell_id.to_string(),
            MatrixCellResult {
                cell_id: cell.cell_id,
                target_id: cell.target_id,
                scenario_id: cell.scenario_id,
                required: cell.required,
                status: MatrixCellState::Unavailable,
                duration_ms: 0,
                error: Some(error),
                artifact_ids: Vec::new(),
            },
        );
        Ok(())
    }

    pub fn state(&self, cell_id: &str) -> Option<MatrixCellState> {
        self.states.get(cell_id).copied()
    }

    pub fn cell_deadline(&self, cell_id: &str) -> Option<Instant> {
        let started_at = self.started_at.get(cell_id).copied()?;
        let cell = self.plan.cell(cell_id).ok()?;
        let timeout_ms = cell.timeout_ms.unwrap_or(self.plan.config.timeout_ms);
        Some(std::cmp::min(
            self.deadline,
            started_at + Duration::from_millis(timeout_ms),
        ))
    }

    pub fn cancel_requested_cells(&self) -> Vec<String> {
        self.states
            .iter()
            .filter_map(|(cell_id, state)| {
                (*state == MatrixCellState::CancelRequested).then_some(cell_id.clone())
            })
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.states.values().all(|state| state.is_terminal())
    }

    pub fn report(&self) -> Result<MatrixReport> {
        if !self.is_complete() {
            bail!("matrix report is incomplete");
        }
        let cells: Vec<MatrixCellResult> =
            self.plan
                .cells
                .iter()
                .map(|cell| {
                    self.results.get(&cell.cell_id).cloned().ok_or_else(|| {
                        anyhow::anyhow!("matrix cell '{}' has no result", cell.cell_id)
                    })
                })
                .collect::<Result<_>>()?;
        let status = summarize_cells(&cells)?;
        Ok(MatrixReport {
            plan_id: self.plan.plan_id.clone(),
            status,
            required_count: cells.iter().filter(|cell| cell.required).count(),
            required_passed: cells
                .iter()
                .filter(|cell| cell.required && cell.status == MatrixCellState::Passed)
                .count(),
            optional_non_passed: cells
                .iter()
                .filter(|cell| !cell.required && cell.status != MatrixCellState::Passed)
                .count(),
            cells,
        })
    }

    fn apply_deadlines(&mut self, now: Instant) {
        if now >= self.deadline {
            self.deadline_exceeded = true;
            self.request_cancel("matrix_deadline_exceeded");
            return;
        }
        let expired: Vec<String> = self
            .started_at
            .iter()
            .filter_map(|(cell_id, started_at)| {
                let cell = self.plan.cell(cell_id).ok()?;
                let timeout_ms = cell.timeout_ms.unwrap_or(self.plan.config.timeout_ms);
                (now >= *started_at + Duration::from_millis(timeout_ms)).then_some(cell_id.clone())
            })
            .collect();
        for cell_id in expired {
            self.mark_cancel_requested(&cell_id, "cell_deadline_exceeded");
        }
    }

    fn mark_cancel_requested(&mut self, cell_id: &str, reason: &str) {
        if self.states.get(cell_id) != Some(&MatrixCellState::Running) {
            return;
        }
        self.states
            .insert(cell_id.to_string(), MatrixCellState::CancelRequested);
        self.cancel_reasons.insert(
            cell_id.to_string(),
            MatrixCellError {
                code: reason.into(),
                message: reason.replace('_', " "),
            },
        );
    }

    fn cancel_queued(&mut self, cell_id: &str, reason: &str) {
        let Some(MatrixCellState::Queued) = self.states.get(cell_id).copied() else {
            return;
        };
        let Ok(cell) = self.plan.cell(cell_id) else {
            return;
        };
        self.states
            .insert(cell_id.to_string(), MatrixCellState::Cancelled);
        self.results.insert(
            cell_id.to_string(),
            MatrixCellResult {
                cell_id: cell.cell_id.clone(),
                target_id: cell.target_id.clone(),
                scenario_id: cell.scenario_id.clone(),
                required: cell.required,
                status: MatrixCellState::Cancelled,
                duration_ms: 0,
                error: Some(MatrixCellError {
                    code: reason.into(),
                    message: reason.replace('_', " "),
                }),
                artifact_ids: Vec::new(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(max_parallel: u16, fail_fast: bool) -> MatrixPlan {
        MatrixPlan {
            plan_id: "matrix-1".into(),
            config: MatrixConfig {
                max_parallel,
                fail_fast,
                timeout_ms: 1_000,
            },
            cells: vec![
                MatrixCellSpec {
                    cell_id: "required-a".into(),
                    target_id: "macos".into(),
                    scenario_id: "smoke".into(),
                    required: true,
                    timeout_ms: None,
                },
                MatrixCellSpec {
                    cell_id: "optional-b".into(),
                    target_id: "android".into(),
                    scenario_id: "smoke".into(),
                    required: false,
                    timeout_ms: Some(100),
                },
            ],
        }
    }

    fn error(code: &str) -> MatrixCellError {
        MatrixCellError {
            code: code.into(),
            message: code.into(),
        }
    }

    #[test]
    fn max_parallel_and_fail_fast_cancel_queued_cells() {
        let start = Instant::now();
        let mut scheduler = MatrixScheduler::new(plan(1, true), start).unwrap();
        assert_eq!(scheduler.dispatch_ready(start), vec!["required-a"]);
        scheduler
            .complete(
                "required-a",
                MatrixCellState::Failed,
                start + Duration::from_millis(10),
                Some(error("assertion_failed")),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            scheduler.state("optional-b"),
            Some(MatrixCellState::Cancelled)
        );
        assert!(scheduler.is_complete());
        assert_eq!(scheduler.report().unwrap().status, MatrixStatus::Failed);
    }

    #[test]
    fn deadline_requests_cleanup_before_a_running_cell_becomes_cancelled() {
        let start = Instant::now();
        let mut scheduler = MatrixScheduler::new(plan(1, false), start).unwrap();
        assert_eq!(scheduler.dispatch_ready(start), vec!["required-a"]);
        assert!(
            scheduler
                .dispatch_ready(start + Duration::from_secs(2))
                .is_empty()
        );
        assert_eq!(
            scheduler.state("required-a"),
            Some(MatrixCellState::CancelRequested)
        );
        assert_eq!(
            scheduler.state("optional-b"),
            Some(MatrixCellState::Cancelled)
        );
        scheduler
            .complete(
                "required-a",
                MatrixCellState::Cancelled,
                start + Duration::from_secs(2),
                None,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(scheduler.report().unwrap().status, MatrixStatus::Cancelled);
    }

    #[test]
    fn required_unavailable_or_inconclusive_never_summarizes_as_passed() {
        let mut cells = vec![MatrixCellResult {
            cell_id: "required".into(),
            target_id: "ios".into(),
            scenario_id: "smoke".into(),
            required: true,
            status: MatrixCellState::Unavailable,
            duration_ms: 0,
            error: Some(error("simulator_missing")),
            artifact_ids: Vec::new(),
        }];
        assert_eq!(summarize_cells(&cells).unwrap(), MatrixStatus::Unavailable);
        cells[0].status = MatrixCellState::Inconclusive;
        assert_eq!(summarize_cells(&cells).unwrap(), MatrixStatus::Inconclusive);
    }

    #[test]
    fn optional_non_passed_cell_produces_partial_after_required_passes() {
        let mut scheduler = MatrixScheduler::new(plan(2, false), Instant::now()).unwrap();
        let now = Instant::now();
        let dispatched = scheduler.dispatch_ready(now);
        assert_eq!(dispatched.len(), 2);
        scheduler
            .complete(
                "required-a",
                MatrixCellState::Passed,
                now,
                None,
                vec!["artifact-a".into()],
            )
            .unwrap();
        scheduler
            .complete(
                "optional-b",
                MatrixCellState::Unavailable,
                now,
                Some(error("no_android_emulator")),
                Vec::new(),
            )
            .unwrap();
        let report = scheduler.report().unwrap();
        assert_eq!(report.status, MatrixStatus::Partial);
        assert_eq!(report.required_passed, 1);
        assert_eq!(report.optional_non_passed, 1);
    }
}
