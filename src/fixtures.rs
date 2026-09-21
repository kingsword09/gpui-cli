//! Deterministic development fixtures used by the agent-native test plan.
//!
//! These types deliberately live next to the CLI rather than in a future
//! scenario executor.  They give the baseline and later scenario work one
//! strict, versioned input contract without pretending that the current CLI
//! can execute a scenario yet.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;

/// One of the fixed component fixtures from the agent-native roadmap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Fixture {
    Counter(CounterFixture),
    LoginForm(LoginFormFixture),
    VirtualList(VirtualListFixture),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterFixture {
    pub schema_version: u32,
    pub component: String,
    pub initial_value: i64,
    pub increment_by: i64,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginFormFixture {
    pub schema_version: u32,
    pub component: String,
    pub initial_username: String,
    pub initial_password: String,
    pub network: FixtureNetwork,
    pub allowed_runtime_errors: Vec<AllowedRuntimeError>,
    pub sensitive_logical_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureNetwork {
    pub mode: String,
    pub latency_ms: u64,
    pub response: FixtureResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureResponse {
    pub status: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedRuntimeError {
    pub source: String,
    pub code: String,
    pub phase: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualListFixture {
    pub schema_version: u32,
    pub component: String,
    pub items: VirtualListItems,
    pub row_height: u32,
    pub initial_scroll_y: u32,
    pub show_images: bool,
    pub animation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualListItems {
    pub count: u32,
    pub stable_key_prefix: String,
    pub stable_key_digits: u8,
    pub label_prefix: String,
}

impl Fixture {
    /// Parse and validate one JSON fixture without accepting unknown fields.
    pub fn parse(input: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(input).context("parsing fixture JSON")?;
        let component = value
            .get("component")
            .and_then(Value::as_str)
            .context("fixture is missing a string component")?;

        let fixture = match component {
            "Counter" => {
                Self::Counter(serde_json::from_value(value).context("decoding Counter fixture")?)
            }
            "LoginForm" => Self::LoginForm(
                serde_json::from_value(value).context("decoding LoginForm fixture")?,
            ),
            "VirtualList" => Self::VirtualList(
                serde_json::from_value(value).context("decoding VirtualList fixture")?,
            ),
            other => bail!("unsupported fixture component '{other}'"),
        };
        fixture.validate()?;
        Ok(fixture)
    }

    /// Validate the fields whose meaning is shared by all fixture variants.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Counter(fixture) => {
                validate_header(fixture.schema_version, &fixture.component, "Counter")?;
                if fixture.increment_by <= 0 {
                    bail!("Counter increment_by must be greater than zero");
                }
            }
            Self::LoginForm(fixture) => {
                validate_header(fixture.schema_version, &fixture.component, "LoginForm")?;
                if fixture.network.mode != "fixture" {
                    bail!("LoginForm network.mode must be 'fixture'");
                }
                if fixture.network.response.status != "error" {
                    bail!("LoginForm fixture response.status must be 'error'");
                }
                if fixture.allowed_runtime_errors.is_empty() {
                    bail!("LoginForm must declare its expected runtime error");
                }
            }
            Self::VirtualList(fixture) => {
                validate_header(fixture.schema_version, &fixture.component, "VirtualList")?;
                if fixture.items.count == 0 {
                    bail!("VirtualList items.count must be greater than zero");
                }
                if fixture.items.stable_key_digits == 0 {
                    bail!("VirtualList stable_key_digits must be greater than zero");
                }
                if fixture.row_height == 0 {
                    bail!("VirtualList row_height must be greater than zero");
                }
            }
        }
        Ok(())
    }

    /// Return the stable content hash used to identify a fixture revision.
    pub fn hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("fixture serialization is infallible");
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    pub fn component(&self) -> &'static str {
        match self {
            Self::Counter(_) => "Counter",
            Self::LoginForm(_) => "LoginForm",
            Self::VirtualList(_) => "VirtualList",
        }
    }
}

impl VirtualListFixture {
    /// Generate the stable key for a list item without using a mutable index.
    pub fn stable_key(&self, index: u32) -> Option<String> {
        (index < self.items.count).then(|| {
            format!(
                "{}{:0width$}",
                self.items.stable_key_prefix,
                index,
                width = self.items.stable_key_digits as usize
            )
        })
    }
}

fn validate_header(version: u32, component: &str, expected: &str) -> Result<()> {
    if version != SCHEMA_VERSION {
        bail!(
            "{expected} fixture schema_version {version} is unsupported; expected {SCHEMA_VERSION}"
        );
    }
    if component != expected {
        bail!("fixture component '{component}' does not match {expected}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> &'static str {
        match name {
            "counter" => include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/agent-native/counter-zero.json"
            )),
            "login" => include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/agent-native/login-invalid.json"
            )),
            "list" => include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/agent-native/list-1000.json"
            )),
            _ => panic!("unknown fixture {name}"),
        }
    }

    #[test]
    fn shipped_fixtures_are_strict_and_versioned() {
        let counter = Fixture::parse(fixture("counter")).unwrap();
        let login = Fixture::parse(fixture("login")).unwrap();
        let list = Fixture::parse(fixture("list")).unwrap();

        assert_eq!(counter.component(), "Counter");
        assert_eq!(login.component(), "LoginForm");
        assert_eq!(list.component(), "VirtualList");
        assert_ne!(counter.hash(), login.hash());
        assert_ne!(login.hash(), list.hash());
    }

    #[test]
    fn virtual_list_keys_are_stable_and_bounded() {
        let Fixture::VirtualList(list) = Fixture::parse(fixture("list")).unwrap() else {
            panic!("expected VirtualList fixture");
        };

        assert_eq!(list.stable_key(0).as_deref(), Some("item-0000"));
        assert_eq!(list.stable_key(42).as_deref(), Some("item-0042"));
        assert_eq!(list.stable_key(999).as_deref(), Some("item-0999"));
        assert_eq!(list.stable_key(1000), None);
    }

    #[test]
    fn unknown_fields_and_invalid_values_are_rejected() {
        let unknown = r#"{
            "schema_version": 1,
            "component": "Counter",
            "initial_value": 0,
            "increment_by": 1,
            "disabled": false,
            "typo": true
        }"#;
        assert!(Fixture::parse(unknown).is_err());

        let invalid = r#"{
            "schema_version": 1,
            "component": "VirtualList",
            "items": {
                "count": 1000,
                "stable_key_prefix": "item-",
                "stable_key_digits": 4,
                "label_prefix": "Item "
            },
            "row_height": 0,
            "initial_scroll_y": 0,
            "show_images": false,
            "animation": "disabled"
        }"#;
        assert!(Fixture::parse(invalid).is_err());
    }
}
