use std::{borrow::Cow, collections::BTreeMap};

use anyhow::{Result, bail, ensure};
use crossterm::event::KeyCode;
use mcbtodo_execution::codex_config::{CodexConfig, ScalarType};
use mcbtodo_platform::{ConfigSnapshot, WorkspacePaths};

const LABELS: [&str; 4] = ["Executable", "Capability", "Instructions", "Outputs"];
const HELP: [&str; 4] = [
    "Absolute executable path",
    "Workflow capability name",
    "Instructions sent to the provider",
    "name=type, name=type · string, integer, boolean",
];

pub(in crate::tui) struct CodexForm {
    original: ConfigSnapshot<Option<CodexConfig>>,
    pub(in crate::tui) row: usize,
    values: [String; LABELS.len()],
}

impl CodexForm {
    pub(in crate::tui) fn load(paths: &WorkspacePaths) -> Result<Self> {
        let original = paths.read_config_snapshot(paths.codex_provider_path(), |text, _| {
            CodexConfig::parse(text).map(Some)
        })?;
        let values = original.value().ok().and_then(Option::as_ref).map_or_else(
            Default::default,
            |config| {
                [
                    config.executable.to_string_lossy().into_owned(),
                    config.capability.clone(),
                    config.instructions.clone(),
                    config
                        .outputs
                        .iter()
                        .map(|(name, kind)| format!("{name}={}", kind.schema_type()))
                        .collect::<Vec<_>>()
                        .join(", "),
                ]
            },
        );
        Ok(Self {
            original,
            row: 0,
            values,
        })
    }

    pub(in crate::tui) fn replaces_invalid_file(&self) -> bool {
        self.original.value().is_err()
    }

    pub(in crate::tui) fn help(&self) -> &'static str {
        HELP[self.row]
    }

    pub(in crate::tui) fn rows(&self) -> impl Iterator<Item = (&'static str, Cow<'_, str>)> {
        LABELS.into_iter().zip(self.values.iter().map(|value| {
            if value.chars().any(char::is_control) {
                Cow::Owned(value.escape_debug().to_string())
            } else {
                Cow::Borrowed(value.as_str())
            }
        }))
    }

    pub(in crate::tui) fn save(&self, paths: &WorkspacePaths) -> Result<bool> {
        let [executable, capability, instructions, output_text] = &self.values;
        let mut outputs = BTreeMap::new();
        for output in output_text
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let Some((name, kind)) = output.split_once('=') else {
                bail!("Outputs use name=type");
            };
            let kind = match kind.trim() {
                "string" => ScalarType::String,
                "integer" => ScalarType::Integer,
                "boolean" => ScalarType::Boolean,
                _ => bail!("Output types are string, integer or boolean"),
            };
            ensure!(
                outputs.insert(name.trim().to_owned(), kind).is_none(),
                "Output names must be unique"
            );
        }
        let config = CodexConfig {
            executable: executable.into(),
            capability: capability.clone(),
            instructions: instructions.clone(),
            outputs,
        };
        self.original.save(paths, &Some(config), |config| {
            config.as_ref().expect("save supplies a draft").encode()
        })
    }

    pub(super) fn handle(&mut self, key: KeyCode) {
        match key {
            KeyCode::Tab | KeyCode::Down => self.row = (self.row + 1) % LABELS.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.row = (self.row + LABELS.len() - 1) % LABELS.len();
            }
            KeyCode::Backspace => {
                self.values[self.row].pop();
            }
            KeyCode::Delete => self.values[self.row].clear(),
            KeyCode::Char(character) if !character.is_control() => {
                self.values[self.row].push(character);
            }
            _ => {}
        }
    }
}
