//! Explicit visual baseline review and approval operations.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::run::Project;
use crate::scenario::baseline::{
    BaselineKey, BaselineLoad, BaselineManifest, MAX_IMAGE_BYTES, hash_bytes, is_safe_identifier,
    load_baseline, manifest_for_image, validate_baseline_key,
};

const APPROVAL_SCHEMA_VERSION: u32 = 1;
const MAX_REASON_BYTES: usize = 4096;

#[derive(Subcommand)]
pub enum BaselineCommands {
    /// Explicitly approve a user-selected PNG as a visual baseline
    Approve(ApproveArgs),
}

#[derive(Args)]
pub struct ApproveArgs {
    /// Baseline target directory and BaselineKey target
    #[arg(long)]
    pub target: String,
    /// Safe baseline identifier
    #[arg(long)]
    pub baseline_id: String,
    /// JSON file containing the complete BaselineKey
    #[arg(long)]
    pub key: PathBuf,
    /// PNG to approve; it is copied into the project baseline directory
    #[arg(long)]
    pub image: PathBuf,
    /// Optional diff artifact to retain in the approval record
    #[arg(long)]
    pub diff: Option<PathBuf>,
    /// Human reason for this explicit approval
    #[arg(long)]
    pub reason: String,
    /// Replace an existing baseline after moving the old revision to history
    #[arg(long)]
    pub replace: bool,
    /// Emit the approval record as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ApprovalRecord {
    schema_version: u32,
    action: &'static str,
    approved_at_ms: u64,
    target: String,
    baseline_id: String,
    reason: String,
    manifest: BaselineManifest,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous: Option<PreviousRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<DiffReference>,
}

#[derive(Clone, Debug, Serialize)]
struct PreviousRevision {
    history_path: String,
    manifest: BaselineManifest,
}

#[derive(Clone, Debug, Serialize)]
struct DiffReference {
    path: String,
    bytes: u64,
    sha256: String,
}

pub fn handle_baseline(command: BaselineCommands) -> Result<()> {
    match command {
        BaselineCommands::Approve(args) => approve(args),
    }
}

fn approve(args: ApproveArgs) -> Result<()> {
    let project = Project::load(None)?;
    let project_root = project
        .root
        .canonicalize()
        .context("resolving project root")?;
    approve_at(&project_root, args)
}

fn approve_at(project_root: &Path, args: ApproveArgs) -> Result<()> {
    let project_root = project_root
        .canonicalize()
        .context("resolving approval project root")?;
    if !is_safe_identifier(&args.target) {
        bail!("baseline target is not a safe identifier");
    }
    if !is_safe_identifier(&args.baseline_id) {
        bail!("baseline_id is not a safe identifier");
    }
    if args.reason.trim().is_empty() || args.reason.len() > MAX_REASON_BYTES {
        bail!("reason must be non-empty and at most {MAX_REASON_BYTES} bytes");
    }

    let key_path = project_file(&project_root, &args.key)?;
    let key: BaselineKey = serde_json::from_slice(
        &fs::read(&key_path).with_context(|| format!("reading key {}", key_path.display()))?,
    )
    .context("parsing BaselineKey JSON")?;
    validate_baseline_key(&key).map_err(|error| anyhow::anyhow!(error))?;
    if key.target != args.target {
        bail!(
            "BaselineKey target {} does not match --target {}",
            key.target,
            args.target
        );
    }

    let image_path = resolve_any_file(&args.image)?;
    let image_bytes = read_regular_file(&image_path, MAX_IMAGE_BYTES)
        .with_context(|| format!("reading PNG {}", args.image.display()))?;
    let manifest = manifest_for_image(&args.baseline_id, key, &image_bytes)
        .map_err(|error| anyhow::anyhow!(error))?;

    let diff = args
        .diff
        .as_ref()
        .map(|path| read_diff_reference(&project_root, path))
        .transpose()?;

    let target_dir = project_root.join("dev/baselines").join(&args.target);
    reject_symlink_components(&project_root, &target_dir)?;
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("creating {}", target_dir.display()))?;
    reject_symlink_components(&project_root, &target_dir)?;

    let baseline_dir = target_dir.join(&args.baseline_id);
    let existing = existing_baseline(&project_root, &args.target, &args.baseline_id)?;
    if existing.is_some() && !args.replace {
        bail!("baseline already exists; pass --replace for an explicit reviewed replacement");
    }

    let approved_at_ms = now_ms();
    let revision = manifest
        .image
        .sha256
        .strip_prefix("sha256:")
        .unwrap_or(&manifest.image.sha256);
    let revision = &revision[..revision.len().min(16)];
    let history_path = target_dir
        .join(".history")
        .join(&args.baseline_id)
        .join(format!("{approved_at_ms}-{revision}"));
    let history_relative = relative_to_root(&project_root, &history_path);
    if existing.is_some() {
        reject_symlink_components(&project_root, &target_dir.join(".history"))?;
        if let Ok(metadata) = fs::symlink_metadata(&history_path) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("baseline history destination is unsafe");
            }
            bail!("baseline history destination already exists");
        }
    }

    let previous = existing.as_ref().map(|old| PreviousRevision {
        history_path: history_relative.clone(),
        manifest: old.clone(),
    });
    let approval = ApprovalRecord {
        schema_version: APPROVAL_SCHEMA_VERSION,
        action: "approve",
        approved_at_ms,
        target: args.target.clone(),
        baseline_id: args.baseline_id.clone(),
        reason: args.reason,
        manifest: manifest.clone(),
        previous,
        diff,
    };

    let stage = target_dir.join(format!(
        ".{}.staging-{}-{}",
        args.baseline_id, approved_at_ms, revision
    ));
    if let Ok(metadata) = fs::symlink_metadata(&stage) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("baseline staging path is unsafe");
        }
        bail!("baseline staging path already exists");
    }
    fs::create_dir_all(&stage).context("creating baseline staging directory")?;
    if let Err(error) = write_stage(&stage, &image_bytes, &manifest, &approval) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }

    let moved_old = if existing.is_some() {
        let history_parent = history_path
            .parent()
            .context("baseline history has no parent")?;
        reject_symlink_components(&project_root, history_parent)?;
        fs::create_dir_all(history_parent)?;
        if let Err(error) = fs::rename(&baseline_dir, &history_path) {
            let _ = fs::remove_dir_all(&stage);
            return Err(error).context("moving old baseline into history");
        }
        true
    } else {
        false
    };

    if let Err(error) = fs::rename(&stage, &baseline_dir) {
        if moved_old {
            let _ = fs::rename(&history_path, &baseline_dir);
        }
        let _ = fs::remove_dir_all(&stage);
        return Err(error).context("publishing approved baseline");
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&approval)?);
    } else if moved_old {
        println!(
            "approved baseline {}; previous revision moved to {}",
            args.baseline_id, history_relative
        );
    } else {
        println!("approved baseline {}", args.baseline_id);
    }
    Ok(())
}

fn existing_baseline(
    project_root: &Path,
    target: &str,
    baseline_id: &str,
) -> Result<Option<BaselineManifest>> {
    let baseline_dir = project_root
        .join("dev/baselines")
        .join(target)
        .join(baseline_id);
    let Ok(metadata) = fs::symlink_metadata(&baseline_dir) else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("existing baseline path is not a regular directory");
    }
    let manifest_path = baseline_dir.join("manifest.json");
    let manifest: BaselineManifest = serde_json::from_slice(
        &fs::read(&manifest_path).context("reading existing baseline manifest")?,
    )
    .context("parsing existing baseline manifest")?;
    let (status, loaded) = load_baseline(project_root, target, baseline_id, &manifest.key);
    if !matches!(status, BaselineLoad::Loaded) || loaded.is_none() {
        bail!("existing baseline is invalid and cannot be moved into history");
    }
    Ok(Some(manifest))
}

fn write_stage(
    stage: &Path,
    image: &[u8],
    manifest: &BaselineManifest,
    approval: &ApprovalRecord,
) -> Result<()> {
    fs::write(stage.join("image.png"), image).context("writing staged baseline image")?;
    write_json(stage.join("manifest.json"), manifest).context("writing staged manifest")?;
    write_json(stage.join("approval.json"), approval).context("writing approval record")?;
    Ok(())
}

fn write_json<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

fn read_diff_reference(project_root: &Path, path: &Path) -> Result<DiffReference> {
    let path = project_file(project_root, path)?;
    let bytes = read_regular_file(&path, MAX_IMAGE_BYTES)
        .with_context(|| format!("reading diff {}", path.display()))?;
    Ok(DiffReference {
        path: relative_to_root(project_root, &path),
        bytes: bytes.len() as u64,
        sha256: hash_bytes(&bytes),
    })
}

fn project_file(project_root: &Path, path: &Path) -> Result<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        project_root.join(path)
    };
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("resolving project path {}", candidate.display()))?;
    if !canonical.starts_with(project_root) {
        bail!("path escapes the project root: {}", path.display());
    }
    reject_symlink_components(project_root, &canonical)?;
    Ok(canonical)
}

fn resolve_any_file(path: &Path) -> Result<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if fs::symlink_metadata(&candidate)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        bail!("selected image path is a symbolic link");
    }
    candidate
        .canonicalize()
        .with_context(|| format!("resolving selected image {}", candidate.display()))
}

fn read_regular_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("path must be a regular non-symlink file");
    }
    if metadata.len() > max_bytes {
        bail!("file exceeds its size limit");
    }
    Ok(fs::read(path)?)
}

fn reject_symlink_components(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("path escapes the project root"))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            bail!("path contains an unsafe component");
        }
        current.push(component.as_os_str());
        if let Ok(metadata) = fs::symlink_metadata(&current)
            && metadata.file_type().is_symlink()
        {
            bail!("path contains a symbolic link: {}", current.display());
        }
    }
    Ok(())
}

fn relative_to_root(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn key() -> BaselineKey {
        BaselineKey {
            scenario: "counter-basic".into(),
            fixture_hash: "sha256:fixture".into(),
            target: "macos".into(),
            backend: "metal".into(),
            os: "macos-26".into(),
            viewport_width: 2,
            viewport_height: 1,
            scale_milli: 1000,
            theme: "light".into(),
            locale: "en-US".into(),
            font_fingerprint: "sha256:fonts".into(),
            scope: "window".into(),
        }
    }

    fn png(red: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[red, 0, 0, 255, 0, 0, 0, 255])
                .unwrap();
        }
        bytes
    }

    fn args(root: &Path, image: &str, replace: bool) -> ApproveArgs {
        ApproveArgs {
            target: "macos".into(),
            baseline_id: "counter-basic".into(),
            key: root.join("key.json"),
            image: root.join(image),
            diff: None,
            reason: "reviewed visual change".into(),
            replace,
            json: false,
        }
    }

    #[test]
    fn approval_writes_manifest_record_and_preserves_replaced_revision() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("key.json"),
            serde_json::to_vec_pretty(&key()).unwrap(),
        )
        .unwrap();
        fs::write(root.path().join("first.png"), png(1)).unwrap();
        approve_at(root.path(), args(root.path(), "first.png", false)).unwrap();
        assert!(
            root.path()
                .join("dev/baselines/macos/counter-basic/approval.json")
                .is_file()
        );
        assert!(approve_at(root.path(), args(root.path(), "first.png", false)).is_err());

        fs::write(root.path().join("second.png"), png(2)).unwrap();
        approve_at(root.path(), args(root.path(), "second.png", true)).unwrap();
        let history_root = root
            .path()
            .join("dev/baselines/macos/.history/counter-basic");
        let history = fs::read_dir(history_root).unwrap().next().unwrap().unwrap();
        assert!(history.path().join("manifest.json").is_file());
        assert!(history.path().join("image.png").is_file());
        let approval: serde_json::Value = serde_json::from_slice(
            &fs::read(
                root.path()
                    .join("dev/baselines/macos/counter-basic/approval.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(approval["action"], "approve");
        assert_eq!(approval["previous"]["manifest"]["image"]["pixel_width"], 2);
        assert!(
            approval["previous"]["history_path"]
                .as_str()
                .unwrap()
                .contains(".history/counter-basic/")
        );
    }
}
