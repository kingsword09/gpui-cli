//! Parse project metadata with TOML's string escaping and section rules.

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Manifest {
    pub app: App,
    #[serde(default)]
    pub run: crate::device::inventory::Defaults,
}

#[derive(Deserialize)]
pub struct App {
    pub name: String,
    pub title: Option<String>,
    pub bundle_id: Option<String>,
    #[serde(default)]
    pub targets: Vec<String>,
}

impl Manifest {
    pub fn parse(contents: &str) -> Result<Self> {
        toml::from_str(contents).context("invalid gpui.toml")
    }
}
