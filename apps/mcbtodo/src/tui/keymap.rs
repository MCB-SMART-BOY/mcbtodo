use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent};
use mcbtodo_platform::WorkspacePaths;
#[cfg(test)]
use mcbtodo_platform::read_regular_file;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum Action {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    keys: BTreeMap<Action, String>,
}

#[derive(Clone, Debug)]
pub(super) struct Keymap {
    keys: BTreeMap<Action, char>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            keys: [
                (Action::Quit, 'q'),
                (Action::Reload, 'r'),
                (Action::Edit, 'e'),
                (Action::MoveLeft, 'h'),
                (Action::MoveRight, 'l'),
                (Action::MoveUp, 'k'),
                (Action::MoveDown, 'j'),
                (Action::Select, 'v'),
                (Action::Space, ' '),
                (Action::Command, ':'),
                (Action::Picker, '/'),
                (Action::Help, '?'),
                (Action::ViewMonth, 'm'),
                (Action::ViewWeek, 'w'),
                (Action::ViewDay, 'd'),
                (Action::ViewAgenda, 'a'),
                (Action::ViewOutline, 'l'),
                (Action::ViewGraph, 'g'),
                (Action::ViewTimeline, 't'),
                (Action::ViewWorkflow, 'f'),
            ]
            .into(),
        }
    }
}

impl Keymap {
    pub(super) fn load_config(paths: &WorkspacePaths) -> Result<Self> {
        let path = paths.keymap_path();
        let contents = match paths.read_config_file(&path) {
            Ok(contents) => contents,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&contents, &path)
    }

    #[cfg(test)]
    pub(super) fn load(path: &Path) -> Result<Self> {
        let contents = match read_regular_file(path) {
            Ok(contents) => contents,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&contents, path)
    }

    fn parse(contents: &str, path: &Path) -> Result<Self> {
        let file: File =
            toml::from_str(contents).with_context(|| format!("parsing {}", path.display()))?;
        let mut keymap = Self::default();
        for (action, value) in file.keys {
            let mut chars = value.chars();
            let Some(character) = chars.next() else {
                bail!("key binding for {action:?} must be one printable character");
            };
            if chars.next().is_some() || character.is_control() {
                bail!("key binding for {action:?} must be one printable character");
            }
            keymap.keys.insert(action, character);
        }
        for views_menu in [false, true] {
            let mut assigned = BTreeMap::new();
            for (action, character) in &keymap.keys {
                if is_view_action(*action) != views_menu {
                    continue;
                }
                if let Some(previous) = assigned.insert(character, action) {
                    bail!("key {character:?} is assigned to both {previous:?} and {action:?}");
                }
            }
        }
        for (action, character) in &keymap.keys {
            if is_view_action(*action) {
                continue;
            }
            if matches!((character, action), ('v', Action::Select))
                || !matches!(character, 'x' | 'a' | 'd' | 'v')
            {
                continue;
            }
            bail!("key {character:?} for {action:?} conflicts with a fixed Graph operation");
        }
        Ok(keymap)
    }

    fn configured_char(&self, character: char, views_menu: bool) -> Option<Action> {
        self.keys.iter().find_map(|(action, configured)| {
            (is_view_action(*action) == views_menu && *configured == character).then_some(*action)
        })
    }

    pub(super) fn normalize(&self, key: KeyEvent, views_menu: bool) -> KeyEvent {
        let KeyCode::Char(character) = key.code else {
            return key;
        };
        let Some(action) = self.configured_char(character, views_menu) else {
            return key;
        };
        let canonical = match action {
            Action::Quit => Some('q'),
            Action::Reload => Some('r'),
            Action::Edit => Some('e'),
            Action::MoveLeft => Some('h'),
            Action::MoveRight => Some('l'),
            Action::MoveUp => Some('k'),
            Action::MoveDown => Some('j'),
            Action::Select => Some('v'),
            Action::Space => Some(' '),
            Action::Command => Some(':'),
            Action::Picker => Some('/'),
            Action::Help => Some('?'),
            Action::ViewMonth if views_menu => Some('m'),
            Action::ViewWeek if views_menu => Some('w'),
            Action::ViewDay if views_menu => Some('d'),
            Action::ViewAgenda if views_menu => Some('a'),
            Action::ViewOutline if views_menu => Some('l'),
            Action::ViewGraph if views_menu => Some('g'),
            Action::ViewTimeline if views_menu => Some('t'),
            Action::ViewWorkflow if views_menu => Some('f'),
            _ => None,
        };
        canonical.map_or(key, |character| KeyEvent {
            code: KeyCode::Char(character),
            ..key
        })
    }
}

fn is_view_action(action: Action) -> bool {
    matches!(
        action,
        Action::ViewMonth
            | Action::ViewWeek
            | Action::ViewDay
            | Action::ViewAgenda
            | Action::ViewOutline
            | Action::ViewGraph
            | Action::ViewTimeline
            | Action::ViewWorkflow
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn loads_partial_bindings_and_keeps_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keymap.toml");
        fs::write(&path, "[keys]\nquit = '0'\nview_month = '1'\n").unwrap();
        let keymap = Keymap::load(&path).unwrap();

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
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keymap.toml");
        for contents in ["[keys]\nunknown = 'x'\n", "[keys]\nquit = 'xx'\n"] {
            fs::write(&path, contents).unwrap();
            assert!(Keymap::load(&path).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlinked_keymap() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("outside.toml");
        let path = dir.path().join("keymap.toml");
        fs::write(&target, "[keys]\nquit = '0'\n").unwrap();
        symlink(target, &path).unwrap();

        assert!(Keymap::load(&path).is_err());
    }

    #[test]
    fn rejects_collisions_within_the_same_input_context() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keymap.toml");
        fs::write(&path, "[keys]\nquit = '0'\nreload = '0'\n").unwrap();

        assert!(
            Keymap::load(&path)
                .unwrap_err()
                .to_string()
                .contains("assigned")
        );
    }

    #[test]
    fn rejects_bindings_that_hide_graph_operations() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("keymap.toml");
        for (action, key) in [("move_left", 'x'), ("select", 'a'), ("quit", 'd')] {
            fs::write(&path, format!("[keys]\n{action} = '{key}'\n")).unwrap();
            assert!(
                Keymap::load(&path)
                    .unwrap_err()
                    .to_string()
                    .contains("Graph")
            );
        }
    }
}
