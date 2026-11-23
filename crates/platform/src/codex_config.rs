//! On-disk settings for the Codex provider.

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::agent_policy::{MAX_POLICY_BYTES, valid_capability};

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodexConfig {
    pub executable: PathBuf,
    pub capability: String,
    pub instructions: String,
    pub outputs: BTreeMap<String, ScalarType>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScalarType {
    Boolean,
    Integer,
    String,
}

impl ScalarType {
    pub fn schema_type(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::String => "string",
        }
    }
}

impl CodexConfig {
    pub fn parse(text: &str) -> Result<Self> {
        ensure!(
            text.len() <= MAX_POLICY_BYTES,
            "codex.toml exceeds the {MAX_POLICY_BYTES} byte limit"
        );
        // TOML diagnostics may contain the prompt. Keep them out of logs.
        let config: Self =
            toml::from_str(text).map_err(|_| anyhow!("Invalid codex.toml configuration"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn encode(&self) -> Result<String> {
        self.validate()?;
        let text = toml::to_string(self).map_err(|_| anyhow!("Cannot serialize Codex settings"))?;
        ensure!(
            text.len() <= MAX_POLICY_BYTES,
            "codex.toml exceeds the {MAX_POLICY_BYTES} byte limit"
        );
        Ok(text)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.executable.is_absolute(),
            "Invalid codex.toml · Executable must be an absolute path"
        );
        ensure!(
            valid_capability(&self.capability),
            "Invalid codex.toml · Capability must start with a letter and use ASCII letters, digits, _ or - (up to 96 bytes)"
        );
        ensure!(
            !self.instructions.trim().is_empty() && self.instructions.len() <= 2048,
            "Invalid codex.toml · Instructions require 1–2048 bytes of text"
        );
        ensure!(
            (1..=16).contains(&self.outputs.len())
                && self.outputs.keys().all(|name| {
                    (1..=64).contains(&name.len())
                        && name.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
                        })
                }),
            "Invalid codex.toml · Use 1–16 outputs with alphanumeric, _ or - names (up to 64 bytes)"
        );
        Ok(())
    }
}
