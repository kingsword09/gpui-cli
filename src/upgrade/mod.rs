//! Read-only three-way upgrade planning.
//!
//! T03 deliberately stops at a plan. It never writes a project file, updates
//! a manifest, or acquires an upgrade lock; those are T04 responsibilities.

pub mod apply;
pub mod lock;
pub mod transaction;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Manifest;
use crate::template::{Platform, ProjectConfig, TEMPLATE_VERSION, UiFramework, scaffold};
use crate::template_manifest::{ManifestFile, TemplateManifest};

pub const PLAN_SCHEMA_VERSION: u32 = 1;
const PLAN_ROOT: &str = ".gpui/upgrade/plans";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Ready,
    Conflict,
    ManualMigrationRequired,
    BaselineUnavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BaseResolution {
    Manifest,
    InferredExact,
    ManualMigrationRequired,
    BaselineUnavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    NoChange,
    Replace,
    KeepLocal,
    Add,
    Delete,
    Conflict,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupStatus {
    Unchanged,
    Ready,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionRef {
    pub template_version: String,
    pub content_id: Option<String>,
    pub distribution: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanInput {
    pub kind: String,
    pub path: String,
    pub sha256: Option<String>,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilePlan {
    pub path: String,
    pub group: String,
    pub action: FileAction,
    pub base_sha256: Option<String>,
    pub local_sha256: Option<String>,
    pub target_sha256: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupPlan {
    pub id: String,
    pub atomic: bool,
    pub status: GroupStatus,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolchainChange {
    pub field: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradePlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub status: PlanStatus,
    pub project: String,
    pub base: VersionRef,
    pub target: VersionRef,
    pub base_resolution: BaseResolution,
    pub inputs: Vec<PlanInput>,
    pub files: Vec<FilePlan>,
    pub groups: Vec<GroupPlan>,
    pub toolchain_changes: Vec<ToolchainChange>,
    pub validation_commands: Vec<Vec<String>>,
    pub notes: Vec<String>,
}

pub fn save_plan(root: &Path, plan: &UpgradePlan) -> Result<PathBuf> {
    validate_plan_id(&plan.plan_id)?;
    let path = plan_path(root, &plan.plan_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(plan)?;
    bytes.push(b'\n');
    fs::write(&path, bytes)?;
    Ok(path)
}

pub fn load_plan(root: &Path, plan_id: &str) -> Result<UpgradePlan> {
    let path = plan_path(root, plan_id)?;
    let bytes =
        fs::read(&path).with_context(|| format!("reading upgrade plan '{}'", path.display()))?;
    let plan: UpgradePlan = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid upgrade plan '{}'", path.display()))?;
    if plan.plan_id != plan_id {
        bail!("upgrade plan id does not match its stored content");
    }
    Ok(plan)
}

fn plan_path(root: &Path, plan_id: &str) -> Result<PathBuf> {
    let hex = plan_id
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context("invalid upgrade plan id")?;
    Ok(root.join(PLAN_ROOT).join(format!("{hex}.json")))
}

fn validate_plan_id(plan_id: &str) -> Result<()> {
    let _ = plan_path(Path::new("."), plan_id)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedProject {
    pub(crate) root: PathBuf,
    pub(crate) config: ProjectConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    Manifest,
    InferredExact,
    ManualMigrationRequired,
}

pub fn plan_project(root: &Path, target_version: &str) -> Result<UpgradePlan> {
    let project = load_project(root)?;
    let target_ref = VersionRef {
        template_version: target_version.to_owned(),
        content_id: None,
        distribution: None,
    };

    if target_version != TEMPLATE_VERSION {
        return Ok(finalize_plan(UpgradePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            status: PlanStatus::BaselineUnavailable,
            project: project.config.name.clone(),
            base: VersionRef {
                template_version: "unknown".to_owned(),
                content_id: None,
                distribution: None,
            },
            target: target_ref,
            base_resolution: BaseResolution::BaselineUnavailable,
            inputs: vec![],
            files: vec![],
            groups: vec![],
            toolchain_changes: vec![],
            validation_commands: validation_commands(),
            notes: vec![format!(
                "baseline_unavailable: target template '{}' is not embedded in this CLI",
                target_version
            )],
        }));
    }

    let target_root = tempfile::tempdir().context("preparing embedded target template")?;
    scaffold(target_root.path(), &project.config)?;
    let target_manifest = TemplateManifest::read(target_root.path())?
        .context("embedded target did not produce a template manifest")?;

    let (base_manifest, resolution, mut notes) = resolve_base(&project, &target_manifest)?;
    if resolution == Resolution::ManualMigrationRequired {
        return Ok(finalize_plan(UpgradePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            status: PlanStatus::ManualMigrationRequired,
            project: project.config.name.clone(),
            base: version_ref(None),
            target: version_ref(Some(&target_manifest)),
            base_resolution: BaseResolution::ManualMigrationRequired,
            inputs: local_inputs(&project.root, &target_manifest)?,
            files: vec![],
            groups: vec![],
            toolchain_changes: vec![],
            validation_commands: validation_commands(),
            notes,
        }));
    }

    let base_manifest = base_manifest.expect("resolved base must have a manifest");
    if let Err(error) = validate_baseline(&base_manifest, &target_manifest) {
        notes.push(format!("baseline_unavailable: {error:#}"));
        return Ok(finalize_plan(UpgradePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            status: PlanStatus::BaselineUnavailable,
            project: project.config.name.clone(),
            base: version_ref(Some(&base_manifest)),
            target: version_ref(Some(&target_manifest)),
            base_resolution: BaseResolution::BaselineUnavailable,
            inputs: local_inputs(&project.root, &target_manifest)?,
            files: vec![],
            groups: vec![],
            toolchain_changes: vec![],
            validation_commands: validation_commands(),
            notes,
        }));
    }

    let files = build_file_plans(&project.root, &base_manifest, &target_manifest)?;
    let groups = build_group_plans(&base_manifest, &target_manifest, &files);
    let status = if files.iter().any(|file| file.action == FileAction::Conflict) {
        PlanStatus::Conflict
    } else {
        PlanStatus::Ready
    };
    if resolution == Resolution::InferredExact {
        notes.push(
            "base was inferred from an exact match of the known embedded template; \
             no user-modified file was treated as base"
                .to_owned(),
        );
    }

    Ok(finalize_plan(UpgradePlan {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_id: String::new(),
        status,
        project: project.config.name.clone(),
        base: version_ref(Some(&base_manifest)),
        target: version_ref(Some(&target_manifest)),
        base_resolution: match resolution {
            Resolution::Manifest => BaseResolution::Manifest,
            Resolution::InferredExact => BaseResolution::InferredExact,
            Resolution::ManualMigrationRequired => BaseResolution::ManualMigrationRequired,
        },
        inputs: local_inputs(&project.root, &target_manifest)?,
        files,
        groups,
        toolchain_changes: toolchain_changes(&base_manifest, &target_manifest),
        validation_commands: validation_commands(),
        notes,
    }))
}

pub(crate) fn load_project(root: &Path) -> Result<ResolvedProject> {
    if !root.join("Cargo.toml").is_file() || !root.join("gpui.toml").is_file() {
        bail!(
            "upgrade plan must run at a generated project root containing Cargo.toml and gpui.toml"
        );
    }
    let text = fs::read_to_string(root.join("gpui.toml"))?;
    let manifest: Manifest = Manifest::parse(&text)?;
    let targets = manifest
        .app
        .targets
        .iter()
        .map(|target| {
            Platform::parse(target)
                .with_context(|| format!("unknown target '{target}' in gpui.toml"))
        })
        .collect::<Result<Vec<_>>>()?;
    if targets.is_empty() {
        bail!("gpui.toml has no target platforms; cannot resolve a template baseline");
    }
    Ok(ResolvedProject {
        root: root.to_path_buf(),
        config: ProjectConfig {
            name: manifest.app.name.clone(),
            title: manifest
                .app
                .title
                .unwrap_or_else(|| manifest.app.name.clone()),
            bundle_id: manifest
                .app
                .bundle_id
                .unwrap_or_else(|| format!("com.example.{}", manifest.app.name.replace('-', ""))),
            ui_framework: UiFramework::GpuiKit,
            targets,
        },
    })
}

fn resolve_base(
    project: &ResolvedProject,
    target_manifest: &TemplateManifest,
) -> Result<(Option<TemplateManifest>, Resolution, Vec<String>)> {
    match TemplateManifest::read(&project.root)? {
        Some(manifest) => Ok((Some(manifest), Resolution::Manifest, vec![])),
        None => {
            let mut mismatches = Vec::new();
            for entry in &target_manifest.files {
                let actual = hash_file(&project.root.join(&entry.path))?;
                if actual.as_deref() != Some(entry.base_sha256.as_str()) {
                    mismatches.push(entry.path.clone());
                }
            }
            if mismatches.is_empty() {
                Ok((
                    Some(target_manifest.clone()),
                    Resolution::InferredExact,
                    vec![
                        "template manifest is absent; all known managed files exactly match \
                         the embedded template"
                            .to_owned(),
                    ],
                ))
            } else {
                Ok((
                    None,
                    Resolution::ManualMigrationRequired,
                    vec![format!(
                        "manual_migration_required: template manifest is absent and \
                         {} managed file(s) do not exactly match the known baseline",
                        mismatches.len()
                    )],
                ))
            }
        }
    }
}

fn validate_baseline(base: &TemplateManifest, target: &TemplateManifest) -> Result<()> {
    if base.template_version != TEMPLATE_VERSION
        || base.baseline.content_id != target.baseline.content_id
    {
        bail!(
            "template '{}' is not available from the current embedded baseline",
            base.template_version
        );
    }
    for entry in &base.files {
        let Some(target_entry) = target.file(&entry.path) else {
            bail!(
                "managed file '{}' is absent from the embedded baseline",
                entry.path
            );
        };
        if target_entry.base_sha256 != entry.base_sha256
            || target_entry.template_path != entry.template_path
        {
            bail!(
                "embedded content for '{}' does not match its manifest hash",
                entry.path
            );
        }
    }
    Ok(())
}

fn build_file_plans(
    root: &Path,
    base: &TemplateManifest,
    target: &TemplateManifest,
) -> Result<Vec<FilePlan>> {
    let mut entries: BTreeMap<String, (Option<&ManifestFile>, Option<&ManifestFile>)> =
        BTreeMap::new();
    for entry in &base.files {
        entries.entry(entry.path.clone()).or_default().0 = Some(entry);
    }
    for entry in &target.files {
        entries.entry(entry.path.clone()).or_default().1 = Some(entry);
    }

    let mut plans = Vec::new();
    for (path, (base_entry, target_entry)) in entries {
        let base_hash = base_entry.map(|entry| entry.base_sha256.clone());
        let target_hash = target_entry.map(|entry| entry.base_sha256.clone());
        let local_hash = hash_file(&root.join(&path))?;
        let group = target_entry
            .or(base_entry)
            .map(|entry| entry.group.clone())
            .unwrap_or_else(|| "unknown".to_owned());
        let (action, reason) = classify(
            base_hash.as_deref(),
            local_hash.as_deref(),
            target_hash.as_deref(),
        );
        plans.push(FilePlan {
            path,
            group,
            action,
            base_sha256: base_hash,
            local_sha256: local_hash,
            target_sha256: target_hash,
            reason: reason.to_owned(),
        });
    }
    Ok(plans)
}

pub fn classify(
    base: Option<&str>,
    local: Option<&str>,
    target: Option<&str>,
) -> (FileAction, &'static str) {
    if local == target {
        return (FileAction::NoChange, "local_equals_target");
    }
    if base.is_none() {
        if local.is_none() && target.is_some() {
            return (FileAction::Add, "new_target_file");
        }
        if local.is_some() && target.is_some() {
            return (FileAction::Conflict, "new_file_collision");
        }
        if local.is_some() && target.is_none() {
            return (FileAction::KeepLocal, "local_only_file");
        }
    }
    if local == base {
        return match target {
            Some(_) => (FileAction::Replace, "local_equals_base_target_changed"),
            None => (FileAction::Delete, "upstream_deleted_local_unchanged"),
        };
    }
    if target == base {
        return (FileAction::KeepLocal, "target_equals_base_local_changed");
    }
    (FileAction::Conflict, "local_and_target_both_changed")
}

fn build_group_plans(
    base: &TemplateManifest,
    target: &TemplateManifest,
    files: &[FilePlan],
) -> Vec<GroupPlan> {
    let conflicting_groups: BTreeSet<String> = files
        .iter()
        .filter(|file| file.action == FileAction::Conflict)
        .map(|file| file.group.clone())
        .collect();
    let changed_groups: BTreeSet<String> = files
        .iter()
        .filter(|file| file.action != FileAction::NoChange)
        .map(|file| file.group.clone())
        .collect();
    let mut groups = BTreeMap::new();
    for group in base.groups.iter().chain(target.groups.iter()) {
        groups
            .entry(group.id.clone())
            .or_insert((group.atomic, BTreeSet::new()));
    }
    for file in files {
        groups
            .entry(file.group.clone())
            .or_insert((true, BTreeSet::new()))
            .1
            .insert(file.path.clone());
    }
    groups
        .into_iter()
        .map(|(id, (atomic, files))| {
            let files: Vec<String> = files.into_iter().collect();
            let status = if conflicting_groups.contains(&id) {
                GroupStatus::Conflict
            } else if changed_groups.contains(&id) {
                GroupStatus::Ready
            } else {
                GroupStatus::Unchanged
            };
            GroupPlan {
                id,
                atomic,
                status,
                files,
            }
        })
        .collect()
}

pub(crate) fn hash_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    if !path.is_file() {
        bail!("managed path '{}' is not a regular file", path.display());
    }
    let bytes = fs::read(path)?;
    Ok(Some(hash_bytes(&bytes)))
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn version_ref(manifest: Option<&TemplateManifest>) -> VersionRef {
    manifest
        .map(|manifest| VersionRef {
            template_version: manifest.template_version.clone(),
            content_id: Some(manifest.baseline.content_id.clone()),
            distribution: Some(manifest.baseline.distribution.clone()),
        })
        .unwrap_or_else(|| VersionRef {
            template_version: "unknown".to_owned(),
            content_id: None,
            distribution: None,
        })
}

fn local_inputs(root: &Path, target: &TemplateManifest) -> Result<Vec<PlanInput>> {
    target
        .files
        .iter()
        .map(|entry| {
            let path = root.join(&entry.path);
            let hash = hash_file(&path)?;
            Ok(PlanInput {
                kind: "local_file".to_owned(),
                path: entry.path.clone(),
                present: hash.is_some(),
                sha256: hash,
            })
        })
        .collect()
}

fn toolchain_changes(base: &TemplateManifest, target: &TemplateManifest) -> Vec<ToolchainChange> {
    let mut changes = Vec::new();
    let fields = [
        (
            "gpui_pre_version",
            &base.dependencies.gpui_pre_version,
            &target.dependencies.gpui_pre_version,
        ),
        (
            "gpui_kit_revision",
            &base.dependencies.gpui_kit_revision,
            &target.dependencies.gpui_kit_revision,
        ),
        (
            "gpui_mobile_revision",
            &base.dependencies.gpui_mobile_revision,
            &target.dependencies.gpui_mobile_revision,
        ),
    ];
    for (field, from, to) in fields {
        if from != to {
            changes.push(ToolchainChange {
                field: field.to_owned(),
                from: from.clone(),
                to: to.clone(),
            });
        }
    }
    changes
}

fn validation_commands() -> Vec<Vec<String>> {
    vec![
        vec!["gpui".into(), "doctor".into(), "--json".into()],
        vec![
            "cargo".into(),
            "check".into(),
            "--workspace".into(),
            "--locked".into(),
        ],
    ]
}

fn finalize_plan(mut plan: UpgradePlan) -> UpgradePlan {
    plan.plan_id.clear();
    let bytes = serde_json::to_vec(&plan).expect("upgrade plan is serializable");
    plan.plan_id = hash_bytes(&bytes);
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{Platform, ProjectConfig, UiFramework, scaffold};

    fn h(value: &str) -> String {
        format!("sha256:{value}")
    }

    #[test]
    fn three_way_rules_cover_replace_keep_delete_and_conflict() {
        assert_eq!(
            classify(Some("b"), Some("b"), Some("n")),
            (FileAction::Replace, "local_equals_base_target_changed")
        );
        assert_eq!(
            classify(Some("b"), Some("l"), Some("b")),
            (FileAction::KeepLocal, "target_equals_base_local_changed")
        );
        assert_eq!(
            classify(Some("b"), Some("b"), None),
            (FileAction::Delete, "upstream_deleted_local_unchanged")
        );
        assert_eq!(
            classify(Some("b"), Some("l"), None),
            (FileAction::Conflict, "local_and_target_both_changed")
        );
        assert_eq!(
            classify(Some("b"), Some("n"), Some("n")),
            (FileAction::NoChange, "local_equals_target")
        );
    }

    #[test]
    fn new_file_and_collision_are_distinct() {
        assert_eq!(
            classify(None, None, Some("n")),
            (FileAction::Add, "new_target_file")
        );
        assert_eq!(
            classify(None, Some("l"), Some("n")),
            (FileAction::Conflict, "new_file_collision")
        );
        assert_eq!(
            classify(None, Some("l"), None),
            (FileAction::KeepLocal, "local_only_file")
        );
    }

    #[test]
    fn plan_id_is_stable_for_same_content() {
        let mut plan = UpgradePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            status: PlanStatus::Ready,
            project: "demo".into(),
            base: VersionRef {
                template_version: TEMPLATE_VERSION.into(),
                content_id: Some(h("b")),
                distribution: Some("embedded-version-package".into()),
            },
            target: VersionRef {
                template_version: TEMPLATE_VERSION.into(),
                content_id: Some(h("n")),
                distribution: Some("embedded-version-package".into()),
            },
            base_resolution: BaseResolution::Manifest,
            inputs: vec![],
            files: vec![],
            groups: vec![],
            toolchain_changes: vec![],
            validation_commands: validation_commands(),
            notes: vec![],
        };
        let first = finalize_plan(plan.clone()).plan_id;
        plan.plan_id = String::new();
        assert_eq!(first, finalize_plan(plan).plan_id);
    }

    #[test]
    fn generated_project_produces_a_ready_plan() {
        let dir = tempfile::tempdir().unwrap();
        let config = ProjectConfig {
            name: "upgrade-fixture".into(),
            title: "Upgrade Fixture".into(),
            bundle_id: "com.example.upgradefixture".into(),
            ui_framework: UiFramework::GpuiKit,
            targets: vec![Platform::MacOs],
        };
        scaffold(dir.path(), &config).unwrap();

        let plan = plan_project(dir.path(), TEMPLATE_VERSION).unwrap();
        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.base_resolution, BaseResolution::Manifest);
        assert!(
            plan.files
                .iter()
                .all(|file| file.action == FileAction::NoChange)
        );
    }

    #[test]
    fn missing_manifest_with_modified_file_requires_manual_migration() {
        let dir = tempfile::tempdir().unwrap();
        let config = ProjectConfig {
            name: "upgrade-fixture".into(),
            title: "Upgrade Fixture".into(),
            bundle_id: "com.example.upgradefixture".into(),
            ui_framework: UiFramework::GpuiKit,
            targets: vec![Platform::MacOs],
        };
        scaffold(dir.path(), &config).unwrap();
        fs::remove_file(TemplateManifest::path(dir.path())).unwrap();
        fs::write(
            dir.path().join("crates/app/src/lib.rs"),
            b"user customization\n",
        )
        .unwrap();

        let plan = plan_project(dir.path(), TEMPLATE_VERSION).unwrap();
        assert_eq!(plan.status, PlanStatus::ManualMigrationRequired);
        assert_eq!(
            plan.base_resolution,
            BaseResolution::ManualMigrationRequired
        );
        assert!(plan.files.is_empty());
    }

    #[test]
    fn missing_manifest_with_exact_project_is_inferred() {
        let dir = tempfile::tempdir().unwrap();
        let config = ProjectConfig {
            name: "upgrade-fixture".into(),
            title: "Upgrade Fixture".into(),
            bundle_id: "com.example.upgradefixture".into(),
            ui_framework: UiFramework::GpuiKit,
            targets: vec![Platform::MacOs],
        };
        scaffold(dir.path(), &config).unwrap();
        fs::remove_file(TemplateManifest::path(dir.path())).unwrap();

        let plan = plan_project(dir.path(), TEMPLATE_VERSION).unwrap();
        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.base_resolution, BaseResolution::InferredExact);
    }
}
