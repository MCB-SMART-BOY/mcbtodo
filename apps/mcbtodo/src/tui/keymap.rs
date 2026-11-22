use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent};
use mcbtodo_platform::{ConfigSnapshot, WorkspacePaths};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Action {
    Quit,
    Reload,
    Edit,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    Select,
    Space,
    Command,
    Picker,
    Help,
    ViewMonth,
    ViewWeek,
    ViewDay,
    ViewAgenda,
    ViewOutline,
    ViewGraph,
    ViewTimeline,
    ViewWorkflow,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    keys: BTreeMap<Action, char>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Keymap {
    keys: BTreeMap<Action, char>,
}

type Binding = (Action, char, &'static str);

const MAIN_BINDINGS: &[Binding] = &[
    (Action::Quit, 'q', "Quit"),
    (Action::Reload, 'r', "Reload"),
    (Action::Edit, 'e', "Edit document"),
    (Action::MoveLeft, 'h', "Move left"),
    (Action::MoveRight, 'l', "Move right"),
    (Action::MoveUp, 'k', "Move up"),
    (Action::MoveDown, 'j', "Move down"),
    (Action::Select, 'v', "Select"),
    (Action::Space, ' ', "Actions menu"),
    (Action::Command, ':', "Command"),
    (Action::Picker, '/', "Search"),
    (Action::Help, '?', "Help"),
];

const VIEW_BINDINGS: &[Binding] = &[
    (Action::ViewMonth, 'm', "Month view"),
    (Action::ViewWeek, 'w', "Week view"),
    (Action::ViewDay, 'd', "Day view"),
    (Action::ViewAgenda, 'a', "Agenda view"),
    (Action::ViewOutline, 'l', "Outline view"),
    (Action::ViewGraph, 'g', "Graph view"),
    (Action::ViewTimeline, 't', "Timeline view"),
    (Action::ViewWorkflow, 'f', "Workflow view"),
];

const fn bindings(views_menu: bool) -> &'static [Binding] {
    if views_menu {
        VIEW_BINDINGS
    } else {
        MAIN_BINDINGS
    }
}

fn all_bindings() -> impl Iterator<Item = &'static Binding> {
    MAIN_BINDINGS.iter().chain(VIEW_BINDINGS)
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            keys: all_bindings()
                .map(|&(action, default, _)| (action, default))
                .collect(),
        }
    }
}

impl Keymap {
    pub(super) fn label(&self, action: Action) -> String {
        match self.keys[&action] {
            ' ' => "Space".into(),
            character => character.to_string(),
        }
    }

    pub(super) fn vertical_label(&self) -> String {
        format!(
            "{}{}",
            self.label(Action::MoveDown),
            self.label(Action::MoveUp)
        )
    }

    pub(super) fn horizontal_label(&self) -> String {
        format!(
            "{}{}",
            self.label(Action::MoveLeft),
            self.label(Action::MoveRight)
        )
    }

    pub(super) fn workflow_confirmation_key(&self) -> KeyCode {
        if self.canonical_char('y', false).is_some() {
            KeyCode::Enter
        } else {
            KeyCode::Char('y')
        }
    }

    pub(super) fn load_config(paths: &WorkspacePaths) -> Result<Self> {
        Self::read_snapshot(paths)?.into_value()
    }

    pub(super) fn read_snapshot(paths: &WorkspacePaths) -> Result<ConfigSnapshot<Self>> {
        paths.read_config_snapshot(paths.keymap_path(), Self::parse)
    }

    #[cfg(test)]
    pub(super) fn from_test_config(contents: &str) -> Self {
        Self::parse(contents, Path::new("keymap.toml")).unwrap()
    }

    fn parse(contents: &str, path: &Path) -> Result<Self> {
        let file: File =
            toml::from_str(contents).with_context(|| format!("parsing {}", path.display()))?;
        let mut keymap = Self::default();
        keymap.keys.extend(file.keys);
        keymap.validate()?;
        Ok(keymap)
    }

    fn validate(&self) -> Result<()> {
        for context in [MAIN_BINDINGS, VIEW_BINDINGS] {
            let mut assigned = BTreeMap::new();
            for &(action, _, label) in context {
                let character = self.keys[&action];
                if character.is_control() {
                    bail!("{label} must be bound to a printable character");
                }
                if let Some(previous) = assigned.insert(character, label) {
                    bail!("{character:?} assigned to {previous} and {label}");
                }
            }
        }
        for &(action, _, label) in MAIN_BINDINGS {
            let character = self.keys[&action];
            if matches!(character, 'x' | 'a' | 'd' | 'v')
                && (character, action) != ('v', Action::Select)
            {
                bail!("{character:?} reserved for Graph ({label})");
            }
        }
        Ok(())
    }

    pub(super) const SHORTCUT_COUNT: usize = MAIN_BINDINGS.len() + VIEW_BINDINGS.len();

    pub(super) fn shortcut_labels(&self) -> impl Iterator<Item = (&'static str, String)> {
        all_bindings().map(|&(action, _, label)| (label, self.label(action)))
    }

    pub(super) fn set_shortcut(&mut self, row: usize, character: Option<char>) {
        let &(action, default, _) = all_bindings().nth(row).expect("shortcut row");
        self.keys.insert(action, character.unwrap_or(default));
    }

    pub(super) fn to_config(&self) -> Result<String> {
        self.validate()?;
        let keys = all_bindings()
            .filter_map(|&(action, default, _)| {
                let character = self.keys[&action];
                (character != default).then_some((action, character))
            })
            .collect();
        toml::to_string(&File { keys }).context("serializing keymap")
    }

    fn canonical_char(&self, character: char, views_menu: bool) -> Option<char> {
        bindings(views_menu)
            .iter()
            .find_map(|&(action, canonical, _)| {
                (self.keys[&action] == character).then_some(canonical)
            })
    }

    pub(super) fn normalize(&self, key: KeyEvent, views_menu: bool) -> KeyEvent {
        let KeyCode::Char(character) = key.code else {
            return key;
        };
        KeyEvent {
            code: KeyCode::Char(
                self.canonical_char(character, views_menu)
                    .unwrap_or(character),
            ),
            ..key
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn workspace() -> (TempDir, WorkspacePaths) {
        let dir = TempDir::new().unwrap();
        let paths = WorkspacePaths::from_home(dir.path().to_owned());
        paths.initialize().unwrap();
        (dir, paths)
    }

    #[test]
    fn loads_partial_bindings_and_keeps_defaults() {
        let (_dir, paths) = workspace();
        fs::write(
            paths.keymap_path(),
            "[keys]\nquit = '0'\nview_month = '1'\n",
        )
        .unwrap();
        let keymap = Keymap::load_config(&paths).unwrap();

        assert_eq!(
            keymap
                .normalize(KeyEvent::from(KeyCode::Char('0')), false)
                .code,
            KeyCode::Char('q')
        );
        assert_eq!(
            keymap
                .normalize(KeyEvent::from(KeyCode::Char('1')), true)
                .code,
            KeyCode::Char('m')
        );
        assert_eq!(
            keymap
                .normalize(KeyEvent::from(KeyCode::Char('w')), true)
                .code,
            KeyCode::Char('w')
        );
    }

    #[test]
    fn rejects_unknown_actions_and_multi_character_bindings() {
        let (_dir, paths) = workspace();
        for contents in [
            "[keys]\nunknown = 'x'\n",
            "[keys]\nquit = 'xx'\n",
            "[keys]\nquit = ''\n",
            "[keys]\nquit = \"\\n\"\n",
        ] {
            fs::write(paths.keymap_path(), contents).unwrap();
            assert!(Keymap::load_config(&paths).is_err(), "{contents:?}");
        }
    }

    #[test]
    fn unicode_shortcut_uses_the_same_format_when_saved_and_loaded() -> Result<()> {
        let (_dir, paths) = workspace();
        let snapshot = Keymap::read_snapshot(&paths)?;
        let mut draft = snapshot.value().unwrap().clone();
        draft.set_shortcut(0, Some('退'));

        assert!(snapshot.save(&paths, &draft, Keymap::to_config)?);
        assert!(fs::read_to_string(paths.keymap_path())?.contains("quit = \"退\""));
        assert_eq!(Keymap::load_config(&paths)?, draft);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlinked_keymap() {
        use std::os::unix::fs::symlink;

        let (dir, paths) = workspace();
        let target = dir.path().join("outside.toml");
        fs::write(&target, "[keys]\nquit = '0'\n").unwrap();
        symlink(target, paths.keymap_path()).unwrap();

        assert!(Keymap::load_config(&paths).is_err());
    }

    #[test]
    fn rejects_collisions_within_the_same_input_context() {
        let (_dir, paths) = workspace();
        fs::write(paths.keymap_path(), "[keys]\nquit = '0'\nreload = '0'\n").unwrap();

        assert!(
            Keymap::load_config(&paths)
                .unwrap_err()
                .to_string()
                .contains("assigned")
        );
    }

    #[test]
    fn rejects_bindings_that_hide_graph_operations() {
        let (_dir, paths) = workspace();
        for (action, key) in [("move_left", 'x'), ("select", 'a'), ("quit", 'd')] {
            fs::write(paths.keymap_path(), format!("[keys]\n{action} = '{key}'\n")).unwrap();
            assert!(
                Keymap::load_config(&paths)
                    .unwrap_err()
                    .to_string()
                    .contains("Graph")
            );
        }
    }
}
