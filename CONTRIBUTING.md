# Development

The CLI is a Rust application. Templates are embedded at compile time with
`include_dir`, so rebuild before testing template changes.

Cargo manifests inside templates use a `.template` suffix so Cargo includes
their directories in the source package. Scaffolding restores the normal
`Cargo.toml` filename. Check the distributable with `cargo package --list`.

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --locked
```

Use `target/debug/gpui init` to generate a project in a temporary directory.
Check the selected targets, run their builds with the relevant platform
toolchains, and inspect the resulting applications when changing native assets.

CI also checks generated macOS projects in debug and release mode, and builds
the generated Android host in both modes with multiple ABIs. These checks use
titles containing quotes, XML characters and backslashes to exercise template
escaping. Android host packaging is separate from GPUI's Rust cross-compilation
and device execution; it does not validate native runtime behavior.

## Design documentation

Start with the [current status](docs/roadmap/current-status.md) and compare its
audited commit with your branch and `origin/main`. For an overall roadmap request,
follow the [closeout plan](docs/roadmap/closeout-plan.md) selection loop: resume an
eligible cursor, close gaps and automatically continue to the next eligible task.
One task or slice is not the whole run; a user-specified bounded scope takes
precedence. Read the matching implementation backlog, acceptance variants and relevant
design before editing. Record implementation, local-test, CI and native evidence
gaps separately; a shared acceptance case's task-local pass is not a full-case
pass. At handoff, update the cursor, remaining exit items, waiting conditions,
next eligible candidates and exact next action.
Proposed APIs and configuration examples must stay marked as drafts until their
implementation and required platform checks have passed.

For documentation-only changes, check links, task dependencies, acceptance IDs
and JSON/TOML examples with the repository's Rust `x` task runner:

```bash
cargo x check-design-docs
git diff --check
```

This checks documentation consistency, not runtime behavior or example artifact
hashes. Rust, native and GUI checks remain required for implementation changes.

## Artwork

The default window mark is original project artwork. Its palette and geometry
are defined in `scripts/generate-icons.py`. Regenerate the SVG, Android launcher
assets and iOS asset catalogs with:

```bash
uv run scripts/generate-icons.py
```

Generated assets are committed. Python, uv and Pillow are only needed when
changing artwork, not to build or use the CLI. iOS app icons must remain opaque;
Android adaptive foregrounds must stay inside the icon safe zone.

## Bundled dependencies

Keep GPUI version pins and the bundled renderer aligned. Record local changes
in the renderer's `PATCHES.md` and preserve original licenses. Update
`THIRD_PARTY_NOTICES.md` when adding bundled source or assets.

Local build output, experiments and tool metadata are excluded from version
control and the Cargo package.
