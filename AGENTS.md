# Project continuity

- Before assessing progress or selecting roadmap work, read
  [the current status](docs/roadmap/current-status.md) and the relevant task in
  [the implementation backlog](docs/roadmap/implementation-backlog.md).
- Compare the status document's audited commit with the current branch and
  `origin/main`. Review intervening changes before carrying forward a finding;
  historical audit reports and experiment notes describe their recorded commit.
- Keep implementation, local tests, CI and real GUI/device acceptance separate.
  Mark a work package `done` only when its required acceptance evidence exists.
  Update the status document and affected backlog entries when progress changes.
- Follow [CONTRIBUTING.md](CONTRIBUTING.md) for validation. Documentation checks
  establish consistency, not runtime or platform acceptance.
