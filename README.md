<img src="assets/app-icon.svg" width="72" height="72" alt="gpui-cli window icon">

# gpui-cli

Create, build and run native [GPUI](https://github.com/longbridge/gpui-kit) applications
for macOS, Windows, Linux, iOS and Android.

Desktop targets share a Cargo binary. Mobile targets get an Xcode or Gradle host
alongside a shared Rust UI crate.

## Install

From a local checkout:

```bash
cargo install --path . --locked
```

Alternatively, run `./install.sh` on macOS/Linux or `install.bat` on Windows.
The executable is named `gpui`.

## Create an app

```bash
gpui init my-app
gpui init my-app --targets macos,ios,android
gpui init my-app --targets android --title "My App" --bundle-id com.example.myapp
```

The interactive wizard asks for the name, display title, bundle identifier and
targets. Supply the options above for noninteractive use. Only the selected
platform files are generated.

## Build and run

Run commands from the generated project's root:

```bash
gpui doctor
gpui run desktop
gpui run ios
gpui run android
gpui build all
gpui build android --release
```

Android release builds use Gradle's output metadata to locate the APK. The
default template produces an unsigned release APK; configure a release
`signingConfig` in `mobile/android/gradle/app/build.gradle.kts` before using
`gpui run android --release`, or sign the build artifact separately.

Use `gpui run --live` to keep the loop open: sources are watched, every save
triggers an incremental rebuild and relaunch (desktop, iOS simulator or Android
emulator), and compile errors are shown with file, line and snippet while the
previous app keeps running until the fix lands. `--live` is debug-only; type
`r` + Enter to force a rebuild and `q` + Enter to quit.

The generated apps come with a dev channel wired in (debug builds only, no
extra dependencies):

- **Logs and panics** are forwarded to the CLI, so desktop and iOS-simulator
  output lands in one place with panic locations and backtraces.
- **Assets hot-reload**: images under `assets/` update in the running app
  without a rebuild (desktop, Android and the iOS simulator).
- **State survives restarts**: the generated click counter demonstrates the
  snapshot protocol — call `live::publish_state` from your render and read
  `live::take_restored_state()` when constructing your view to keep any
  JSON-serializable state across rebuilds. Every failure degrades to an
  announced cold start.

Agents can query the running live session from another terminal in the same
project (or one of its subdirectories):

```bash
gpui dev status --json
gpui dev diagnostics --json
gpui dev events --follow --json
gpui dev events --after <seq> --timeout 30s --json
```

JSON replies contain `ok`, `session_id`, `schema_version` and `result` (or
`error`). Take the cursor from `result.seq` in `status`, then request events
after that cursor. Event pages include `next_seq` and `has_more`; `--follow`
emits one event per line. Errors use a nonzero exit status. If multiple live
sessions are running, select one with `--session <id>`.

`status` separates the desired source/asset revisions from the running build
and process. A failed build keeps the previous process and marks its version
stale. Cargo diagnostics, including warnings, source spans and suggestions,
arrive during compilation; desktop stdout/stderr and process exits are also
recorded. Each launch has its own credentials, so older app templates still
get correctly attributed logs and panic reports.

Events and raw output are retained under `.gpui/live/<session_id>/`. The live
event cursor covers up to 2,048 events / 4 MiB; an expired cursor returns
`cursor_expired` with resynchronization information. On disk, event segments
use up to 8 MiB and raw output up to 32 MiB per session. Ended sessions remain
on disk for inspection; the query commands only connect to a live supervisor.

This implements D1 of the [agent feedback design](docs/DESIGN-agent-live-feedback.md).
UI responsiveness, screenshots, semantic inspection and automated interaction
are later phases and are reported as unavailable. Version freshness does not
confirm that a new frame has rendered; resource delivery is also reported
without a rendering acknowledgment. Native mobile log collection remains
outside D1.

| Target | Prerequisites |
| --- | --- |
| Desktop | Rust and the host platform's native build dependencies |
| iOS | macOS, Xcode, XcodeGen and the iOS Rust targets |
| Android | Android SDK/NDK, cargo-ndk, an Android Rust target and JDK 21 |

For Android, set `ANDROID_HOME` and `ANDROID_NDK_HOME` to the SDK and NDK
directories.

`GPUI_ANDROID_ABIS` accepts `arm64-v8a`, `armeabi-v7a`, `x86` and `x86_64`.
The CLI installs the matching Rust targets and passes the same ABI list to
Cargo and Gradle. For example, `GPUI_ANDROID_ABIS=arm64-v8a,x86_64 gpui build android`
packages both architectures. Existing projects with a hard-coded Gradle
`abiFilters` list need to adopt the `gpui.abis` / `GPUI_ANDROID_ABIS` handling
from [the current template](templates/android/gradle/app/build.gradle.kts).

## Devices and emulators

List what is installed, then pick one explicitly:

```bash
gpui device list                 # simulators, emulators and physical devices
gpui device list --all           # include ones that cannot currently be used
gpui device list --json

gpui device boot Pixel_9_Pro     # boot and wait until it is ready
gpui device boot --last          # boot the most recently used
gpui device shutdown Pixel_9_Pro
gpui device shutdown --all

gpui device create --platform ios --name Test --type "iPhone 17"
gpui device create --platform android --name Test \
  --image "system-images;android-36;google_apis_playstore;arm64-v8a" --device pixel_9_pro
gpui device remove Test --yes
```

`gpui run` and `gpui build` accept the selection directly:

```bash
gpui run ios --sim "iPhone 17 Pro@26.2"
gpui run ios --device-only        # require a physical iOS device
gpui run android --avd Pixel_9a
gpui run android --device <serial>
```

Selection is resolved in this order: CLI flags, then the `[run]` section of
`gpui.toml`, then the environment variables below, then an interactive prompt
(when stdin and stdout are a terminal), then a documented default. Pin devices
per project so runs are reproducible:

```toml
[run]
ios_simulator = "iPhone 17 Pro@26.2"   # name@runtime; runtime optional
android_avd = "Pixel_9a"
```

| Environment variable | Purpose |
| --- | --- |
| `GPUI_IOS_DEVICE` | iOS simulator name (or `name@runtime`) |
| `GPUI_IOS_DEVICE_ID` | Connected iOS device identifier |
| `GPUI_ANDROID_ABIS` | Android ABIs, comma separated; default `arm64-v8a` |
| `ANDROID_SERIAL` | Select the adb device |
| `JAVA_HOME` | Select the JDK used by Gradle |

Other commands:

```bash
gpui init --add --targets ios
gpui info
gpui completions zsh
```

Adding platforms preserves files that do not need updating. If a required
update conflicts with an edited file, the command stops before writing changes.

## Project layout

```text
my-app/
├── Cargo.toml
├── gpui.toml
├── crates/
│   ├── app/                 # Shared UI and mobile entry points
│   └── desktop/             # Desktop binary, when selected
├── mobile/
│   ├── ios/                 # XcodeGen project, Swift host and assets
│   └── android/             # Gradle host and JNI bridge
└── vendor/                  # Renderer compatibility crate for Android
```

GPUI dependencies use a pinned, compatible version set. Android projects include
a small renderer override; keep its `vendor/` directory and the workspace's
`[patch.crates-io]` entry together when versioning the app. Pins are defined in
[src/template.rs](src/template.rs).

The templates use original window artwork. Replace the Android launcher resources
and iOS asset catalog to brand a generated application.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for checks, template development and icon
generation. Templates are embedded in the CLI binary, so rebuild the CLI after
changing them.

The [agent-native development roadmap](docs/ROADMAP-agent-native-development.md)
contains detailed designs, PR-sized tasks, acceptance cases and configuration
drafts for cross-platform observation, scenarios, performance and Agent tooling.
These are planned capabilities, not commands available in the current release.

## License

Original code and artwork are available under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE). Bundled components retain their own licenses;
see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
