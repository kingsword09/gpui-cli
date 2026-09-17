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

| Target | Prerequisites |
| --- | --- |
| Desktop | Rust and the host platform's native build dependencies |
| iOS | macOS, Xcode, XcodeGen and the iOS Rust targets |
| Android | Android SDK/NDK, cargo-ndk, an Android Rust target and JDK 21 |

For Android, set `ANDROID_HOME` and `ANDROID_NDK_HOME` to the SDK and NDK
directories. Start an emulator or connect a device before running the app.
The tested emulator configuration on Apple Silicon uses host GPU rendering:

```bash
emulator -avd <avd-name> -gpu host
```

| Environment variable | Purpose |
| --- | --- |
| `GPUI_IOS_DEVICE` | iOS simulator name |
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

## License

Original code and artwork are available under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE). Bundled components retain their own licenses;
see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
