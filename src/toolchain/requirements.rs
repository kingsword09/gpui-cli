//! Pure target requirement rules. Probing is deliberately kept separate.

use super::Target;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    Command,
    Directory,
    Environment,
    HostPlatform,
    RustTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }

    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandSuggestion {
    pub argv: Vec<String>,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Requirement {
    pub id: String,
    pub target: Target,
    pub kind: RequirementKind,
    pub label: String,
    pub required: bool,
    pub expected: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandSpec>,
    pub remediation: Vec<CommandSuggestion>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Context {
    pub host_os: String,
    #[serde(default)]
    pub android_abis: Vec<String>,
    #[serde(default)]
    pub project_agp: Option<String>,
    #[serde(default)]
    pub project_targets: Vec<Target>,
}

impl Context {
    pub fn for_host(host_os: impl Into<String>) -> Self {
        Self {
            host_os: host_os.into(),
            android_abis: vec!["arm64-v8a".into()],
            ..Self::default()
        }
    }
}

pub fn requirements_for(context: &Context, target: Target) -> Result<Vec<Requirement>> {
    let mut requirements = rust_requirements(target);
    match target {
        Target::Desktop => requirements.extend(desktop_requirements(context)),
        Target::Ios => requirements.extend(ios_requirements(context)),
        Target::Android => requirements.extend(android_requirements(context)?),
    }
    Ok(requirements)
}

fn rust_requirements(target: Target) -> Vec<Requirement> {
    vec![
        command_requirement(
            "rust.rustc",
            target,
            "rustc",
            CommandSpec::new("rustc", &["-vV"]),
            true,
            json!({"executable": true, "version": "parseable"}),
            vec![],
        ),
        command_requirement(
            "rust.cargo",
            target,
            "cargo",
            CommandSpec::new("cargo", &["-V"]),
            true,
            json!({"executable": true, "version": "parseable"}),
            vec![],
        ),
        command_requirement(
            "rust.rustup",
            target,
            "rustup",
            CommandSpec::new("rustup", &["--version"]),
            true,
            json!({"executable": true, "version": "parseable"}),
            vec![],
        ),
    ]
}

fn desktop_requirements(context: &Context) -> Vec<Requirement> {
    vec![
        Requirement {
            id: "desktop.host_platform".into(),
            target: Target::Desktop,
            kind: RequirementKind::HostPlatform,
            label: "desktop host platform".into(),
            required: true,
            expected: json!(["macos", "windows", "linux"]),
            command: None,
            remediation: vec![],
        },
        command_requirement(
            "desktop.c_compiler",
            Target::Desktop,
            "C compiler",
            CommandSpec::new("cc", &["--version"]),
            true,
            json!({"executable": true, "version": "reported"}),
            vec![],
        ),
        optional_command(
            "capture.xcode_gpu_tools",
            Target::Desktop,
            "optional GPU capture tools",
            CommandSpec::new("xcrun", &["--find", "metal"]),
            context.host_os == "macos",
            "Optional; absence must not block desktop builds.",
        ),
    ]
}

fn ios_requirements(context: &Context) -> Vec<Requirement> {
    vec![
        Requirement {
            id: "ios.host_platform".into(),
            target: Target::Ios,
            kind: RequirementKind::HostPlatform,
            label: "iOS build host".into(),
            required: true,
            expected: json!("macos"),
            command: None,
            remediation: vec![],
        },
        command_requirement(
            "ios.xcodebuild",
            Target::Ios,
            "xcodebuild",
            CommandSpec::new("xcodebuild", &["-version"]),
            true,
            json!({"executable": true, "version": "reported"}),
            vec![],
        ),
        command_requirement(
            "ios.xcodegen",
            Target::Ios,
            "xcodegen",
            CommandSpec::new("xcodegen", &["--version"]),
            true,
            json!({"executable": true, "version": "reported"}),
            vec![suggestion(
                &["brew", "install", "xcodegen"],
                "Install XcodeGen before building an iOS host.",
            )],
        ),
        command_requirement(
            "ios.xcrun",
            Target::Ios,
            "xcrun",
            CommandSpec::new("xcrun", &["--version"]),
            true,
            json!({"executable": true, "version": "reported"}),
            vec![],
        ),
        command_requirement(
            "ios.simctl",
            Target::Ios,
            "xcrun simctl",
            CommandSpec::new("xcrun", &["simctl", "help"]),
            true,
            json!({"subcommand": "simctl", "usable": true}),
            vec![],
        ),
        rust_target_requirement("ios.rust_target.simulator", "aarch64-apple-ios-sim", true),
        rust_target_requirement("ios.rust_target.device", "aarch64-apple-ios", true),
        optional_command(
            "ios.devicectl",
            Target::Ios,
            "xcrun devicectl",
            CommandSpec::new("xcrun", &["devicectl", "help"]),
            context.host_os == "macos",
            "Only needed for physical-device deployment; simulator builds remain diagnosable.",
        ),
    ]
}

fn android_requirements(context: &Context) -> Result<Vec<Requirement>> {
    let abis = if context.android_abis.is_empty() {
        vec!["arm64-v8a".to_string()]
    } else {
        context.android_abis.clone()
    };
    let mut requirements = vec![
        command_requirement(
            "android.cargo_ndk",
            Target::Android,
            "cargo-ndk",
            CommandSpec::new("cargo-ndk", &["--version"]),
            true,
            json!({"executable": true, "version": "reported"}),
            vec![suggestion(
                &["cargo", "install", "cargo-ndk"],
                "Install cargo-ndk without changing the project automatically.",
            )],
        ),
        environment_requirement(
            "android.sdk",
            "Android SDK",
            &["ANDROID_HOME", "ANDROID_SDK_ROOT"],
            true,
            json!({"directory": true, "platforms": "project-selected"}),
        ),
        environment_requirement(
            "android.ndk",
            "Android NDK",
            &["ANDROID_NDK_HOME", "NDK_HOME"],
            true,
            json!({"directory": true, "version": "project-selected"}),
        ),
        command_requirement(
            "android.adb",
            Target::Android,
            "adb",
            CommandSpec::new("adb", &["version"]),
            true,
            json!({"executable": true, "device": "selected-or-reported"}),
            vec![],
        ),
        command_requirement(
            "android.java",
            Target::Android,
            "java",
            CommandSpec::new("java", &["-version"]),
            true,
            json!({"executable": true, "version": "AGP-compatible"}),
            vec![],
        ),
        command_requirement(
            "android.gradle",
            Target::Android,
            "Gradle wrapper",
            CommandSpec::new("./gradlew", &["--version"]),
            true,
            json!({"wrapper": true, "version": "project-selected"}),
            vec![],
        ),
    ];
    if let Some(agp) = &context.project_agp {
        requirements.push(Requirement {
            id: "android.agp".into(),
            target: Target::Android,
            kind: RequirementKind::Command,
            label: "Android Gradle Plugin".into(),
            required: true,
            expected: json!({"version": agp}),
            command: None,
            remediation: vec![],
        });
    }
    for abi in abis {
        requirements.push(rust_target_requirement(
            &format!("android.rust_target.{abi}"),
            android_rust_target(&abi)?,
            true,
        ));
    }
    Ok(requirements)
}

fn android_rust_target(abi: &str) -> Result<&'static str> {
    match abi {
        "arm64-v8a" => Ok("aarch64-linux-android"),
        "armeabi-v7a" => Ok("armv7-linux-androideabi"),
        "x86" => Ok("i686-linux-android"),
        "x86_64" => Ok("x86_64-linux-android"),
        _ => bail!("unknown Android ABI '{abi}'"),
    }
}

fn command_requirement(
    id: &str,
    target: Target,
    label: &str,
    command: CommandSpec,
    required: bool,
    expected: serde_json::Value,
    remediation: Vec<CommandSuggestion>,
) -> Requirement {
    Requirement {
        id: id.into(),
        target,
        kind: RequirementKind::Command,
        label: label.into(),
        required,
        expected,
        command: Some(command),
        remediation,
    }
}

fn optional_command(
    id: &str,
    target: Target,
    label: &str,
    command: CommandSpec,
    enabled: bool,
    description: &str,
) -> Requirement {
    command_requirement(
        id,
        target,
        label,
        command,
        false,
        json!({"optional": true, "enabled_on_host": enabled}),
        vec![suggestion(&["xcrun", "--find", "metal"], description)],
    )
}

fn environment_requirement(
    id: &str,
    label: &str,
    names: &[&str],
    required: bool,
    expected: serde_json::Value,
) -> Requirement {
    Requirement {
        id: id.into(),
        target: Target::Android,
        kind: RequirementKind::Environment,
        label: label.into(),
        required,
        expected: json!({"variables": names, "requirements": expected}),
        command: None,
        remediation: vec![],
    }
}

fn rust_target_requirement(id: &str, target_name: &str, required: bool) -> Requirement {
    Requirement {
        id: id.into(),
        target: if target_name.starts_with("aarch64-apple") {
            Target::Ios
        } else {
            Target::Android
        },
        kind: RequirementKind::RustTarget,
        label: format!("Rust target {target_name}"),
        required,
        expected: json!({"target": target_name, "installed": true}),
        command: Some(CommandSpec::new(
            "rustup",
            &["target", "list", "--installed"],
        )),
        remediation: vec![suggestion(
            &["rustup", "target", "add", target_name],
            "Install the selected Rust target explicitly.",
        )],
    }
}

fn suggestion(argv: &[&str], description: &str) -> CommandSuggestion {
    CommandSuggestion {
        argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
        description: description.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_rules_do_not_require_mobile_tools() {
        let requirements = requirements_for(&Context::for_host("linux"), Target::Desktop).unwrap();
        assert!(
            requirements
                .iter()
                .any(|item| item.id == "desktop.c_compiler")
        );
        assert!(
            !requirements
                .iter()
                .any(|item| item.id.starts_with("android."))
        );
        assert!(!requirements.iter().any(|item| item.id.starts_with("ios.")));
    }

    #[test]
    fn android_rules_follow_selected_abis_and_agp() {
        let mut context = Context::for_host("linux");
        context.android_abis = vec!["arm64-v8a".into(), "x86_64".into()];
        context.project_agp = Some("8.9.0".into());
        let requirements = requirements_for(&context, Target::Android).unwrap();
        assert!(requirements.iter().any(|item| item.id == "android.agp"));
        assert!(
            requirements
                .iter()
                .any(|item| item.label == "Rust target x86_64-linux-android")
        );
        assert_eq!(
            requirements
                .iter()
                .filter(|item| item.kind == RequirementKind::RustTarget)
                .count(),
            2
        );
    }

    #[test]
    fn unknown_abi_fails_before_any_probe_can_run() {
        let mut context = Context::for_host("linux");
        context.android_abis = vec!["mips".into()];
        let error = requirements_for(&context, Target::Android).unwrap_err();
        assert!(error.to_string().contains("unknown Android ABI"));
    }

    #[test]
    fn suggestions_are_argument_vectors() {
        let requirements = requirements_for(&Context::for_host("macos"), Target::Ios).unwrap();
        let xcodegen = requirements
            .iter()
            .find(|item| item.id == "ios.xcodegen")
            .unwrap();
        assert_eq!(
            xcodegen.remediation[0].argv,
            ["brew", "install", "xcodegen"]
        );
    }
}
