//! Strict visual baseline loading and exact comparison primitives.
//!
//! The first baseline slice intentionally uses an exact SHA-256 algorithm.
//! It establishes safe paths, image dimensions, and a complete BaselineKey;
//! tolerance/mask algorithms can be added later without treating an unknown
//! or incomparable image as a pass.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const EXACT_ALGORITHM: &str = "exact-sha256-v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineKey {
    pub scenario: String,
    pub fixture_hash: String,
    pub target: String,
    pub backend: String,
    pub os: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub scale_milli: u32,
    pub theme: String,
    pub locale: String,
    pub font_fingerprint: String,
    pub scope: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineImage {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineAlgorithm {
    pub id: String,
    pub version: u32,
    pub tolerance_milli: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineManifest {
    pub schema_version: u32,
    pub baseline_id: String,
    pub key: BaselineKey,
    pub image: BaselineImage,
    pub algorithm: BaselineAlgorithm,
}

#[derive(Clone, Debug)]
pub struct LoadedBaseline {
    pub manifest_path: PathBuf,
    pub image_path: PathBuf,
    pub manifest: BaselineManifest,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BaselineLoad {
    Loaded,
    Missing { manifest_path: PathBuf },
    Invalid { code: String, message: String },
    NotComparable { mismatches: Vec<String> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BaselineComparison {
    Matched {
        baseline_sha256: String,
        actual_sha256: String,
        pixel_width: u32,
        pixel_height: u32,
    },
    Different {
        baseline_sha256: String,
        actual_sha256: String,
        pixel_width: u32,
        pixel_height: u32,
    },
    NotComparable {
        code: String,
        message: String,
    },
}

/// Load `dev/baselines/<target>/<baseline_id>/manifest.json` and validate the
/// complete image/hash/path contract. No baseline is created or updated by
/// this function.
pub fn load_baseline(
    project_root: &Path,
    target: &str,
    baseline_id: &str,
    expected_key: &BaselineKey,
) -> (BaselineLoad, Option<LoadedBaseline>) {
    let Some(target) = safe_identifier(target) else {
        return (
            invalid("invalid_target", "baseline target is not a safe identifier"),
            None,
        );
    };
    let Some(baseline_id) = safe_identifier(baseline_id) else {
        return (
            invalid(
                "invalid_baseline_id",
                "baseline_id is not a safe identifier",
            ),
            None,
        );
    };
    let root = project_root.join("dev/baselines");
    let baseline_dir = root.join(target).join(baseline_id);
    let manifest_path = baseline_dir.join("manifest.json");
    if !manifest_path.is_file() {
        return (BaselineLoad::Missing { manifest_path }, None);
    }
    if let Err(error) = reject_symlink_path(&root, &manifest_path) {
        return (invalid("unsafe_baseline_path", &error), None);
    }
    let manifest_metadata = match fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return (
                invalid("unsafe_baseline_manifest", "manifest is not a regular file"),
                None,
            );
        }
        Err(error) => return (invalid("baseline_read_failed", &error.to_string()), None),
    };
    if manifest_metadata.len() > MAX_MANIFEST_BYTES {
        return (
            invalid(
                "baseline_manifest_too_large",
                "baseline manifest exceeds its size limit",
            ),
            None,
        );
    }
    let manifest_bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(error) => return (invalid("baseline_read_failed", &error.to_string()), None),
    };
    let manifest: BaselineManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => return (invalid("baseline_invalid", &error.to_string()), None),
    };
    if let Err(error) = validate_manifest(&manifest, baseline_id) {
        return (invalid("baseline_invalid", &error), None);
    }
    let mismatches = key_mismatches(&manifest.key, expected_key);
    if !mismatches.is_empty() {
        return (BaselineLoad::NotComparable { mismatches }, None);
    }

    let image_path = baseline_dir.join(&manifest.image.path);
    if let Err(error) = validate_relative_image_path(&baseline_dir, &image_path) {
        return (invalid("unsafe_baseline_image", &error), None);
    }
    let image_metadata = match fs::symlink_metadata(&image_path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return (
                invalid(
                    "unsafe_baseline_image",
                    "baseline image is not a regular file",
                ),
                None,
            );
        }
        Err(error) => return (invalid("baseline_image_missing", &error.to_string()), None),
    };
    if image_metadata.len() != manifest.image.bytes {
        return (
            invalid(
                "baseline_image_size_mismatch",
                "baseline image bytes do not match its manifest",
            ),
            None,
        );
    }
    if image_metadata.len() > MAX_IMAGE_BYTES {
        return (
            invalid(
                "baseline_image_too_large",
                "baseline image exceeds its size limit",
            ),
            None,
        );
    }
    let bytes = match fs::read(&image_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                invalid("baseline_image_read_failed", &error.to_string()),
                None,
            );
        }
    };
    let actual_hash = hash_bytes(&bytes);
    if actual_hash != manifest.image.sha256 {
        return (
            invalid(
                "baseline_image_hash_mismatch",
                "baseline image does not match its declared SHA-256",
            ),
            None,
        );
    }
    let Some((width, height)) = png_dimensions(&bytes) else {
        return (
            invalid(
                "baseline_image_invalid",
                "baseline image is not a bounded PNG",
            ),
            None,
        );
    };
    if (width, height) != (manifest.image.pixel_width, manifest.image.pixel_height) {
        return (
            invalid(
                "baseline_image_dimensions_mismatch",
                "baseline image dimensions do not match its manifest",
            ),
            None,
        );
    }
    (
        BaselineLoad::Loaded,
        Some(LoadedBaseline {
            manifest_path,
            image_path,
            manifest,
            bytes,
        }),
    )
}

/// Compare one captured PNG with an already loaded baseline. This first
/// algorithm is deliberately exact; dimensions are checked before hashes so
/// a differently sized image is reported as not comparable rather than as a
/// visual regression.
pub fn compare_png(baseline: &LoadedBaseline, actual: &[u8]) -> BaselineComparison {
    let Some((width, height)) = png_dimensions(actual) else {
        return BaselineComparison::NotComparable {
            code: "actual_image_invalid".into(),
            message: "captured image is not a bounded PNG".into(),
        };
    };
    if (width, height)
        != (
            baseline.manifest.image.pixel_width,
            baseline.manifest.image.pixel_height,
        )
    {
        return BaselineComparison::NotComparable {
            code: "image_dimensions_mismatch".into(),
            message: "captured image dimensions do not match the baseline".into(),
        };
    }
    let actual_sha256 = hash_bytes(actual);
    if actual_sha256 == baseline.manifest.image.sha256 {
        BaselineComparison::Matched {
            baseline_sha256: baseline.manifest.image.sha256.clone(),
            actual_sha256,
            pixel_width: width,
            pixel_height: height,
        }
    } else {
        BaselineComparison::Different {
            baseline_sha256: baseline.manifest.image.sha256.clone(),
            actual_sha256,
            pixel_width: width,
            pixel_height: height,
        }
    }
}

fn validate_manifest(manifest: &BaselineManifest, baseline_id: &str) -> Result<(), String> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported baseline schema_version {}; expected {SCHEMA_VERSION}",
            manifest.schema_version
        ));
    }
    if manifest.baseline_id != baseline_id {
        return Err("baseline_id does not match its directory".into());
    }
    if manifest.image.bytes == 0 || manifest.image.bytes > MAX_IMAGE_BYTES {
        return Err("baseline image bytes are outside the allowed range".into());
    }
    if manifest.image.pixel_width == 0 || manifest.image.pixel_height == 0 {
        return Err("baseline image dimensions must be positive".into());
    }
    if manifest.algorithm.id != EXACT_ALGORITHM || manifest.algorithm.version != 1 {
        return Err(format!(
            "unsupported baseline algorithm {}; expected {EXACT_ALGORITHM} v1",
            manifest.algorithm.id
        ));
    }
    if manifest.algorithm.tolerance_milli != 0 {
        return Err("exact-sha256-v1 requires tolerance_milli = 0".into());
    }
    if !is_sha256(&manifest.image.sha256) {
        return Err("baseline image sha256 must be sha256:<64 lowercase hex>".into());
    }
    Ok(())
}

fn key_mismatches(actual: &BaselineKey, expected: &BaselineKey) -> Vec<String> {
    let mut mismatches = Vec::new();
    macro_rules! compare {
        ($field:ident) => {
            if actual.$field != expected.$field {
                mismatches.push(stringify!($field).to_owned());
            }
        };
    }
    compare!(scenario);
    compare!(fixture_hash);
    compare!(target);
    compare!(backend);
    compare!(os);
    compare!(viewport_width);
    compare!(viewport_height);
    compare!(scale_milli);
    compare!(theme);
    compare!(locale);
    compare!(font_fingerprint);
    compare!(scope);
    mismatches
}

fn validate_relative_image_path(base: &Path, image: &Path) -> Result<(), String> {
    if image
        .strip_prefix(base)
        .ok()
        .is_none_or(|relative| relative.as_os_str().is_empty())
    {
        return Err("baseline image escapes its baseline directory".into());
    }
    reject_symlink_path(base, image)
}

fn reject_symlink_path(root: &Path, path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(root)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("baseline root is a symbolic link".into());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "baseline path escapes its root".to_owned())?;
    let mut current = root.to_owned();
    for component in relative.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err("baseline path contains an unsafe component".into());
        }
        current.push(component.as_os_str());
        if fs::symlink_metadata(&current)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("baseline path contains a symbolic link".into());
        }
    }
    Ok(())
}

fn safe_identifier(value: &str) -> Option<&str> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.contains('\0')
        || value.contains('/')
        || value.contains('\\')
        || value == "."
        || value == ".."
    {
        return None;
    }
    Some(value)
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn invalid(code: &str, message: &str) -> BaselineLoad {
    BaselineLoad::Invalid {
        code: code.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn key() -> BaselineKey {
        BaselineKey {
            scenario: "counter-basic".into(),
            fixture_hash: "sha256:fixture".into(),
            target: "macos".into(),
            backend: "metal".into(),
            os: "macos-26".into(),
            viewport_width: 640,
            viewport_height: 480,
            scale_milli: 1000,
            theme: "light".into(),
            locale: "en-US".into(),
            font_fingerprint: "sha256:fonts".into(),
            scope: "window".into(),
        }
    }

    fn png(width: u32, height: u32, marker: u8) -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes[31] = marker;
        bytes
    }

    fn write_manifest(root: &Path, image: &[u8], baseline_key: BaselineKey) {
        let dir = root.join("dev/baselines/macos/counter-baseline");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("image.png"), image).unwrap();
        let manifest = BaselineManifest {
            schema_version: SCHEMA_VERSION,
            baseline_id: "counter-baseline".into(),
            key: baseline_key,
            image: BaselineImage {
                path: "image.png".into(),
                sha256: hash_bytes(image),
                bytes: image.len() as u64,
                pixel_width: 2,
                pixel_height: 3,
            },
            algorithm: BaselineAlgorithm {
                id: EXACT_ALGORITHM.into(),
                version: 1,
                tolerance_milli: 0,
            },
        };
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn missing_baseline_is_not_a_failure_or_a_pass() {
        let root = tempdir().unwrap();
        let (status, loaded) = load_baseline(root.path(), "macos", "missing", &key());
        assert!(loaded.is_none());
        assert!(matches!(status, BaselineLoad::Missing { .. }));
    }

    #[test]
    fn baseline_key_mismatch_is_not_comparable() {
        let root = tempdir().unwrap();
        let image = png(2, 3, 1);
        write_manifest(root.path(), &image, key());
        let mut expected = key();
        expected.locale = "zh-CN".into();
        let (status, loaded) = load_baseline(root.path(), "macos", "counter-baseline", &expected);
        assert!(loaded.is_none());
        assert_eq!(
            status,
            BaselineLoad::NotComparable {
                mismatches: vec!["locale".into()]
            }
        );
    }

    #[test]
    fn exact_comparison_distinguishes_match_and_difference() {
        let root = tempdir().unwrap();
        let image = png(2, 3, 1);
        write_manifest(root.path(), &image, key());
        let (status, loaded) = load_baseline(root.path(), "macos", "counter-baseline", &key());
        assert_eq!(status, BaselineLoad::Loaded);
        let baseline = loaded.unwrap();
        assert!(matches!(
            compare_png(&baseline, &image),
            BaselineComparison::Matched { .. }
        ));
        assert!(matches!(
            compare_png(&baseline, &png(2, 3, 2)),
            BaselineComparison::Different { .. }
        ));
        assert!(matches!(
            compare_png(&baseline, &png(3, 3, 1)),
            BaselineComparison::NotComparable { code, .. } if code == "image_dimensions_mismatch"
        ));
    }

    #[test]
    fn tampered_image_hash_is_rejected() {
        let root = tempdir().unwrap();
        let image = png(2, 3, 1);
        write_manifest(root.path(), &image, key());
        fs::write(
            root.path()
                .join("dev/baselines/macos/counter-baseline/image.png"),
            png(2, 3, 2),
        )
        .unwrap();
        let (status, loaded) = load_baseline(root.path(), "macos", "counter-baseline", &key());
        assert!(loaded.is_none());
        assert!(matches!(
            status,
            BaselineLoad::Invalid { code, .. } if code == "baseline_image_hash_mismatch"
        ));
    }

    #[test]
    fn manifest_rejects_unknown_fields() {
        let value = json!({"schema_version": 1, "unknown": true});
        assert!(serde_json::from_value::<BaselineManifest>(value).is_err());
    }
}
