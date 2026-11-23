use std::borrow::Cow;

use crossterm::event::KeyCode;
use mcbtodo_platform::{
    AppConfig, AppConfigSnapshot, ConfigConflict, ConfigSnapshot, HomeViewPolicy, TuiTheme,
    WorkerSlots, WorkspacePaths,
};

use super::{keymap::Keymap, move_row};
use crate::agent_discovery::Discovery;
#[cfg(target_os = "linux")]
use mcbtodo_platform::agent_policy::{AgentBackend, AgentPolicyEdit, AgentPolicySnapshot};

#[cfg(target_os = "linux")]
mod codex;
#[cfg(target_os = "linux")]
pub(super) use codex::CodexForm;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Field {
    Timezone,
    HomeView,
    Theme,
    #[cfg(target_os = "linux")]
    Workers,
}

impl Field {
    pub(super) const ALL: &'static [Self] = &[
        Self::Timezone,
        Self::HomeView,
        Self::Theme,
        #[cfg(target_os = "linux")]
        Self::Workers,
    ];
}

const THEMES: [(TuiTheme, &str); 3] = [
    (TuiTheme::Default, "Default"),
    (TuiTheme::HighContrast, "High contrast"),
    (TuiTheme::Monochrome, "Monochrome"),
];

pub(super) struct Form {
    pub(super) preferences: anyhow::Result<Preferences>,
    pub(super) subpage: Option<Subpage>,
}

pub(super) struct Preferences {
    original: AppConfigSnapshot,
    selected: usize,
    home_view: HomeViewPolicy,
    theme: TuiTheme,
    timezone: String,
    worker_slots: WorkerSlots,
}

pub(super) enum Subpage {
    Keyboard(anyhow::Result<ShortcutForm>),
    Agents(AgentPage),
    #[cfg(target_os = "linux")]
    Codex(anyhow::Result<CodexForm>),
}

pub(super) struct AgentPage {
    pub(super) discovery: Discovery,
    pub(super) row: usize,
    #[cfg(target_os = "linux")]
    original: anyhow::Result<AgentPolicySnapshot>,
    #[cfg(target_os = "linux")]
    endpoint: Option<String>,
}

pub(super) enum Input {
    Continue,
    Close,
    Command(Command),
}

impl Input {
    pub(super) fn editor_key(key: KeyCode) -> Option<Self> {
        match key {
            KeyCode::Esc => Some(Self::Close),
            KeyCode::F(5) => Some(Self::Command(Command::Reload)),
            KeyCode::Enter => Some(Self::Command(Command::Save)),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    OpenKeyboard,
    OpenAgents,
    #[cfg(target_os = "linux")]
    OpenCodex,
    Reload,
    Save,
}

pub(super) fn save_error(error: &anyhow::Error) -> String {
    if error.is::<ConfigConflict>() {
        format!("{error} · F5 reload")
    } else {
        error.to_string()
    }
}

pub(super) fn reload_feedback(result: anyhow::Result<()>) -> String {
    match result {
        Ok(()) => "Settings reloaded".into(),
        Err(error) => format!("Cannot reload settings · {error:#}"),
    }
}

impl Form {
    pub(super) fn load(paths: &WorkspacePaths) -> Self {
        Self {
            preferences: Preferences::load(paths),
            subpage: None,
        }
    }

    /// Discard only the current page's draft after a successful read.
    pub(super) fn reload(&mut self, paths: &WorkspacePaths) -> anyhow::Result<()> {
        match &mut self.subpage {
            Some(Subpage::Keyboard(shortcuts)) => {
                *shortcuts = Ok(ShortcutForm {
                    row: shortcuts.as_ref().map_or(0, |draft| draft.row),
                    ..ShortcutForm::load(paths)?
                });
            }
            Some(Subpage::Agents(page)) => page.reload(paths)?,
            #[cfg(target_os = "linux")]
            Some(Subpage::Codex(form)) => {
                let mut reloaded = CodexForm::load(paths)?;
                reloaded.row = form.as_ref().map_or(0, |draft| draft.row);
                *form = Ok(reloaded);
            }
            None => match &mut self.preferences {
                Ok(preferences) => preferences.reload(paths)?,
                Err(_) => self.preferences = Ok(Preferences::load(paths)?),
            },
        }
        Ok(())
    }

    pub(super) fn handle(&mut self, key: KeyCode) -> Input {
        if let Some(input) = Input::editor_key(key) {
            #[cfg(target_os = "linux")]
            if matches!(input, Input::Close)
                && let Some(Subpage::Agents(page)) = &mut self.subpage
                && page.endpoint.take().is_some()
            {
                return Input::Continue;
            }
            return match input {
                #[cfg(target_os = "linux")]
                Input::Close if matches!(self.subpage, Some(Subpage::Codex(_))) => {
                    Input::Command(Command::OpenAgents)
                }
                Input::Close if self.subpage.take().is_some() => Input::Continue,
                Input::Command(Command::Save) => match &self.subpage {
                    Some(Subpage::Keyboard(Ok(_))) => input,
                    #[cfg(target_os = "linux")]
                    Some(Subpage::Agents(page))
                        if page.can_save_default() || page.endpoint.is_some() =>
                    {
                        input
                    }
                    #[cfg(target_os = "linux")]
                    Some(Subpage::Codex(Ok(_))) => input,
                    None if self.preferences.is_ok() => input,
                    _ => Input::Continue,
                },
                _ => input,
            };
        }
        match (&mut self.subpage, key) {
            #[cfg(target_os = "linux")]
            (Some(Subpage::Agents(page)), KeyCode::F(4)) if page.endpoint.is_none() => {
                return Input::Command(Command::OpenCodex);
            }
            #[cfg(target_os = "linux")]
            (Some(Subpage::Codex(form)), _) => {
                if let Ok(draft) = form {
                    draft.handle(key);
                }
            }
            (Some(Subpage::Agents(page)), _) => page.handle(key),
            (Some(Subpage::Keyboard(Ok(shortcuts))), _) => shortcuts.handle(key),
            (Some(Subpage::Keyboard(Err(_))), _) => {}
            (None, KeyCode::F(2)) => return Input::Command(Command::OpenKeyboard),
            (None, KeyCode::F(3)) => return Input::Command(Command::OpenAgents),
            (None, _) => {
                if let Ok(preferences) = &mut self.preferences {
                    preferences.edit(key);
                }
            }
        }
        Input::Continue
    }
}

impl Preferences {
    fn load(paths: &WorkspacePaths) -> anyhow::Result<Self> {
        Ok(Self::from_snapshot(paths.read_app_config_snapshot()?))
    }

    pub(super) fn from_snapshot(original: AppConfigSnapshot) -> Self {
        let config = original.preferences().unwrap_or_default();
        Self {
            original,
            selected: 0,
            home_view: config.home_view,
            theme: config.tui_theme,
            timezone: config.timezone.to_string(),
            worker_slots: config.worker_slots,
        }
    }

    pub(super) fn replaces_invalid_file(&self) -> bool {
        self.original.preferences().is_none()
    }

    pub(super) fn selected(&self) -> usize {
        self.selected
    }

    pub(super) fn field(&self) -> Field {
        Field::ALL[self.selected]
    }

    pub(super) fn row(&self, field: Field) -> (&'static str, Cow<'_, str>) {
        match field {
            Field::Timezone => ("TZ", self.timezone.as_str().into()),
            Field::HomeView => (
                "Home",
                match self.home_view {
                    HomeViewPolicy::Month => "Month",
                    HomeViewPolicy::RestoreLastView => "Restore last view",
                }
                .into(),
            ),
            Field::Theme => ("Theme", THEMES[self.theme_index()].1.into()),
            #[cfg(target_os = "linux")]
            Field::Workers => ("Worker slots", self.worker_slots.get().to_string().into()),
        }
    }

    pub(super) fn worker_limit_changed(&self) -> bool {
        self.original.preferences().unwrap_or_default().worker_slots != self.worker_slots
    }

    pub(super) fn save(&mut self, paths: &WorkspacePaths) -> anyhow::Result<(AppConfig, bool)> {
        let timezone = self.timezone.trim().parse().map_err(|_| {
            self.selected = 0;
            anyhow::anyhow!("Invalid IANA timezone")
        })?;
        let config = AppConfig {
            timezone,
            home_view: self.home_view,
            tui_theme: self.theme,
            worker_slots: self.worker_slots,
        };
        let written = paths.save_config(&self.original, config)?;
        Ok((config, written))
    }

    pub(super) fn reload(&mut self, paths: &WorkspacePaths) -> anyhow::Result<()> {
        *self = Self {
            selected: self.selected,
            ..Self::load(paths)?
        };
        Ok(())
    }

    pub(super) fn edit(&mut self, key: KeyCode) {
        match (key, self.field()) {
            (KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down, _) => {
                let direction = if matches!(key, KeyCode::Tab | KeyCode::Down) {
                    1
                } else {
                    Field::ALL.len() - 1
                };
                self.selected = (self.selected + direction) % Field::ALL.len();
            }
            (KeyCode::Left | KeyCode::Right | KeyCode::Char(' '), Field::Theme) => {
                let direction = if key == KeyCode::Left {
                    THEMES.len() - 1
                } else {
                    1
                };
                self.theme = THEMES[(self.theme_index() + direction) % THEMES.len()].0;
            }
            (KeyCode::Left | KeyCode::Right | KeyCode::Char(' '), Field::HomeView) => {
                self.home_view = match self.home_view {
                    HomeViewPolicy::Month => HomeViewPolicy::RestoreLastView,
                    HomeViewPolicy::RestoreLastView => HomeViewPolicy::Month,
                };
            }
            #[cfg(target_os = "linux")]
            (KeyCode::Left | KeyCode::Right, Field::Workers) => {
                let current = self.worker_slots.get();
                let next = if key == KeyCode::Left {
                    current - 1
                } else {
                    current + 1
                };
                if let Ok(slots) = WorkerSlots::try_from(next) {
                    self.worker_slots = slots;
                }
            }
            (KeyCode::Backspace, Field::Timezone) => {
                self.timezone.pop();
            }
            (KeyCode::Delete, Field::Timezone) => self.timezone.clear(),
            (KeyCode::Char(character), Field::Timezone) if !character.is_control() => {
                self.timezone.push(character);
            }
            _ => {}
        }
    }

    fn theme_index(&self) -> usize {
        match self.theme {
            TuiTheme::Default => 0,
            TuiTheme::HighContrast => 1,
            TuiTheme::Monochrome => 2,
        }
    }
}

impl AgentPage {
    pub(super) fn load(paths: &WorkspacePaths) -> Self {
        #[cfg(target_os = "linux")]
        let original = paths.read_agent_policy_snapshot();
        #[cfg(target_os = "linux")]
        let discovery =
            Discovery::from_policy(original.as_ref().and_then(AgentPolicySnapshot::policy));
        #[cfg(not(target_os = "linux"))]
        let discovery = Discovery::inspect(paths);
        Self {
            row: discovery
                .providers
                .iter()
                .position(|provider| provider.is_default)
                .unwrap_or(0),
            discovery,
            #[cfg(target_os = "linux")]
            original,
            #[cfg(target_os = "linux")]
            endpoint: None,
        }
    }

    fn reload(&mut self, paths: &WorkspacePaths) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        if self.endpoint.is_some() {
            let mut refreshed = Self::load(paths);
            let backend = self.discovery.providers[self.row].backend;
            if !refreshed.select(backend) {
                let message = if refreshed.discovery.providers.is_empty() {
                    refreshed.discovery.policy.message()
                } else {
                    "Selected Agent provider is no longer configured"
                };
                anyhow::bail!("{message}");
            }
            refreshed.start_endpoint_edit();
            *self = refreshed;
            return Ok(());
        }
        *self = Self::load(paths);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn select(&mut self, backend: AgentBackend) -> bool {
        if let Some(row) = self
            .discovery
            .providers
            .iter()
            .position(|p| p.backend == backend)
        {
            self.row = row;
            true
        } else {
            false
        }
    }

    #[cfg(target_os = "linux")]
    fn start_endpoint_edit(&mut self) {
        let Some(provider) = self.discovery.providers.get(self.row) else {
            return;
        };
        self.endpoint = self
            .original
            .as_ref()
            .ok()
            .and_then(|snapshot| snapshot.policy().ok().flatten())
            .and_then(|policy| policy.provider_socket(provider.backend))
            .and_then(std::path::Path::to_str)
            .map(str::to_owned);
    }

    #[cfg(target_os = "linux")]
    pub(super) fn endpoint_draft(&self) -> Option<(&'static str, &str)> {
        self.endpoint
            .as_deref()
            .map(|endpoint| (self.discovery.providers[self.row].backend.name(), endpoint))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn can_save_default(&self) -> bool {
        self.discovery.providers.len() > 1
    }

    #[cfg(target_os = "linux")]
    pub(super) fn inherited_grants(&self) -> usize {
        self.original
            .as_ref()
            .ok()
            .and_then(|snapshot| snapshot.policy().ok().flatten())
            .map_or(0, |policy| policy.inherited_grant_count())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn save(&mut self, paths: &WorkspacePaths) -> anyhow::Result<&'static str> {
        let snapshot = self
            .original
            .as_ref()
            .expect("the Agents page has configured providers");
        let backend = self.discovery.providers[self.row].backend;
        let (edit, saved, unchanged) = match self.endpoint.as_deref() {
            Some(path) => (
                AgentPolicyEdit::Endpoint { backend, path },
                "Agent endpoint saved · Used on the next attempt",
                "Agent endpoint unchanged",
            ),
            None => (
                AgentPolicyEdit::Default(backend),
                "Default provider saved · Used on the next attempt",
                "Default provider unchanged",
            ),
        };
        let written = paths.save_agent_policy(snapshot, edit)?;
        self.endpoint = None;
        if written {
            *self = Self::load(paths);
            self.select(backend);
            Ok(saved)
        } else {
            Ok(unchanged)
        }
    }

    fn handle(&mut self, key: KeyCode) {
        #[cfg(target_os = "linux")]
        if let Some(endpoint) = &mut self.endpoint {
            match key {
                KeyCode::Backspace => {
                    endpoint.pop();
                }
                KeyCode::Delete => endpoint.clear(),
                KeyCode::Char(character) if !character.is_control() => endpoint.push(character),
                _ => {}
            }
            return;
        }
        let rows = self.discovery.providers.len().max(1);
        match key {
            #[cfg(target_os = "linux")]
            KeyCode::F(6) => self.start_endpoint_edit(),
            KeyCode::Home => self.row = 0,
            KeyCode::End => self.row = rows - 1,
            KeyCode::Down | KeyCode::Up => {
                self.row = move_row(self.row, key == KeyCode::Down, rows);
            }
            _ => {}
        }
    }
}

pub(super) struct ShortcutForm {
    original: ConfigSnapshot<Keymap>,
    pub(super) row: usize,
    pub(super) draft: Keymap,
}

impl ShortcutForm {
    pub(super) fn load(paths: &WorkspacePaths) -> anyhow::Result<Self> {
        let original = Keymap::read_snapshot(paths)?;
        let draft = original.value().cloned().unwrap_or_default();
        Ok(Self {
            original,
            row: 0,
            draft,
        })
    }

    pub(super) fn replaces_invalid_file(&self) -> bool {
        self.original.value().is_err()
    }

    pub(super) fn save(&self, paths: &WorkspacePaths) -> anyhow::Result<bool> {
        self.original.save(paths, &self.draft, Keymap::to_config)
    }

    fn handle(&mut self, key: KeyCode) {
        match key {
            KeyCode::Home => self.row = 0,
            KeyCode::End => self.row = Keymap::SHORTCUT_COUNT - 1,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                self.row = move_row(
                    self.row,
                    matches!(key, KeyCode::Tab | KeyCode::Down),
                    Keymap::SHORTCUT_COUNT,
                );
            }
            KeyCode::Delete | KeyCode::Backspace => self.draft.set_shortcut(self.row, None),
            KeyCode::Char(character) if !character.is_control() => {
                self.draft.set_shortcut(self.row, Some(character));
            }
            _ => {}
        }
    }
}
