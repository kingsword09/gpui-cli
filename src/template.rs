use anyhow::{Context, Result, bail};
use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::template_manifest::{
    BaselineInfo, DependencyInfo, GeneratorInfo, ManifestFile, ManifestGroup, TemplateManifest,
};

/// Full built-in template tree, embedded at compile time.
static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

const LEGACY_GITIGNORE: &str = r#"/target
**/*.rs.bk
.DS_Store
.idea/
.vscode/

# Live-mode dev channel credentials (\`gpui run --live\`)
.gpui/

# iOS
mobile/ios/build/
mobile/ios/*.xcodeproj/
mobile/ios/*.xcworkspace/

# Android
mobile/android/gradle/.gradle/
mobile/android/gradle/build/
mobile/android/gradle/app/build/
mobile/android/gradle/local.properties
"#;

/// GPUI is published as a family of `gpui-pre-*` crates that must all share one
/// version. These constants centralise the pins the generated projects use.
pub const GPUI_PRE_VERSION: &str = "0.3.5";
pub const GPUI_KIT_GIT: &str = "https://github.com/longbridge/gpui-kit";
/// Published package version paired with the immutable gpui-kit revision.
pub const GPUI_KIT_VERSION: &str = "0.6.1";
/// `gpui-kit` needs a git revision rather than the published crate: on
/// iOS/Android it must not pull in the desktop-only `gpui-pre-platform` crate.
pub const GPUI_KIT_REV: &str = "9504b4658d57c59024a664f22b5ab55e070a24b4";
pub const GPUI_MOBILE_GIT: &str = "https://github.com/longbridge/gpui-mobile.git";
/// Mobile platform revision validated with the pinned GPUI family and renderer patch.
pub const GPUI_MOBILE_REV: &str = "b4e3ab258f271003b7d4b874f7c5ebe3a77fac60";
/// Published package version paired with the immutable gpui-mobile revision.
pub const GPUI_MOBILE_VERSION: &str = "0.1.0";
/// Version shared by generated workspace packages and path dependencies.
pub const TEMPLATE_PACKAGE_VERSION: &str = "0.1.0";
/// Stable identifier for the template layout represented by this CLI.
pub const TEMPLATE_VERSION: &str = "agent-native-v1-draft";
/// Historical baseline retained so upgrade apply can exercise real migrations.
pub const LEGACY_TEMPLATE_VERSION: &str = "agent-native-v0-legacy";
/// Package name of the crate at the root of `GPUI_MOBILE_GIT`.
pub const GPUI_MOBILE_PKG: &str = "gpui-pre-mobile";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiFramework {
    /// `gpui-kit`: styled components on top of GPUI. The only supported choice
    /// today, since the generated entry points assume its `application()` /
    /// `init()` helpers.
    GpuiKit,
}

impl UiFramework {
    pub fn label(&self) -> &'static str {
        match self {
            UiFramework::GpuiKit => "gpui-kit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
    IOs,
    Android,
}

impl Platform {
    pub fn is_desktop(&self) -> bool {
        matches!(self, Platform::MacOs | Platform::Windows | Platform::Linux)
    }

    pub fn is_mobile(&self) -> bool {
        matches!(self, Platform::IOs | Platform::Android)
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
            Platform::IOs => "iOS",
            Platform::Android => "Android",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::MacOs => "macos",
            Platform::Windows => "windows",
            Platform::Linux => "linux",
            Platform::IOs => "ios",
            Platform::Android => "android",
        }
    }

    /// Parses a user-facing target name (`desktop`, `ios`, `android`, ...).
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "macos" | "mac" | "osx" | "darwin" => Some(Platform::MacOs),
            "windows" | "win" => Some(Platform::Windows),
            "linux" => Some(Platform::Linux),
            "ios" => Some(Platform::IOs),
            "android" => Some(Platform::Android),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    /// Cargo/package identifier, e.g. `delta-mobile`.
    pub name: String,
    /// Human-readable window/app title.
    pub title: String,
    pub bundle_id: String,
    pub ui_framework: UiFramework,
    pub targets: Vec<Platform>,
}

impl ProjectConfig {
    pub fn has_desktop(&self) -> bool {
        self.targets.iter().any(|t| t.is_desktop())
    }

    pub fn has_ios(&self) -> bool {
        self.targets.contains(&Platform::IOs)
    }

    pub fn has_android(&self) -> bool {
        self.targets.contains(&Platform::Android)
    }

    pub fn has_mobile(&self) -> bool {
        self.has_ios() || self.has_android()
    }

    /// `delta-mobile` -> `delta_mobile`
    pub fn crate_ident(&self) -> String {
        self.name.replace('-', "_")
    }

    /// Shared UI crate name, e.g. `delta-mobile-app`.
    pub fn app_crate(&self) -> String {
        format!("{}-app", self.name)
    }

    /// `lib` target name of the shared crate, e.g. `delta_mobile_app`.
    pub fn app_lib_name(&self) -> String {
        format!("{}_app", self.crate_ident())
    }

    pub fn desktop_crate(&self) -> String {
        format!("{}-desktop", self.name)
    }

    /// PascalCase name used for the Xcode target and the Gradle root project.
    pub fn xcode_target(&self) -> String {
        self.name
            .split(['-', '_', ' '])
            .filter(|s| !s.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect()
    }

    /// `com.example.delta` -> `com.example`
    pub fn bundle_prefix(&self) -> String {
        match self.bundle_id.rsplit_once('.') {
            Some((prefix, _)) => prefix.to_string(),
            None => self.bundle_id.clone(),
        }
    }
}

/// Replaces every `{{KEY}}` placeholder in `input`.
fn substitute(input: &str, vars: &HashMap<&str, String>) -> String {
    // Scan the template once. A user title containing another placeholder is
    // literal text, not a second template expansion (HashMap order is random).
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find("}}") else { break };
        let placeholder = &rest[..end + 2];
        out.push_str(
            vars.get(&rest[2..end])
                .map(String::as_str)
                .unwrap_or(placeholder),
        );
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;")
}

fn android_string(value: &str) -> String {
    // Android has an additional string-resource escaping layer after XML.
    // Surrounding quotes also preserve whitespace and literal leading @ / ?.
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    xml_escape(&format!("\"{escaped}\""))
}

/// Emits the mobile entry points for the shared crate.
///
/// iOS: `App.swift` calls `gpui_ios_register_app()` before `gpui_ios_run_demo()`,
/// so the run loop knows which view to build once UIKit finishes launching.
/// Android: `android-activity` loads the `cdylib` and calls `android_main` on a
/// dedicated native thread; the window arrives later as an NDK lifecycle event.
fn mobile_entry(config: &ProjectConfig) -> String {
    if !config.has_mobile() {
        return String::new();
    }

    // Shared by the iOS and Android entry points below. Gated on the mobile
    // `cfg`s so a host-target build of this crate has no dead code.
    // The imports below are only used by `open_main_window`, which exists only
    // on mobile targets; keeping them gated avoids unused-import warnings when
    // the crate is built for the host.
    let mut out = String::from(
        r#"
/// Wires up gpui-kit and opens the main window.
///
/// `gpui_kit::init` must run before any view is created: it installs the theme
/// and global state that `Root` relies on to paint a background. Wrapping the
/// content in `Root` is what gives the window its themed surface.
#[cfg(any(target_os = "ios", target_os = "android"))]
fn open_main_window(cx: &mut App) {
    use gpui_kit::component::{Root, Theme, ThemeMode};

    gpui_kit::init(cx);
    Theme::change(ThemeMode::Light, None, cx);

    // Hot-reloads images changed under `assets/` while `gpui run --live` is
    // running (Android via pushed files, iOS simulator via the host path).
    #[cfg(all(debug_assertions, any(target_os = "android", target_os = "ios")))]
    crate::pump_live_assets(cx);

    cx.open_window(WindowOptions::default(), |window, cx| {
        crate::register_window_with_handle(
            "main",
            {{APP_TITLE_RUST}},
            800,
            600,
            1000,
            true,
            window.window_handle(),
        );
        cx.on_app_quit(|_| async {
            crate::close_window("main", Some("app_quit"));
        })
        .detach();
        let view = cx.new(|_| MainView::new());
        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("failed to open the main window");

    cx.activate(true);
}
"#,
    );
    out = out.replace("{{APP_TITLE_RUST}}", &format!("{:?}", config.title));

    if config.has_ios() {
        out.push_str(
            r#"
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn gpui_ios_register_app() {
    // Debug builds connect back to `gpui run --live` for logs and panics.
    crate::init_live(None);

    gpui_mobile::ios::ffi::set_app_callback(Box::new(|cx: &mut App| {
        open_main_window(cx);
    }));
}
"#,
        );
    }

    if config.has_android() {
        out.push_str(&format!(
            r#"
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: android_activity::AndroidApp) {{
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("{}"),
    );

    // Debug builds connect back to `gpui run --live`; the CLI stages its
    // dev-channel credentials into `gpui_live.txt` before launching. The
    // connection happens below, after the asset source is installed, so the
    // hello carries the asset-reload capability.
    gpui_mobile::android::jni::install_panic_hook();

    let _platform = gpui_mobile::android::jni::init_platform(&app);
    let Some(shared) = gpui_mobile::android::jni::shared_platform() else {{
        log::error!("shared_platform() returned None - aborting");
        return;
    }};

    // Live development reads assets pushed into the app's files dir by the
    // CLI; release builds keep the default asset source.
    #[cfg(debug_assertions)]
    let application = Application::with_platform(shared.into_rc()).with_assets(
        crate::dev_asset_source(app.internal_data_path().map(|p| p.join("assets"))),
    );
    #[cfg(not(debug_assertions))]
    let application = Application::with_platform(shared.into_rc());

    // The dev client reads `gpui_live.txt` from the app files dir; resolve
    // the concrete file path here (internal_data_path is the directory).
    let live_config = app
        .internal_data_path()
        .map(|dir| dir.join("gpui_live.txt"));
    crate::init_live(live_config.as_deref());

    application.run(|cx: &mut App| {{
        open_main_window(cx);
    }});
}}
"#,
            config.name
        ));
    }

    out
}

fn vars_for(config: &ProjectConfig) -> HashMap<&'static str, String> {
    let mut vars: HashMap<&'static str, String> = HashMap::new();

    vars.insert("PROJECT_NAME", config.name.clone());
    vars.insert("APP_TITLE", config.title.clone());
    vars.insert("APP_TITLE_RUST", format!("{:?}", config.title));
    vars.insert(
        "APP_TITLE_TOML",
        toml::Value::String(config.title.clone()).to_string(),
    );
    vars.insert("APP_TITLE_XML", xml_escape(&config.title));
    vars.insert("APP_TITLE_ANDROID", android_string(&config.title));
    vars.insert("APP_TITLE_COMMENT", config.title.replace(['\r', '\n'], " "));
    vars.insert("BUNDLE_ID", config.bundle_id.clone());
    vars.insert("BUNDLE_PREFIX", config.bundle_prefix());
    vars.insert("XCODE_TARGET", config.xcode_target());
    vars.insert("APP_CRATE", config.app_crate());
    vars.insert("APP_LIB_NAME", config.app_lib_name());
    vars.insert("DESKTOP_CRATE", config.desktop_crate());
    vars.insert("GPUI_PRE_VERSION", GPUI_PRE_VERSION.to_string());
    vars.insert("GPUI_KIT_GIT", GPUI_KIT_GIT.to_string());
    vars.insert("GPUI_KIT_REV", GPUI_KIT_REV.to_string());
    vars.insert("GPUI_MOBILE_GIT", GPUI_MOBILE_GIT.to_string());
    vars.insert("GPUI_MOBILE_REV", GPUI_MOBILE_REV.to_string());
    vars.insert("GPUI_MOBILE_PKG", GPUI_MOBILE_PKG.to_string());
    vars.insert("UI_FRAMEWORK", config.ui_framework.label().to_string());

    // Workspace member list: keep the desktop entry only when it exists.
    let desktop_member = if config.has_desktop() {
        "    \"crates/desktop\",\n".to_string()
    } else {
        String::new()
    };
    vars.insert("DESKTOP_MEMBER", desktop_member);

    // gpui-mobile is only needed when a mobile platform is targeted.
    let mobile_dep = if config.has_mobile() {
        "gpui-mobile.workspace = true\n".to_string()
    } else {
        String::new()
    };
    vars.insert("MOBILE_DEP", mobile_dep);
    vars.insert("GPUI_KIT_VERSION", GPUI_KIT_VERSION.to_string());
    vars.insert("GPUI_MOBILE_VERSION", GPUI_MOBILE_VERSION.to_string());
    vars.insert(
        "TEMPLATE_PACKAGE_VERSION",
        TEMPLATE_PACKAGE_VERSION.to_string(),
    );

    let android_section = if config.has_android() {
        r#"
[target.'cfg(target_os = "android")'.dependencies]
android-activity = { version = "0.6", features = ["native-activity"] }
android_logger = "0.15"
"#
        .to_string()
    } else {
        String::new()
    };
    vars.insert("ANDROID_SECTION", android_section);

    let android_patch = if config.has_android() {
        format!(
            "\n# Bundled renderer; see vendor/gpui-pre-wgpu-{GPUI_PRE_VERSION}/PATCHES.md.\n\
             [patch.crates-io]\n\
             gpui-pre-wgpu = {{ path = \"vendor/gpui-pre-wgpu-{GPUI_PRE_VERSION}\" }}\n"
        )
    } else {
        String::new()
    };
    vars.insert("ANDROID_PATCH", android_patch);

    vars.insert("MOBILE_ENTRY", mobile_entry(config));

    vars.insert(
        "TARGETS",
        config
            .targets
            .iter()
            .map(|t| format!("\"{}\"", t.as_str()))
            .collect::<Vec<_>>()
            .join(", "),
    );

    vars.insert(
        "TARGETS_LIST",
        config
            .targets
            .iter()
            .map(|t| t.display_name())
            .collect::<Vec<_>>()
            .join(", "),
    );

    let mut commands = Vec::new();
    let mut notes = String::new();
    if config.has_desktop() {
        commands.push("gpui run desktop");
    }
    if config.has_ios() {
        commands.push("gpui run ios");
        notes.push_str(
            "## iOS\n\nUse macOS with Xcode and XcodeGen installed. Set \
             `GPUI_IOS_DEVICE` to select a simulator, or `GPUI_IOS_DEVICE_ID` \
             to use a connected device. App artwork is in \
             `mobile/ios/Assets.xcassets/`.\n\n",
        );
    }
    if config.has_android() {
        commands.push("gpui run android");
        notes.push_str(&format!(
            "## Android\n\nInstall cargo-ndk, Android SDK/NDK and JDK 21. Set \
             `ANDROID_HOME` and `ANDROID_NDK_HOME`, then connect a device or \
             start an emulator. The tested emulator mode uses \
             `emulator -avd <avd-name> -gpu host`.\n\n\
             Keep `vendor/gpui-pre-wgpu-{GPUI_PRE_VERSION}` and the workspace \
             `[patch.crates-io]` entry together. Launcher artwork is in \
             `mobile/android/gradle/app/src/main/res/`.\n\n"
        ));
    }
    vars.insert("RUN_COMMANDS", commands.join("\n"));
    vars.insert("PLATFORM_NOTES", notes);

    vars
}

fn is_binary_asset(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("png")
            | Some("jpg")
            | Some("jpeg")
            | Some("gif")
            | Some("jar")
            | Some("ttf")
            | Some("otf")
            | Some("ico")
            | Some("zip")
    )
}

/// Scripts embedded via `include_dir` lose their executable bit, so restore it
/// for the ones we generate (notably Gradle's `gradlew`, which runs directly).
fn is_executable_script(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name == "gradlew" || name.ends_with(".sh")
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Copies an embedded directory into `dest`, substituting placeholders in text
/// files and writing binary assets through byte-for-byte.
fn copy_dir(dir: &Dir<'_>, dest: &Path, vars: &HashMap<&str, String>) -> Result<()> {
    fs::create_dir_all(dest)?;

    for file in dir.files() {
        let name = file
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("template file {:?} has no name", file.path()))?;
        let target = dest.join(name.strip_suffix(".template").unwrap_or(name));
        let contents = file.contents();

        if is_binary_asset(file.path()) {
            fs::write(&target, contents)?;
        } else {
            let text = std::str::from_utf8(contents)
                .with_context(|| format!("template file {:?} is not UTF-8", file.path()))?;
            fs::write(&target, substitute(text, vars))?;
        }

        if is_executable_script(file.path()) {
            set_executable(&target)?;
        }
    }

    for sub in dir.dirs() {
        let name = sub
            .path()
            .file_name()
            .with_context(|| format!("template dir {:?} has no name", sub.path()))?;
        copy_dir(sub, &dest.join(name), vars)?;
    }

    Ok(())
}

fn render_subtree(template_path: &str, dest: &Path, vars: &HashMap<&str, String>) -> Result<()> {
    let dir = TEMPLATES
        .get_dir(template_path)
        .with_context(|| format!("built-in template '{template_path}' is missing"))?;
    copy_dir(dir, dest, vars)
}

fn render_file(template_path: &str, dest: &Path, vars: &HashMap<&str, String>) -> Result<()> {
    let file = TEMPLATES
        .get_file(template_path)
        .with_context(|| format!("built-in template '{template_path}' is missing"))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = std::str::from_utf8(file.contents())
        .with_context(|| format!("template file '{template_path}' is not UTF-8"))?;
    fs::write(dest, substitute(text, vars))?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn is_legacy_gitignore(bytes: &[u8]) -> bool {
    let escaped_tick = format!("{}{}", '\\', '\u{60}');
    String::from_utf8_lossy(bytes).replace('\u{60}', &escaped_tick) == LEGACY_GITIGNORE
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn collect_embedded_files(
    dir: &Dir<'_>,
    prefix: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    for file in dir.files() {
        let name = file
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("embedded template file {:?} has no name", file.path()))?;
        out.push((slash_path(&prefix.join(name)), file.contents().to_vec()));
    }
    for sub in dir.dirs() {
        let name = sub
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("embedded template directory {:?} has no name", sub.path()))?;
        collect_embedded_files(sub, &prefix.join(name), out)?;
    }
    Ok(())
}

fn current_template_content_id() -> &'static str {
    static CONTENT_ID: OnceLock<String> = OnceLock::new();
    CONTENT_ID.get_or_init(|| {
        let mut files = Vec::new();
        collect_embedded_files(&TEMPLATES, Path::new(""), &mut files)
            .expect("embedded template paths must be valid");
        files.sort_by(|left, right| left.0.cmp(&right.0));

        let mut digest = Sha256::new();
        for (path, bytes) in files {
            digest.update(path.as_bytes());
            digest.update([0]);
            digest.update(bytes);
            digest.update([0]);
        }
        format!("sha256:{:x}", digest.finalize())
    })
}

fn legacy_template_content_id() -> String {
    let mut digest = Sha256::new();
    digest.update(LEGACY_TEMPLATE_VERSION.as_bytes());
    digest.update([0]);
    digest.update(current_template_content_id().as_bytes());
    digest.update([0]);
    digest.update(LEGACY_LIB_PREFIX.as_bytes());
    digest.update([0]);
    digest.update(LEGACY_RUNTIME_SOURCE.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

fn template_path_for_output(relative: &Path) -> Option<String> {
    let value = slash_path(relative);
    let direct = match value.as_str() {
        "Cargo.toml" => Some("workspace.Cargo.toml"),
        "gpui.toml" => Some("gpui.toml"),
        ".gitignore" => Some("gitignore"),
        "README.md" => Some("README.md"),
        "THIRD_PARTY_NOTICES.md" => Some("THIRD_PARTY_NOTICES.md"),
        "crates/app/Cargo.toml" => Some("app/Cargo.toml.template"),
        "crates/app/src/lib.rs" => Some("app/src/lib.rs"),
        "crates/app/src/live.rs" => Some("app/src/live.rs"),
        "crates/app/src/agent_runtime.rs" => Some("app/src/agent_runtime.rs"),
        "crates/app/src/legacy_runtime.rs" => Some("legacy/app/src/legacy_runtime.rs"),
        "crates/desktop/Cargo.toml" => Some("desktop/Cargo.toml.template"),
        "crates/desktop/src/main.rs" => Some("desktop/src/main.rs"),
        "mobile/android/.cargo/config.toml" => Some("cargo-config.toml"),
        _ => None,
    };
    if let Some(path) = direct {
        return Some(path.to_string());
    }
    value
        .strip_prefix("assets/")
        .map(|suffix| format!("assets/{suffix}"))
        .or_else(|| {
            value
                .strip_prefix("licenses/")
                .map(|suffix| format!("licenses/{suffix}"))
        })
        .or_else(|| {
            value
                .strip_prefix("mobile/ios/")
                .map(|suffix| format!("ios/{suffix}"))
        })
        .or_else(|| {
            value
                .strip_prefix("mobile/android/")
                .map(|suffix| format!("android/{suffix}"))
        })
        .or_else(|| {
            value
                .strip_prefix("vendor/")
                .map(|suffix| format!("android-compat/{suffix}"))
        })
}

fn manifest_group_for(relative: &Path, config: &ProjectConfig) -> &'static str {
    let value = slash_path(relative);
    if value.starts_with("mobile/ios/") {
        "ios-host"
    } else if value.starts_with("mobile/android/")
        || value.starts_with("vendor/")
        || (value == "Cargo.toml" && config.has_android())
    {
        "android-renderer"
    } else if value.starts_with("crates/desktop/") {
        "desktop-host"
    } else if value.starts_with("crates/app/") || value.starts_with("assets/") {
        "app-runtime"
    } else if value == "Cargo.toml" || value == "gpui.toml" {
        "project-config"
    } else {
        "project-support"
    }
}

fn manifest_groups() -> Vec<ManifestGroup> {
    [
        ("android-renderer", true),
        ("app-runtime", true),
        ("desktop-host", true),
        ("ios-host", true),
        ("project-config", true),
        ("project-support", true),
    ]
    .into_iter()
    .map(|(id, atomic)| ManifestGroup {
        id: id.to_string(),
        atomic,
    })
    .collect()
}

pub(crate) fn build_template_manifest(
    root: &Path,
    config: &ProjectConfig,
) -> Result<TemplateManifest> {
    build_template_manifest_for_version(root, config, TEMPLATE_VERSION)
}

pub(crate) fn build_template_manifest_for_version(
    root: &Path,
    config: &ProjectConfig,
    version: &str,
) -> Result<TemplateManifest> {
    if !is_supported_template_version(version) {
        bail!(
            "baseline_unavailable: template '{}' is not embedded in this CLI",
            version
        );
    }
    let mut paths = Vec::new();
    collect_files(root, &mut paths)?;

    let mut files = Vec::new();
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("cannot relativize generated file {}", path.display()))?;
        if slash_path(relative) == crate::template_manifest::MANIFEST_RELATIVE_PATH {
            continue;
        }
        let template_path = template_path_for_output(relative).with_context(|| {
            format!(
                "generated file '{}' has no embedded template source",
                relative.display()
            )
        })?;
        let bytes = fs::read(&path)?;
        files.push(ManifestFile {
            path: slash_path(relative),
            group: manifest_group_for(relative, config).to_string(),
            base_sha256: sha256(&bytes),
            template_path,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut platforms: Vec<String> = config
        .targets
        .iter()
        .map(|platform| platform.as_str().to_string())
        .collect();
    platforms.sort();
    platforms.dedup();

    let content_id = template_content_id(version);
    Ok(TemplateManifest {
        schema_version: crate::template_manifest::SCHEMA_VERSION,
        generator: GeneratorInfo {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        template_version: version.to_string(),
        platforms,
        dependencies: DependencyInfo {
            gpui_pre_version: GPUI_PRE_VERSION.to_string(),
            gpui_kit_revision: GPUI_KIT_REV.to_string(),
            gpui_mobile_revision: GPUI_MOBILE_REV.to_string(),
        },
        baseline: BaselineInfo {
            template_version: version.to_string(),
            content_id,
            distribution: "embedded-version-package".to_string(),
        },
        groups: manifest_groups(),
        files,
    })
}

pub fn read_template_baseline(
    config: &ProjectConfig,
    manifest: &TemplateManifest,
    relative: &Path,
) -> Result<Vec<u8>> {
    if !is_supported_template_version(&manifest.template_version)
        || manifest.baseline.template_version != manifest.template_version
        || manifest.baseline.content_id != template_content_id(&manifest.template_version)
    {
        bail!(
            "baseline_unavailable: template '{}' is not embedded in this CLI",
            manifest.template_version
        );
    }
    let relative_string = slash_path(relative);
    let Some(entry) = manifest.file(&relative_string) else {
        bail!(
            "baseline_unavailable: '{}' is not managed by the template",
            relative_string
        );
    };
    let scratch = tempfile::tempdir().context("cannot prepare template baseline")?;
    scaffold_files_for_version(scratch.path(), config, &manifest.template_version)?;
    let path = scratch.path().join(relative);
    let bytes = fs::read(&path)
        .with_context(|| format!("baseline file '{}' is unavailable", relative_string))?;
    if sha256(&bytes) != entry.base_sha256 {
        bail!(
            "baseline_unavailable: embedded content for '{}' does not match its manifest hash",
            relative_string
        );
    }
    Ok(bytes)
}

/// Writes the project described by `config` into `target_dir`.
pub fn scaffold(target_dir: &Path, config: &ProjectConfig) -> Result<()> {
    scaffold_version(target_dir, config, TEMPLATE_VERSION)
}

/// Writes a supported version of the project template into `target_dir`.
pub fn scaffold_version(target_dir: &Path, config: &ProjectConfig, version: &str) -> Result<()> {
    scaffold_files_for_version(target_dir, config, version)?;
    let manifest = build_template_manifest_for_version(target_dir, config, version)?;
    manifest.write(target_dir)
}

pub fn is_supported_template_version(version: &str) -> bool {
    matches!(version, TEMPLATE_VERSION | LEGACY_TEMPLATE_VERSION)
}

pub fn template_content_id(version: &str) -> String {
    match version {
        TEMPLATE_VERSION => current_template_content_id().to_owned(),
        LEGACY_TEMPLATE_VERSION => legacy_template_content_id(),
        _ => String::new(),
    }
}

fn scaffold_files_for_version(
    target_dir: &Path,
    config: &ProjectConfig,
    version: &str,
) -> Result<()> {
    if !is_supported_template_version(version) {
        bail!(
            "baseline_unavailable: template '{}' is not embedded in this CLI",
            version
        );
    }
    scaffold_files(target_dir, config)?;
    if version == LEGACY_TEMPLATE_VERSION {
        fs::remove_file(target_dir.join("crates/app/src/agent_runtime.rs"))?;
        fs::write(
            target_dir.join("crates/app/src/legacy_runtime.rs"),
            LEGACY_RUNTIME_SOURCE,
        )?;
        let lib = target_dir.join("crates/app/src/lib.rs");
        let bytes = fs::read(&lib)?;
        let mut legacy = Vec::with_capacity(LEGACY_LIB_PREFIX.len() + bytes.len());
        legacy.extend_from_slice(LEGACY_LIB_PREFIX.as_bytes());
        legacy.extend_from_slice(&bytes);
        fs::write(lib, legacy)?;
    }
    Ok(())
}

const LEGACY_LIB_PREFIX: &str = "// Historical agent-native-v0 baseline.\n";
const LEGACY_RUNTIME_SOURCE: &str = "//! Runtime marker removed by the agent-native-v1 template.\n\n\
pub const LEGACY_RUNTIME_REVISION: &str = \"agent-native-v0-legacy\";\n";

fn scaffold_files(target_dir: &Path, config: &ProjectConfig) -> Result<()> {
    if config.targets.is_empty() {
        bail!("at least one target platform is required");
    }
    // Identifiers appear in code, manifest keys and paths. Reject invalid
    // identifiers before creating files; display titles are escaped separately.
    if !config.name.starts_with(|c: char| c.is_ascii_alphabetic())
        || !config
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("Project name must start with a letter and contain only letters, digits, '-' or '_'");
    }
    let segments: Vec<_> = config.bundle_id.split('.').collect();
    if segments.len() < 2
        || segments.iter().any(|part| {
            !part.starts_with(|c: char| c.is_ascii_alphabetic())
                || !part.chars().all(|c| {
                    c.is_ascii_alphanumeric() || c == '_' || (c == '-' && !config.has_android())
                })
        })
    {
        bail!(
            "Invalid bundle identifier '{}': use dot-separated identifiers starting with a letter (e.g. com.example.myapp)",
            config.bundle_id
        );
    }
    if config.title.chars().any(|c| {
        (c.is_control() && !matches!(c, '\n' | '\r' | '\t')) || matches!(c, '\u{fffe}' | '\u{ffff}')
    }) {
        bail!("Application title contains control characters unsupported by native manifests");
    }
    let vars = vars_for(config);
    fs::create_dir_all(target_dir)?;

    render_file(
        "workspace.Cargo.toml",
        &target_dir.join("Cargo.toml"),
        &vars,
    )?;
    render_file("gpui.toml", &target_dir.join("gpui.toml"), &vars)?;
    render_file("gitignore", &target_dir.join(".gitignore"), &vars)?;
    render_file("README.md", &target_dir.join("README.md"), &vars)?;
    render_file(
        "THIRD_PARTY_NOTICES.md",
        &target_dir.join("THIRD_PARTY_NOTICES.md"),
        &vars,
    )?;
    render_subtree("licenses", &target_dir.join("licenses"), &HashMap::new())?;

    render_file(
        "app/Cargo.toml.template",
        &target_dir.join("crates/app/Cargo.toml"),
        &vars,
    )?;
    render_file(
        "app/src/lib.rs",
        &target_dir.join("crates/app/src/lib.rs"),
        &vars,
    )?;
    render_file(
        "app/src/live.rs",
        &target_dir.join("crates/app/src/live.rs"),
        &vars,
    )?;
    render_file(
        "app/src/agent_runtime.rs",
        &target_dir.join("crates/app/src/agent_runtime.rs"),
        &vars,
    )?;
    render_subtree("assets", &target_dir.join("assets"), &HashMap::new())?;

    if config.has_desktop() {
        render_file(
            "desktop/Cargo.toml.template",
            &target_dir.join("crates/desktop/Cargo.toml"),
            &vars,
        )?;
        render_file(
            "desktop/src/main.rs",
            &target_dir.join("crates/desktop/src/main.rs"),
            &vars,
        )?;
    }

    if config.has_ios() {
        render_subtree("ios", &target_dir.join("mobile/ios"), &vars)?;
    }

    if config.has_android() {
        render_subtree("android", &target_dir.join("mobile/android"), &vars)?;
        // Ship the tested renderer with the generated project so builds never
        // depend on edits to a developer's Cargo registry cache. Do not substitute
        // application placeholders in third-party source code.
        render_subtree(
            "android-compat",
            &target_dir.join("vendor"),
            &HashMap::new(),
        )?;
        // `RUST_FONTCONFIG_DLOPEN` is required by GPUI's text stack on Android.
        render_file(
            "cargo-config.toml",
            &target_dir.join("mobile/android/.cargo/config.toml"),
            &vars,
        )?;
    }

    Ok(())
}

/// Add generated platform files without replacing application customizations.
/// Validate the entire update before writing any project files.
pub fn add_platforms(
    target_dir: &Path,
    before: &ProjectConfig,
    after: &ProjectConfig,
) -> Result<()> {
    let scratch = tempfile::tempdir().context("cannot prepare platform update")?;
    let previous = scratch.path().join("previous");
    let updated = scratch.path().join("updated");
    scaffold_files(&previous, before)?;
    scaffold_files(&updated, after)?;

    let mut files = Vec::new();
    collect_files(&updated, &mut files)?;
    files.sort();
    let mut writes = Vec::new();
    let mut base_updates = std::collections::BTreeSet::new();
    for source in files {
        let relative = source.strip_prefix(&updated)?;
        let old_file = previous.join(relative);
        let destination = target_dir.join(relative);
        let desired = fs::read(&source)?;
        let old = if old_file.exists() {
            Some(fs::read(old_file)?)
        } else {
            None
        };
        let current = if destination.exists() {
            Some(
                fs::read(&destination)
                    .with_context(|| format!("cannot read existing '{}'", relative.display()))?,
            )
        } else {
            None
        };
        let legacy_gitignore = slash_path(relative) == ".gitignore"
            && current.as_deref().is_some_and(is_legacy_gitignore);
        if old.as_ref() == Some(&desired) && !legacy_gitignore {
            continue;
        }
        let mut parent = target_dir.to_path_buf();
        for component in relative.components() {
            parent.push(component);
            if parent.is_symlink() {
                bail!(
                    "Cannot update '{}': it is inside a symbolic link.",
                    relative.display()
                );
            }
        }
        if current.as_ref() == Some(&desired) {
            continue;
        }
        if current != old && !legacy_gitignore {
            bail!(
                "Adding platforms would overwrite changes in '{}'. \
                 No project files were changed. Merge the platform files manually.",
                relative.display()
            );
        }
        base_updates.insert(slash_path(relative));
        writes.push((source, destination));
    }

    for (source, destination) in writes {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, &destination)?;
        if is_executable_script(&destination) {
            set_executable(&destination)?;
        }
    }

    let previous_manifest = build_template_manifest(&previous, before)?;
    let updated_manifest = build_template_manifest(&updated, after)?;
    let existing_manifest = TemplateManifest::read(target_dir)?;
    if let Some(existing) = &existing_manifest
        && (existing.template_version != TEMPLATE_VERSION
            || existing.baseline.content_id != updated_manifest.baseline.content_id)
    {
        bail!(
            "baseline_unavailable: cannot add platforms to template '{}'",
            existing.template_version
        );
    }

    let mut manifest = existing_manifest.unwrap_or(previous_manifest);
    manifest.generator = updated_manifest.generator.clone();
    manifest.template_version = updated_manifest.template_version.clone();
    manifest.platforms = updated_manifest.platforms.clone();
    manifest.dependencies = updated_manifest.dependencies.clone();
    manifest.baseline = updated_manifest.baseline.clone();
    manifest.groups = updated_manifest.groups.clone();
    for desired in &updated_manifest.files {
        if let Some(current) = manifest
            .files
            .iter_mut()
            .find(|entry| entry.path == desired.path)
        {
            if base_updates.contains(&desired.path) {
                *current = desired.clone();
            }
        } else {
            manifest.files.push(desired.clone());
        }
    }
    manifest
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    manifest.write(target_dir)?;
    Ok(())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), out)?;
        } else {
            out.push(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(targets: Vec<Platform>) -> ProjectConfig {
        ProjectConfig {
            name: "delta-mobile".into(),
            title: "Delta Mobile".into(),
            bundle_id: "app.kingsword09.delta".into(),
            ui_framework: UiFramework::GpuiKit,
            targets,
        }
    }

    /// Every `{{PLACEHOLDER}}` used in a template must be substituted, otherwise
    /// it silently leaks into generated files (e.g. a literal `{{LIB_NAME}}.a`
    /// reaching the Xcode project).
    #[test]
    fn all_placeholders_are_defined() {
        let vars = vars_for(&config(vec![
            Platform::MacOs,
            Platform::IOs,
            Platform::Android,
        ]));
        let known: std::collections::HashSet<&str> = vars.keys().copied().collect();

        let mut seen = std::collections::BTreeSet::new();
        collect_placeholders(&TEMPLATES, &mut seen);

        let undefined: Vec<&String> = seen
            .iter()
            .filter(|p| !known.contains(p.as_str()))
            .collect();
        assert!(
            undefined.is_empty(),
            "templates use placeholders with no variable: {undefined:?}"
        );
    }

    #[test]
    fn scaffold_writes_trackable_template_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(vec![Platform::MacOs, Platform::Android]);
        scaffold(dir.path(), &config).unwrap();

        let manifest = TemplateManifest::read(dir.path()).unwrap().unwrap();
        assert_eq!(
            manifest.schema_version,
            crate::template_manifest::SCHEMA_VERSION
        );
        assert_eq!(manifest.platforms, vec!["android", "macos"]);
        assert!(manifest.file("crates/app/src/lib.rs").is_some());
        assert!(
            manifest
                .file("vendor/gpui-pre-wgpu-0.3.5/src/shaders.wgsl")
                .is_some()
        );

        let baseline =
            read_template_baseline(&config, &manifest, Path::new("crates/app/src/lib.rs")).unwrap();
        assert_eq!(
            baseline,
            fs::read(dir.path().join("crates/app/src/lib.rs")).unwrap()
        );

        let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".gpui/*"));
        assert!(gitignore.contains("!.gpui/template-manifest.json"));
        let manifest_text = fs::read_to_string(TemplateManifest::path(dir.path())).unwrap();
        assert!(!manifest_text.contains("dev-token"));

        let app_cargo = fs::read_to_string(dir.path().join("crates/app/Cargo.toml")).unwrap();
        assert!(app_cargo.contains("gpui-dev = []"));
        assert!(app_cargo.contains("gpui-profile = []"));
        let workspace_cargo = fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert!(
            workspace_cargo
                .contains("version = \"=0.6.1\", git = \"https://github.com/longbridge/gpui-kit\"")
        );
        assert!(workspace_cargo.contains(
            "version = \"=0.1.0\", git = \"https://github.com/longbridge/gpui-mobile.git\""
        ));
        let desktop_cargo =
            fs::read_to_string(dir.path().join("crates/desktop/Cargo.toml")).unwrap();
        assert!(desktop_cargo.contains("version = \"0.1.0\", path = \"../app\""));
        assert!(desktop_cargo.contains("gpui-dev = [\"delta-mobile-app/gpui-dev\"]"));
        assert!(desktop_cargo.contains("gpui-profile = [\"delta-mobile-app/gpui-profile\"]"));
    }

    #[test]
    fn adding_platform_updates_manifest_groups() {
        let dir = tempfile::tempdir().unwrap();
        let before = config(vec![Platform::MacOs]);
        let after = config(vec![Platform::MacOs, Platform::Android]);
        scaffold(dir.path(), &before).unwrap();

        add_platforms(dir.path(), &before, &after).unwrap();

        let manifest = TemplateManifest::read(dir.path()).unwrap().unwrap();
        assert_eq!(manifest.platforms, vec!["android", "macos"]);
        let renderer = manifest
            .file("vendor/gpui-pre-wgpu-0.3.5/src/shaders.wgsl")
            .unwrap();
        assert_eq!(renderer.group, "android-renderer");
        assert!(dir.path().join("mobile/android/gradle/gradlew").exists());
    }

    #[test]
    fn customized_shared_file_is_not_registered_as_a_new_base() {
        let dir = tempfile::tempdir().unwrap();
        let before = config(vec![Platform::MacOs]);
        let after = config(vec![Platform::MacOs, Platform::Android]);
        scaffold(dir.path(), &before).unwrap();
        let manifest_before = TemplateManifest::read(dir.path()).unwrap().unwrap();
        let view = dir.path().join("crates/app/src/lib.rs");
        fs::write(&view, "// user customization\n").unwrap();

        let error = add_platforms(dir.path(), &before, &after).unwrap_err();
        assert!(error.to_string().contains("lib.rs"));
        assert_eq!(
            TemplateManifest::read(dir.path()).unwrap().unwrap(),
            manifest_before
        );
        assert_eq!(fs::read_to_string(view).unwrap(), "// user customization\n");
        assert!(!dir.path().join("mobile/android").exists());
    }

    #[test]
    fn adding_platform_migrates_legacy_gitignore_before_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let before = config(vec![Platform::MacOs]);
        let after = config(vec![Platform::MacOs, Platform::Android]);
        scaffold(dir.path(), &before).unwrap();
        fs::remove_file(TemplateManifest::path(dir.path())).unwrap();

        let escaped_tick = format!("{}{}", '\\', '\u{60}');
        let old_gitignore = LEGACY_GITIGNORE.replace(&escaped_tick, "\u{60}");
        fs::write(dir.path().join(".gitignore"), old_gitignore).unwrap();

        add_platforms(dir.path(), &before, &after).unwrap();

        let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains("!.gpui/template-manifest.json"));
        assert!(TemplateManifest::path(dir.path()).is_file());
    }

    fn collect_placeholders(dir: &Dir<'_>, out: &mut std::collections::BTreeSet<String>) {
        for file in dir.files() {
            if is_binary_asset(file.path()) {
                continue;
            }
            let Ok(text) = std::str::from_utf8(file.contents()) else {
                continue;
            };
            for (start, _) in text.match_indices("{{") {
                let rest = &text[start..];
                if let Some(end) = rest.find("}}") {
                    let name = &rest[2..end];
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                    {
                        out.insert(name.to_string());
                    }
                }
            }
        }
        for sub in dir.dirs() {
            collect_placeholders(sub, out);
        }
    }

    #[test]
    fn sanitized_names_are_valid() {
        let c = config(vec![Platform::MacOs]);
        assert_eq!(c.crate_ident(), "delta_mobile");
        assert_eq!(c.app_crate(), "delta-mobile-app");
        assert_eq!(c.app_lib_name(), "delta_mobile_app");
        assert_eq!(c.xcode_target(), "DeltaMobile");
        assert_eq!(c.bundle_prefix(), "app.kingsword09");
    }

    #[test]
    fn scaffold_mobile_only_omits_desktop() {
        let dir = std::env::temp_dir().join(format!("gpui-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        scaffold(&dir, &config(vec![Platform::IOs, Platform::Android])).unwrap();

        assert!(dir.join("crates/app/src/lib.rs").exists());
        assert!(dir.join("mobile/ios/project.yml").exists());
        assert!(
            dir.join("mobile/android/gradle/app/build.gradle.kts")
                .exists()
        );
        assert!(
            !dir.join("crates/desktop").exists(),
            "desktop must be absent"
        );

        let workspace = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(!workspace.contains("crates/desktop"));

        // No placeholder may survive into generated output.
        let lib = fs::read_to_string(dir.join("crates/app/src/lib.rs")).unwrap();
        assert!(!lib.contains("{{"), "unsubstituted placeholder in lib.rs");
        let android =
            fs::read_to_string(dir.join("mobile/android/gradle/app/build.gradle.kts")).unwrap();
        assert!(
            android.contains("manifestPlaceholders[\"nativeLibraryName\"] = \"delta_mobile_app\"")
        );
        assert!(!android.contains("{{"));

        // The workspace override must point to an actual, version-matched crate
        // shipped with the generated project, including its source and license.
        let renderer_path = format!("vendor/gpui-pre-wgpu-{GPUI_PRE_VERSION}");
        assert!(workspace.contains(&format!("gpui-pre-wgpu = {{ path = \"{renderer_path}\" }}")));
        let renderer = dir.join(&renderer_path);
        let manifest = fs::read_to_string(renderer.join("Cargo.toml")).unwrap();
        assert!(manifest.contains(&format!("version = \"{GPUI_PRE_VERSION}\"")));
        assert!(renderer.join("LICENSE-APACHE").exists());
        assert!(renderer.join("src/gpui_wgpu.rs").exists());
        let shader = fs::read(renderer.join("src/shaders.wgsl")).unwrap();
        let embedded_shader = TEMPLATES
            .get_file(format!(
                "android-compat/gpui-pre-wgpu-{GPUI_PRE_VERSION}/src/shaders.wgsl"
            ))
            .unwrap();
        assert_eq!(shader, embedded_shader.contents());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scaffold_desktop_only_omits_mobile() {
        let dir = std::env::temp_dir().join(format!("gpui-test-d-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        scaffold(&dir, &config(vec![Platform::MacOs, Platform::Windows])).unwrap();

        assert!(dir.join("crates/desktop/src/main.rs").exists());
        assert!(!dir.join("mobile").exists(), "mobile hosts must be absent");
        assert!(
            !dir.join("vendor").exists(),
            "Android workaround must be absent"
        );

        let workspace = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(workspace.contains("crates/desktop"));
        assert!(!workspace.contains("[patch.crates-io]"));
        // gpui-mobile must not be pulled in for desktop-only projects.
        let app = fs::read_to_string(dir.join("crates/app/Cargo.toml")).unwrap();
        assert!(!app.contains("gpui-mobile"));

        let _ = fs::remove_dir_all(&dir);
    }

    /// On Android 13+ the system emoji font is COLR v1, which swash cannot
    /// render, so gpui-mobile falls back to a bundled CBDT font from the APK
    /// assets. Without it, emoji silently stop rendering.
    #[test]
    fn android_ships_bundled_emoji_font() {
        let dir = std::env::temp_dir().join(format!("gpui-test-e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        scaffold(&dir, &config(vec![Platform::Android])).unwrap();

        let font = dir.join("mobile/android/gradle/app/src/main/assets/fonts/NotoColorEmoji.ttf");
        assert!(font.exists(), "bundled emoji font must be generated");
        // Binary assets are copied verbatim, so compare against the template.
        let copied = fs::read(&font).unwrap();
        let original = TEMPLATES
            .get_file("android/gradle/app/src/main/assets/fonts/NotoColorEmoji.ttf")
            .unwrap()
            .contents();
        assert_eq!(copied, original, "emoji font must be byte-identical");

        let _ = fs::remove_dir_all(&dir);
    }

    /// `gradlew` is invoked directly by `gpui build android`, so the generated
    /// copy must be executable even though `include_dir` drops the mode bit.
    #[cfg(unix)]
    #[test]
    fn gradlew_is_executable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("gpui-test-x-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        scaffold(&dir, &config(vec![Platform::Android])).unwrap();

        let gradlew = dir.join("mobile/android/gradle/gradlew");
        assert!(gradlew.exists());
        let mode = fs::metadata(&gradlew).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "gradlew must be executable");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn platform_parsing_covers_aliases() {
        assert_eq!(Platform::parse("macos"), Some(Platform::MacOs));
        assert_eq!(Platform::parse("mac"), Some(Platform::MacOs));
        assert_eq!(Platform::parse("IOS"), Some(Platform::IOs));
        assert_eq!(Platform::parse("android"), Some(Platform::Android));
        assert_eq!(Platform::parse("beos"), None);
    }

    #[test]
    fn adding_platform_preserves_unrelated_customizations() {
        let dir = tempfile::tempdir().unwrap();
        let before = config(vec![Platform::MacOs]);
        scaffold(dir.path(), &before).unwrap();
        let desktop = dir.path().join("crates/desktop/src/main.rs");
        fs::write(&desktop, "// application's customized desktop entry\n").unwrap();

        add_platforms(
            dir.path(),
            &before,
            &config(vec![Platform::MacOs, Platform::Android]),
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(desktop).unwrap(),
            "// application's customized desktop entry\n"
        );
        assert!(dir.path().join("mobile/android/gradle/gradlew").exists());
        assert!(
            fs::read_to_string(dir.path().join("gpui.toml"))
                .unwrap()
                .contains("android")
        );
    }

    #[test]
    fn adding_platform_refuses_conflicts_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let before = config(vec![Platform::MacOs]);
        scaffold(dir.path(), &before).unwrap();
        let manifest = fs::read(dir.path().join("Cargo.toml")).unwrap();
        let metadata = fs::read(dir.path().join("gpui.toml")).unwrap();
        let view = dir.path().join("crates/app/src/lib.rs");
        fs::write(&view, "// application's customized view\n").unwrap();

        let error = add_platforms(
            dir.path(),
            &before,
            &config(vec![Platform::MacOs, Platform::Android]),
        )
        .unwrap_err();

        assert!(error.to_string().contains("lib.rs"));
        assert_eq!(fs::read(dir.path().join("Cargo.toml")).unwrap(), manifest);
        assert_eq!(fs::read(dir.path().join("gpui.toml")).unwrap(), metadata);
        assert_eq!(
            fs::read_to_string(view).unwrap(),
            "// application's customized view\n"
        );
        assert!(!dir.path().join("mobile").exists());
    }

    #[test]
    fn titles_round_trip_through_metadata_rust_and_native_xml() {
        for title in [
            r#"R&D "Desk" \path {{APP_CRATE}} <工具> ' % @test"#,
            "first\n[run]\nios_simulator = \"not-a-device\"\nlast",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut before = config(vec![Platform::MacOs]);
            before.title = title.into();
            scaffold(dir.path(), &before).unwrap();
            let mut after = before.clone();
            after.targets.extend([Platform::IOs, Platform::Android]);
            add_platforms(dir.path(), &before, &after).unwrap();
            let project = crate::commands::run::Project::load(Some(dir.path().into())).unwrap();
            assert_eq!(project.title, title);
            assert!(project.defaults.ios_simulator.is_none());
            for file in ["crates/app/src/lib.rs", "crates/desktop/src/main.rs"] {
                let source = fs::read_to_string(dir.path().join(file)).unwrap();
                syn::parse_file(&source).unwrap_or_else(|error| panic!("{file}: {error}"));
            }
            for file in [
                "mobile/ios/Info.plist",
                "mobile/ios/LaunchScreen.storyboard",
                "mobile/android/gradle/app/src/main/AndroidManifest.xml",
                "mobile/android/gradle/app/src/main/res/values/strings.xml",
            ] {
                let text = fs::read_to_string(dir.path().join(file)).unwrap();
                let doc = roxmltree::Document::parse_with_options(
                    &text,
                    roxmltree::ParsingOptions {
                        allow_dtd: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                if file.ends_with("Info.plist") {
                    assert!(
                        doc.descendants()
                            .any(|node| node.has_tag_name("string") && node.text() == Some(title))
                    );
                }
                if file.ends_with("storyboard") {
                    assert!(
                        doc.descendants()
                            .any(|node| node.attribute("text") == Some(title))
                    );
                }
            }
            let source = fs::read_to_string(dir.path().join("crates/app/src/lib.rs")).unwrap();
            assert!(
                source.contains(&format!(".child({title:?})")),
                "title was expanded as another template"
            );
        }
    }

    #[test]
    fn invalid_identifiers_fail_before_writing_project_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(vec![Platform::Android]);
        config.bundle_id = "com.example.bad\"id".into();
        assert!(scaffold(dir.path(), &config).is_err());
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }
}
