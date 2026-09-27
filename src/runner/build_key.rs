//! Deterministic build-key dimensions.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// The complete set of dimensions that currently participates in a build key.
///
/// Hash-valued fields are supplied by the caller after the corresponding
/// input has been collected. This type does not probe the toolchain, inspect
/// the environment, or infer native configuration by itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BuildKeyMaterial {
    pub source_manifest_hash: String,
    pub cargo_lock_hash: String,
    pub target_triple: String,
    pub profile: String,
    pub features: Vec<String>,
    pub abi: Option<String>,
    pub native_config_hash: String,
    pub toolchain_fingerprint: String,
    pub relevant_env_hash: String,
    pub preview_registry_hash: String,
}

/// A normalized build-key material plus its content-addressed digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BuildKey {
    material: BuildKeyMaterial,
    key_hash: String,
}

impl BuildKey {
    /// Normalizes feature ordering and computes the SHA-256 of the canonical
    /// JSON material. Every required dimension must be explicit and non-empty.
    pub fn new(material: BuildKeyMaterial) -> Result<Self> {
        let material = material.normalize()?;
        let encoded = serde_json::to_vec(&material).context("serializing build-key material")?;
        let key_hash = format!("{:x}", Sha256::digest(encoded));
        Ok(Self { material, key_hash })
    }

    pub fn material(&self) -> &BuildKeyMaterial {
        &self.material
    }

    pub fn key_hash(&self) -> &str {
        &self.key_hash
    }
}

impl BuildKeyMaterial {
    fn normalize(mut self) -> Result<Self> {
        self.source_manifest_hash = required("source_manifest_hash", self.source_manifest_hash)?;
        self.cargo_lock_hash = required("cargo_lock_hash", self.cargo_lock_hash)?;
        self.target_triple = required("target_triple", self.target_triple)?;
        self.profile = required("profile", self.profile)?;
        self.native_config_hash = required("native_config_hash", self.native_config_hash)?;
        self.toolchain_fingerprint = required("toolchain_fingerprint", self.toolchain_fingerprint)?;
        self.relevant_env_hash = required("relevant_env_hash", self.relevant_env_hash)?;
        self.preview_registry_hash = required("preview_registry_hash", self.preview_registry_hash)?;

        let mut features = Vec::with_capacity(self.features.len());
        for feature in self.features {
            let feature = feature.trim().to_string();
            if feature.is_empty() {
                bail!("BuildKey dimension `features` contains an empty feature");
            }
            features.push(feature);
        }
        features.sort();
        features.dedup();
        self.features = features;

        if let Some(abi) = self.abi.as_mut() {
            *abi = abi.trim().to_string();
            if abi.is_empty() {
                bail!("BuildKey dimension `abi` must be absent or non-empty");
            }
        }
        Ok(self)
    }
}

/// Hashes an explicit environment allowlist. The caller owns the allowlist;
/// this helper never scans the process environment or decides which values
/// are safe to include.
pub fn hash_relevant_environment<I>(entries: I) -> Result<String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut entries: Vec<_> = entries
        .into_iter()
        .map(|(name, value)| {
            let name = name.trim().to_string();
            if name.is_empty() {
                bail!("relevant environment name must not be empty");
            }
            Ok((name, value))
        })
        .collect::<Result<_>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        bail!("relevant environment names must be unique");
    }
    let encoded = serde_json::to_vec(&entries).context("serializing relevant environment")?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn required(name: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("BuildKey dimension `{name}` must not be empty");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn material() -> BuildKeyMaterial {
        BuildKeyMaterial {
            source_manifest_hash: "source".into(),
            cargo_lock_hash: "lock".into(),
            target_triple: "aarch64-apple-darwin".into(),
            profile: "dev".into(),
            features: vec!["gpui-dev".into(), "default".into(), "gpui-dev".into()],
            abi: None,
            native_config_hash: "native".into(),
            toolchain_fingerprint: "rustc".into(),
            relevant_env_hash: "env".into(),
            preview_registry_hash: "registry".into(),
        }
    }

    #[test]
    fn normalizes_features_before_hashing() {
        let first = BuildKey::new(material()).unwrap();
        let mut alternate = material();
        alternate.features = vec!["gpui-dev".into(), "default".into()];
        let second = BuildKey::new(alternate).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.material().features,
            vec!["default".to_string(), "gpui-dev".to_string()]
        );
        assert_eq!(first.key_hash().len(), 64);
    }

    #[test]
    fn every_build_dimension_changes_the_key() {
        let baseline = BuildKey::new(material()).unwrap();
        let variations = [
            BuildKeyMaterial {
                source_manifest_hash: "changed".into(),
                ..material()
            },
            BuildKeyMaterial {
                cargo_lock_hash: "changed".into(),
                ..material()
            },
            BuildKeyMaterial {
                target_triple: "x86_64-unknown-linux-gnu".into(),
                ..material()
            },
            BuildKeyMaterial {
                profile: "release".into(),
                ..material()
            },
            BuildKeyMaterial {
                features: vec!["other-feature".into()],
                ..material()
            },
            BuildKeyMaterial {
                abi: Some("arm64-v8a".into()),
                ..material()
            },
            BuildKeyMaterial {
                native_config_hash: "changed".into(),
                ..material()
            },
            BuildKeyMaterial {
                toolchain_fingerprint: "changed".into(),
                ..material()
            },
            BuildKeyMaterial {
                relevant_env_hash: "changed".into(),
                ..material()
            },
            BuildKeyMaterial {
                preview_registry_hash: "changed".into(),
                ..material()
            },
        ];

        for variation in variations {
            assert_ne!(
                baseline.key_hash(),
                BuildKey::new(variation).unwrap().key_hash()
            );
        }
    }

    #[test]
    fn serialized_key_retains_material_and_hash_for_evidence() {
        let key = BuildKey::new(material()).unwrap();
        let value: Value = serde_json::to_value(&key).unwrap();

        assert_eq!(value["material"]["profile"], "dev");
        assert_eq!(value["material"]["features"][0], "default");
        assert_eq!(value["key_hash"], key.key_hash());
    }

    #[test]
    fn empty_required_dimensions_are_rejected() {
        let mut invalid = material();
        invalid.native_config_hash.clear();
        let error = BuildKey::new(invalid).unwrap_err();
        assert!(error.to_string().contains("native_config_hash"));

        let mut invalid = material();
        invalid.features.push(" ".into());
        let error = BuildKey::new(invalid).unwrap_err();
        assert!(error.to_string().contains("empty feature"));

        let mut invalid = material();
        invalid.abi = Some(" ".into());
        let error = BuildKey::new(invalid).unwrap_err();
        assert!(error.to_string().contains("abi"));
    }

    #[test]
    fn relevant_environment_hash_is_ordered_and_allowlist_scoped() {
        let first = hash_relevant_environment([
            ("RUSTFLAGS".into(), "-C opt-level=2".into()),
            ("CC".into(), "clang".into()),
        ])
        .unwrap();
        let second = hash_relevant_environment([
            ("CC".into(), "clang".into()),
            ("RUSTFLAGS".into(), "-C opt-level=2".into()),
        ])
        .unwrap();
        assert_eq!(first, second);
        assert_ne!(
            first,
            hash_relevant_environment([("CC".into(), "gcc".into())]).unwrap()
        );
        assert!(hash_relevant_environment([(" ".into(), "value".into())]).is_err());
        assert!(
            hash_relevant_environment(
                [("CC".into(), "clang".into()), ("CC".into(), "gcc".into()),]
            )
            .is_err()
        );
    }
}
