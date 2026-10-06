# Project continuity

- Before assessing progress or selecting roadmap work, read
  [the current status](docs/roadmap/current-status.md) and the relevant task in
  [the implementation backlog](docs/roadmap/implementation-backlog.md), then
  [the closeout execution plan](docs/roadmap/closeout-plan.md).
- Compare the status document's audited commit with the current branch and
  `origin/main`. Review intervening changes before carrying forward a finding;
  historical audit reports and experiment notes describe their recorded commit.
- Keep implementation, local tests, CI and real GUI/device acceptance separate.
  Mark a work package `done` only when its required acceptance evidence exists.
  Update the status document and affected backlog entries when progress changes.
- Follow [CONTRIBUTING.md](CONTRIBUTING.md) for validation. Documentation checks
  establish consistency, not runtime or platform acceptance.

# Closeout-first execution

- A request to advance the roadmap covers the whole core roadmap, not just the
  task named in an example or handoff. Resume an eligible execution cursor, close
  its gaps, update evidence and status, then automatically select the next
  dependency-eligible task. Do not stop merely because one slice or task is
  finished, or ask for routine permission to continue. A user-specified bounded
  task overrides this default scope.
- Keep one implementation focus at a time, but do not bind the entire run to
  that task. Select by dependencies, gate order, remaining closeout gaps and
  available acceptance environments, not recent commit topics or the existence
  of an unfinished module. Historical `in_progress` tasks are not all active
  work; waiting on one task does not block unrelated eligible work.
- Before editing implementation, record the task's remaining implementation,
  local-test, CI and GUI/device evidence gaps, acceptance variants, dependencies,
  non-goals and the bounded exit for this iteration. Check code and existing
  evidence; do not infer completeness from a status label.
- Work toward that task's exit before opening another work package. A merged
  slice or passing unit test is not a parent-task completion. Split large tasks
  into auditable acceptance slices without dropping required variants or changing
  the 35 parent-task counts to manufacture progress.
- For shared acceptance cases, follow the task-to-variant ownership in the
  acceptance matrix. A task-local pass is not a full-case or gate pass. Do not
  silently treat an implemented dependency subset as a completed hard dependency;
  reconcile any dependency mismatch explicitly before promoting the task.
- Switch focus only after closeout, an evidenced external blocker, an explicit
  user reprioritization, or a documented reproducible correctness/security defect.
  Record the reason, unfinished exit items and exact resume condition in the
  closeout plan; choose the next dependency-eligible task, not another convenient
  hardening slice. Code/evidence ready but awaiting required review, merge or CI
  is a recorded waiting state, not `done`; continue independent eligible work
  without claiming dependent gates passed. Do not use T05/T06 cache expansion as
  a default alternative to closing earlier tasks; select it only when the same
  dependency, scope and evidence rules justify it.
- At handoff, update the cursor, remaining gaps, evidence references, exact next
  action, waiting-task resume conditions and next eligible candidates, plus
  affected status/backlog entries. Stop the roadmap run only when its authorized
  scope is complete, no eligible work remains without an external prerequisite,
  the user asks to stop, or the execution limit requires handoff. Distinguish
  local completion from review/merge and unrun platform acceptance. Never change
  `done` merely to improve the count.
