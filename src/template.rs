use anyhow::{bail, Context, Result};
use include_dir::{include_dir, Dir};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Full built-in template tree, embedded at compile time.
static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

/// GPUI is published as a family of `gpui-pre-*` crates that must all share one
/// version. These constants centralise the pins the generated projects use.
pub const GPUI_PRE_VERSION: &str = "0.3.5";
pub const GPUI_KIT_GIT: &str = "https://github.com/longbridge/gpui-kit";
/// `gpui-kit` needs a git revision rather than the published crate: on
/// iOS/Android it must not pull in the desktop-only `gpui-pre-platform` crate.
pub const GPUI_KIT_REV: &str = "9504b4658d57c59024a664f22b5ab55e070a24b4";
pub const GPUI_MOBILE_GIT: &str = "https://github.com/longbridge/gpui-mobile.git";
/// Mobile platform revision validated with the pinned GPUI family and renderer patch.
pub const GPUI_MOBILE_REV: &str = "b4e3ab258f271003b7d4b874f7c5ebe3a77fac60";
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

#[derive(Clone)]
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
    let mut out = input.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{}}}}}", key), value);
    }
    out
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

    cx.open_window(WindowOptions::default(), |window, cx| {
        let view = cx.new(|_| MainView::new());
        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("failed to open the main window");

    cx.activate(true);
}
"#,
    );

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
    // dev-channel credentials into `gpui_live.txt` before launching.
    crate::init_live(app.internal_data_path().as_deref());

    gpui_mobile::android::jni::install_panic_hook();

    let _platform = gpui_mobile::android::jni::init_platform(&app);
    let Some(shared) = gpui_mobile::android::jni::shared_platform() else {{
        log::error!("shared_platform() returned None - aborting");
        return;
    }};

    Application::with_platform(shared.into_rc()).run(|cx: &mut App| {{
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

/// Writes the project described by `config` into `target_dir`.
pub fn scaffold(target_dir: &Path, config: &ProjectConfig) -> Result<()> {
    if config.targets.is_empty() {
        bail!("at least one target platform is required");
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
    scaffold(&previous, before)?;
    scaffold(&updated, after)?;

    let mut files = Vec::new();
    collect_files(&updated, &mut files)?;
    files.sort();
    let mut writes = Vec::new();
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
        if old.as_ref() == Some(&desired) {
            // This file does not need an update; keep any user customization.
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
        let current = if destination.exists() {
            Some(
                fs::read(&destination)
                    .with_context(|| format!("cannot read existing '{}'", relative.display()))?,
            )
        } else {
            None
        };
        if current.as_ref() == Some(&desired) {
            continue;
        }
        if current != old {
            bail!(
                "Adding platforms would overwrite changes in '{}'. \
                 No project files were changed. Merge the platform files manually.",
                relative.display()
            );
        }
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
        assert!(dir
            .join("mobile/android/gradle/app/build.gradle.kts")
            .exists());
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
        assert!(fs::read_to_string(dir.path().join("gpui.toml"))
            .unwrap()
            .contains("android"));
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
}
