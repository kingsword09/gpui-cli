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

## Mobile CI evidence

Mobile checks run independently of the existing template packaging checks:

- `doctor-android-cold` creates a real x86_64 AVD and checks ABI match/mismatch
  without booting it. `ios-simulator (doctor)` owns a fresh, stopped simulator and
  checks valid/unknown UDID selection. These are toolchain/inventory evidence.
- `android-emulator (doctor)` requires readable/writable KVM and a successful
  acceleration preflight, then boots an API 35 x86_64 emulator with SwiftShader.
  It records `adb` boot/ABI metadata and separate match/mismatch doctor reports.
- `android-native-build` builds a real GPUI APK without an emulator;
  `ios-simulator (build)` produces a linked Simulator `.app` without requiring
  boot or Metal. Fresh fixtures resolve and retain their `Cargo.lock` before
  the CLI's locked input discovery. Android cache bypass remains distinct from
  build failure; APK selection does not require a reusable-cache manifest.
- `android-emulator (smoke)` and `ios-simulator (smoke)` build unmodified GPUI
  applications through the CLI, install and launch them, require a stable app
  process for five seconds, save a device PNG and native logs, and clean up owned
  state. iOS runs on ARM64 `macos-15`, Xcode 26.2 and an iOS 26.2 runtime; the host
  Metal probe explicitly links CoreGraphics. A missing capability fails rather
  than silently skipping the runtime check.

Cold Android AVD creation and doctor share an explicit `ANDROID_AVD_HOME`, and
the real `config.ini` is retained in preflight artifacts. Linux emulator jobs
install `libpulse0` before invoking the emulator, including version/acceleration
preflight. iOS jobs install XcodeGen and both `aarch64-apple-ios` and
`aarch64-apple-ios-sim`; doctor requires the device target even when the selected
device is a Simulator. These preparations do not replace the required checks.

Only iOS smoke's live doctor may re-probe a case up to twice, 15 seconds apart,
when its selected-device check is correct and every unrelated required failure
is explicitly a probe timeout. Each failed report is retained separately;
this includes Rust target probe timeouts, whose `unknown` result does not
establish whether the target is installed.
`doctor-result.json` identifies the final validated reports, and the smoke
summary records recovery counts. Missing tools, invalid versions/targets and
device identity errors fail without retries; persistent timeouts still fail.

CI passes `--disable-push-service` only to iOS smoke. On its newly created
simulator, the driver removes `com.apple.apsd` from the simulator's foreground
user domain with `bootout`, verifies its absence, and records `simulator-services.json`.
The scope is the current boot; no persistent launchd disable override is needed
because the owned simulator is deleted after the run.
Only `bootout` and the absence query are needed; their configured deadline is
180 seconds, matching other native driver commands. The actual user domain is
read from the service-not-found response. Each command's observed duration and
failure remain recorded independently of that configured budget.
This avoids the APNs certificate/reconnect log storm observed on the hosted iOS
26.2 runtime. Service setup failures fail the job and still delete the owned
simulator. Cold doctor/build and default local smoke keep their normal services;
the CI smoke excludes push-notification behavior. App launch remains a single
attempt with a 180-second deadline, followed by the same process/capture checks.

The smoke summaries deliberately retain `verified_present=false`,
`application_ready=not_instrumented` and `gui_acceptance=not_run`. Process
survival and a device screenshot do not establish app-owned pixels, verified
presentation, input/semantics, or physical-device acceptance. See
[the mobile CI evidence scope](docs/experiments/mobile-ci-layers-2026-10-08.md).

Run driver regressions without SDKs or devices:

```bash
python3 -m unittest discover -s scripts/tests -v
```

On a prepared ARM64 Mac, run an isolated cold doctor or simulator smoke:

```bash
python3 scripts/check-ios-simulator.py --gpui target/debug/gpui --mode doctor --runtime 26.2 --output /tmp/gpui-ios-cold-evidence
python3 scripts/check-ios-simulator.py --gpui target/debug/gpui --mode smoke --runtime 26.2 --output /tmp/gpui-ios-smoke-evidence
```

Output directories must be new to prevent stale evidence from being mistaken
for a current pass. The drivers never install SDKs or modify signing settings;
toolchain installation is confined to the workflow setup steps. Each command,
exit/timeout, source revision/dirty state, driver/workflow hashes and failure
report is preserved before assertions. Only owned simulator UDIDs and the
application installed by the Android smoke driver are cleaned up. CI artifacts
are uploaded even on failure; a boot failure before the driver starts is
diagnosed from preflight artifacts and the emulator action's job log.

## Design documentation

The F01 `baseline-driver` artifact includes the current CLI, raw 10+30 mutation
samples and real setup-build cold/warm groups. Its `legacy/` directory preserves
the immutable `6d091b661d7ef82115e88cf142c7ff252b593864` source archive, rebuilt
legacy CLI, original CLI lockfile, generated macOS/iOS/Android v1 scaffold,
toolchain/commands and a SHA-256 inventory. It is a historical-source rebuild
with the recorded toolchain, not a recovered historical release binary or an
online compatibility result. Source extraction uses a temporary directory
outside the current Cargo workspace; old source files are not edited.

Reproduce the archive in a new output directory (Python 3.12 or later):

```bash
git fetch --no-tags --depth=1 origin 6d091b661d7ef82115e88cf142c7ff252b593864
python3 scripts/archive-f01-baseline.py --output /tmp/gpui-f01-legacy --expected-producer "$(git rev-parse HEAD)" --expected-dirty false
python3 scripts/archive-f01-baseline.py --output /tmp/gpui-f01-legacy --verify-only --expected-producer "$(git rev-parse HEAD)" --expected-dirty false
```

The expected dirty state must match the actual producer checkout. Creation
rejects existing directories, preserves failed command logs, and never installs
SDKs or modifies devices. Three legacy protocol unit tests and scaffold/hash
verification do not establish generated application builds or live old/new
compatibility; those remain separate F02 acceptance responsibilities.

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
