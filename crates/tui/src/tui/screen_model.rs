use std::borrow::Cow;

use chrono::{Datelike, Days, NaiveDate};
use chrono_tz::Tz;
use mcbtodo_application::{CalendarItem, InboxSummary, OutlineItem};
use mcbtodo_domain::CalendarKind;
use ratatui::text::Line;

use super::{
    Overlay, State, View,
    calendar::{self, Lane, Week},
    creation, deadline, picker,
    review::ReviewViewport,
    screen_data::{ScreenData, ViewRows},
    settings, timeline,
};
use mcbtodo_client::calendar_format;

mod dependency_graph;
mod proposals;
pub(super) mod shortcuts;
mod time_block_form;
mod workflow;
pub(super) use dependency_graph::{GraphEditScreenModel, GraphScreenModel};
pub(super) use proposals::{ProposalPanel, ProposalScreenModel};
pub(super) use time_block_form::{TimeBlockPanel, TimeBlockScreenModel};
pub(super) use workflow::{WorkflowConfirmation, WorkflowPanel, WorkflowScreenModel};

/// Project only the active screen. The renderer receives display data
/// without borrowing the Vault or the input state.
pub(super) struct ScreenModel<'a> {
    pub(super) content: Content<'a>,
    pub(super) chrome: ScreenChrome,
}

pub(super) struct ScreenChrome {
    pub(super) title: &'static str,
    pub(super) context: String,
    pub(super) subtitle: String,
    pub(super) mode: String,
    pub(super) active_mode: bool,
    pub(super) feedback: String,
    pub(super) warning: Option<String>,
    pub(super) footer: ScreenFooter,
}

pub(super) enum ScreenFooter {
    Main(shortcuts::MainHints),
    Time { can_add_block: bool },
    Command(String),
    Panel,
}

impl ScreenFooter {
    pub(super) fn timer_hint(width: u16, can_add_block: bool) -> &'static str {
        let (wide, compact) = if can_add_block {
            (
                "Esc  b add time block  i status  s start  p pause  x stop",
                "Esc  b block  i/s/p/x",
            )
        } else {
            (
                "Esc  i status  s start  p pause  x stop",
                "Esc  i/s/p/x timer",
            )
        };
        [wide, compact, "Esc  i/s/p/x", "Esc back", "Esc"]
            .into_iter()
            .find(|hint| hint.len() < usize::from(width))
            .unwrap_or_default()
    }
}

impl ScreenChrome {
    pub(super) fn project(state: &State, timezone: Tz) -> Self {
        let mode = match &state.overlay {
            Overlay::Space(_) => "MENU".into(),
            Overlay::Settings(form) => match &form.subpage {
                #[cfg(target_os = "linux")]
                Some(settings::Subpage::Agents(page)) if page.endpoint_draft().is_some() => "EDIT",
                Some(settings::Subpage::Agents(_) | settings::Subpage::Keyboard(Err(_))) => "INFO",
                #[cfg(target_os = "linux")]
                Some(settings::Subpage::Codex(Err(_))) => "INFO",
                None if form.preferences.is_err() => "INFO",
                _ => "EDIT",
            }
            .into(),
            Overlay::TimeBlock(form) if form.unit.is_none() => "PICKER".into(),
            Overlay::Picker { .. } | Overlay::InboxProject { .. } => "PICKER".into(),
            Overlay::Command(_) => "COMMAND".into(),
            Overlay::Views(_) => "VIEWS".into(),
            Overlay::Create(_) => "CREATE".into(),
            Overlay::CreateForm(_) | Overlay::Deadline(_) | Overlay::TimeBlock(_) => {
                "INSERT".into()
            }
            Overlay::Proposals => "REVIEW".into(),
            _ if state.view == View::Graph => state.graph_selection.as_ref().map_or_else(
                || "NORMAL".into(),
                |selection| format!("SELECT · {} nodes", selection.len()),
            ),
            _ if state.view.uses_dates() && state.anchor.is_some() => "SELECT".into(),
            _ => "NORMAL".into(),
        };
        let (start, end) = state.selected_range();
        let (title, context, subtitle) = if let Overlay::Settings(form) = &state.overlay {
            let (title, subtitle) = match &form.subpage {
                Some(settings::Subpage::Keyboard(Ok(shortcuts))) => (
                    "Keyboard",
                    if shortcuts.replaces_invalid_file() {
                        "Invalid keymap · Editing defaults"
                    } else {
                        "Type a key · Applies on save"
                    },
                ),
                Some(settings::Subpage::Keyboard(Err(_))) => {
                    ("Keyboard", "Cannot read keymap.toml")
                }
                #[cfg(target_os = "linux")]
                Some(settings::Subpage::Agents(page)) if page.endpoint_draft().is_some() => {
                    ("Agent endpoint", "Socket relative to app home")
                }
                Some(settings::Subpage::Agents(_)) => ("Agents", "Host policy · Health unchecked"),
                #[cfg(target_os = "linux")]
                Some(settings::Subpage::Codex(Ok(draft))) => ("Codex", draft.help()),
                #[cfg(target_os = "linux")]
                Some(settings::Subpage::Codex(Err(_))) => ("Codex", "Cannot read codex.toml"),
                None => match &form.preferences {
                    Err(_) => ("Settings", "Cannot read config.toml"),
                    Ok(preferences) if preferences.replaces_invalid_file() => {
                        ("Settings", "Invalid config · Editing defaults")
                    }
                    Ok(preferences) => (
                        "Settings",
                        match preferences.field() {
                            settings::Field::Timezone if state.has_timezone_override => {
                                "Session timezone override active"
                            }
                            settings::Field::Timezone => "Applies on restart · Type an IANA name",
                            settings::Field::HomeView => "Applies on restart · ←/→ change",
                            settings::Field::Theme if state.no_color => {
                                "NO_COLOR active · ←/→ change saved palette"
                            }
                            settings::Field::Theme => "Applies on save · ←/→ change",
                            #[cfg(target_os = "linux")]
                            settings::Field::Workers => "Daemon restart required · ←/→ change",
                        },
                    ),
                },
            };
            (title, String::new(), subtitle.into())
        } else if state.view == View::Workflow {
            (state.view.label(), " runs".into(), "Local daemon".into())
        } else if state.view.uses_dates() && start != end {
            (
                state.view.label(),
                format!(
                    "  {}  ·  {start} → {end} · {} days",
                    state.day,
                    (end - start).num_days() + 1
                ),
                timezone.to_string(),
            )
        } else {
            (
                state.view.label(),
                format!("  {}", state.day),
                timezone.to_string(),
            )
        };
        let footer = match &state.overlay {
            Overlay::None => {
                ScreenFooter::Main(shortcuts::MainHints::project(&state.keymap, state.view))
            }
            Overlay::Time => ScreenFooter::Time {
                can_add_block: state.view.uses_dates(),
            },
            Overlay::Command(input) => ScreenFooter::Command(format!(":{input}")),
            _ => ScreenFooter::Panel,
        };
        Self {
            title,
            context,
            subtitle,
            active_mode: mode != "NORMAL",
            mode,
            feedback: display_line(state.message.lines().next().unwrap_or_default()),
            warning: state.vault_warning.as_deref().map(display_line),
            footer,
        }
    }
}

fn display_line(text: &str) -> String {
    text.replace(char::is_control, " ")
}

pub(super) enum Content<'a> {
    Month(MonthScreenModel),
    Week(Box<WeekScreenModel>),
    Day(DayScreenModel),
    Agenda(AgendaScreenModel),
    Outline(OutlineScreenModel),
    Inbox(InboxScreenModel),
    Graph(GraphScreenModel<'a>),
    Timeline(TimelineScreenModel),
    ViewError {
        title: &'static str,
        message: String,
    },
    Workflow(WorkflowScreenModel<'a>),
    Menu {
        title: &'static str,
        viewport: &'a ReviewViewport,
        text: String,
    },
    Settings(SettingsScreenModel<'a>),
    CreateForm(CreationScreenModel<'a>),
    Deadline(DeadlineScreenModel<'a>),
    TimeBlock(TimeBlockScreenModel<'a>),
    GraphEdit(GraphEditScreenModel<'a>),
    Proposals(ProposalScreenModel<'a>),
    Picker(PickerScreenModel),
    InboxProject(ProjectPickerScreenModel),
    Help {
        viewport: &'a ReviewViewport,
        text: String,
    },
}

pub(super) struct SettingsScreenModel<'a> {
    pub(super) panel: &'static str,
    selected: usize,
    rows: Vec<SettingsEntry<'a>>,
    hints: [&'static str; 3],
}

struct SettingsEntry<'a> {
    section: Option<&'static str>,
    label: &'static str,
    value: Cow<'a, str>,
    cursor: bool,
}

pub(super) enum SettingsRow<'model> {
    Section(&'static str),
    Preference {
        label: &'static str,
        value: &'model str,
        focused: bool,
        cursor: bool,
    },
}

impl<'a> SettingsScreenModel<'a> {
    fn project(form: &'a settings::Form) -> Self {
        match &form.subpage {
            Some(settings::Subpage::Agents(page)) => Self::agents(page),
            #[cfg(target_os = "linux")]
            Some(settings::Subpage::Codex(Ok(draft))) => Self::codex(draft),
            #[cfg(target_os = "linux")]
            Some(settings::Subpage::Codex(Err(error))) => Self::read_error(
                " Provider settings ",
                error,
                ["F5 retry  Esc back", "F5 retry Esc", "F5 Esc"],
            ),
            Some(settings::Subpage::Keyboard(Ok(shortcuts))) => Self::keyboard(shortcuts),
            Some(settings::Subpage::Keyboard(Err(error))) => Self::read_error(
                " Bindings ",
                error,
                ["F5 retry  Esc back", "F5 retry Esc", "F5 Esc"],
            ),
            None => match &form.preferences {
                Ok(preferences) => Self::preferences(preferences, false),
                Err(error) => Self::read_error(
                    " Preferences ",
                    error,
                    [
                        "F2 keys  F3 agents  F5 retry  Esc back",
                        "F2 F3 F5 Esc",
                        "F5 Esc",
                    ],
                ),
            },
        }
    }

    #[cfg(target_os = "linux")]
    fn codex(draft: &'a settings::CodexForm) -> Self {
        Self {
            panel: " Provider settings ",
            selected: draft.row,
            rows: draft
                .rows()
                .map(|(label, value)| SettingsEntry {
                    section: None,
                    label,
                    value,
                    cursor: true,
                })
                .collect(),
            hints: if draft.replaces_invalid_file() {
                [
                    "Invalid file · Tab field  F5 reload  Enter replace  Esc cancel",
                    "Tab Enter replace Esc",
                    "Enter Esc",
                ]
            } else {
                [
                    "Tab field  Del clear  F5 reload  Enter save  Esc cancel",
                    "Tab Del F5 Enter Esc",
                    "Enter Esc",
                ]
            },
        }
    }

    fn keyboard(shortcuts: &settings::ShortcutForm) -> Self {
        Self {
            panel: " Bindings ",
            selected: shortcuts.row,
            rows: shortcuts
                .draft
                .shortcut_labels()
                .map(|(label, value)| SettingsEntry {
                    section: None,
                    label,
                    value: Cow::Owned(value),
                    cursor: false,
                })
                .collect(),
            hints: if shortcuts.replaces_invalid_file() {
                [
                    "↑/↓ select  F5 reload  Enter replace  Esc cancel",
                    "↑↓ Enter replace Esc",
                    "Enter Esc",
                ]
            } else {
                [
                    "↑/↓ select  Del reset  F5 reload  Enter save  Esc cancel",
                    "↑↓ Del Enter Esc",
                    "Enter Esc",
                ]
            },
        }
    }

    fn read_error(panel: &'static str, error: &anyhow::Error, hints: [&'static str; 3]) -> Self {
        Self {
            panel,
            selected: 0,
            rows: vec![SettingsEntry {
                section: None,
                label: "Read error",
                value: Cow::Owned(format!("{error:#}")),
                cursor: false,
            }],
            hints,
        }
    }

    fn preferences(draft: &'a settings::Preferences, startup_repair: bool) -> Self {
        use settings::Field::*;
        let hints = if startup_repair && draft.replaces_invalid_file() {
            [
                "Tab field  F5 reload  Enter replace  Esc exit",
                "Enter replace Esc exit",
                "Enter Esc",
            ]
        } else if startup_repair {
            [
                "Tab field  F5 reload  Enter continue  Esc exit",
                "Enter continue Esc exit",
                "Enter Esc",
            ]
        } else if draft.replaces_invalid_file() {
            [
                "F2 keys  F3 agents  F5 reload  Enter replace  Esc cancel",
                "F2 F3 Enter replace Esc",
                "Enter Esc",
            ]
        } else {
            let compact = if draft.field() == Timezone {
                "F2 F3 Del Enter Esc"
            } else {
                "F2 F3 ←/→ Enter Esc"
            };
            [
                "F2 keys  F3 agents  F5 reload  Enter save  Esc cancel",
                compact,
                "Enter Esc",
            ]
        };
        Self {
            panel: " Preferences ",
            selected: draft.selected(),
            rows: settings::Field::ALL
                .iter()
                .copied()
                .map(|field| {
                    let (label, value) = draft.row(field);
                    SettingsEntry {
                        section: match field {
                            Timezone => Some("General"),
                            Theme => Some("Appearance"),
                            HomeView => None,
                            #[cfg(target_os = "linux")]
                            settings::Field::Workers => Some("Daemon"),
                        },
                        label,
                        value,
                        cursor: field == Timezone,
                    }
                })
                .collect(),
            hints,
        }
    }

    fn agents(page: &'a settings::AgentPage) -> Self {
        #[cfg(target_os = "linux")]
        if let Some((provider, endpoint)) = page.endpoint_draft() {
            return Self {
                panel: " Socket path ",
                selected: 0,
                rows: vec![SettingsEntry {
                    section: Some(provider),
                    label: "",
                    value: if endpoint.chars().any(char::is_control) {
                        Cow::Owned(endpoint.escape_debug().to_string())
                    } else {
                        Cow::Borrowed(endpoint)
                    },
                    cursor: true,
                }],
                hints: [
                    "Del clear  F5 reload  Enter save  Esc cancel",
                    "Del F5 Enter Esc",
                    "Enter Esc",
                ],
            };
        }
        let discovery = &page.discovery;
        let rows = if discovery.providers.is_empty() {
            vec![SettingsEntry {
                section: None,
                label: "",
                value: Cow::Borrowed(discovery.policy.message()),
                cursor: false,
            }]
        } else {
            #[cfg(target_os = "linux")]
            {
                discovery
                    .providers
                    .iter()
                    .enumerate()
                    .map(|(row, provider)| {
                        let value = if provider.is_default {
                            format!("Default · {} grants", provider.grant_count)
                        } else if row == page.row {
                            let count = page.inherited_grants();
                            let effect = if count == 1 {
                                "grant moves"
                            } else {
                                "grants move"
                            };
                            format!("{count} {effect} · Enter")
                        } else {
                            format!("{} grants", provider.grant_count)
                        };
                        SettingsEntry {
                            section: None,
                            label: provider.backend.name(),
                            value: Cow::Owned(value),
                            cursor: false,
                        }
                    })
                    .collect()
            }
            #[cfg(not(target_os = "linux"))]
            Vec::new()
        };
        Self {
            panel: " Providers ",
            selected: page.row,
            rows,
            #[cfg(target_os = "linux")]
            hints: if page.can_save_default() {
                [
                    "↑/↓ select  Enter default  F4 Codex  F6 endpoint  Esc back",
                    "Enter default F4 F6 Esc",
                    "Enter F6 Esc",
                ]
            } else if !discovery.providers.is_empty() {
                [
                    "F4 Codex settings  F6 endpoint  F5 reload  Esc back",
                    "F4 Codex F6 F5 Esc",
                    "F4 F6 Esc",
                ]
            } else {
                [
                    "F4 Codex settings  F5 refresh  Esc back",
                    "F4 Codex F5 Esc",
                    "F4 Esc",
                ]
            },
            #[cfg(not(target_os = "linux"))]
            hints: ["Esc back", "Esc", "Esc"],
        }
    }

    pub(super) fn visible_rows(&self, height: u16) -> impl Iterator<Item = SettingsRow<'_>> {
        let height = usize::from(height);
        let section_count = self.rows.iter().filter(|row| row.section.is_some()).count();
        let grouped = height >= self.rows.len() + section_count;
        let start = (self.selected + 1).saturating_sub(height);
        self.rows
            .iter()
            .enumerate()
            .skip(start)
            .take(height)
            .flat_map(move |(index, row)| {
                let focused = index == self.selected;
                let heading = row.section.filter(|_| grouped).map(SettingsRow::Section);
                let preference = SettingsRow::Preference {
                    label: row.label,
                    value: &row.value,
                    focused,
                    cursor: focused && row.cursor,
                };
                heading.into_iter().chain(std::iter::once(preference))
            })
    }

    pub(super) fn hint(&self, width: u16) -> &'static str {
        self.hints
            .into_iter()
            .chain(["Esc"])
            .find(|hint| Line::raw(*hint).width() < usize::from(width))
            .unwrap_or_default()
    }
}

impl<'a> ScreenModel<'a> {
    pub(super) fn main_hint(&self, width: u16, height: u16) -> &str {
        match &self.chrome.footer {
            ScreenFooter::Main(hints) => match &self.content {
                Content::Workflow(workflow) if width < 52 => &workflow.compact_hint,
                Content::Workflow(workflow) => &workflow.hint,
                Content::Graph(graph) => graph
                    .selection_hint(width)
                    .unwrap_or_else(|| hints.visible(width, height, false)),
                Content::Inbox(inbox) => hints.visible(width, height, inbox.rows.is_empty()),
                Content::ViewError { .. } => hints.visible(width, height, true),
                _ => hints.visible(width, height, false),
            },
            ScreenFooter::Time { can_add_block } => ScreenFooter::timer_hint(width, *can_add_block),
            ScreenFooter::Command(input) => input,
            ScreenFooter::Panel if matches!(self.content, Content::ViewError { .. }) => {
                "Esc cancel"
            }
            ScreenFooter::Panel => unreachable!("main view requires its footer projection"),
        }
    }

    pub(super) fn project(state: &'a State, timezone: Tz, data: ScreenData<'a>) -> Self {
        let content = match data {
            ScreenData::Main(Ok(rows)) => Self::project_view(state, timezone, rows),
            ScreenData::Main(Err(error)) => Self::view_error(
                if state.view == View::Outline {
                    " Outline error "
                } else {
                    " Calendar error "
                },
                error,
            ),
            ScreenData::Space(viewport) => Content::Menu {
                title: " Actions ",
                viewport,
                text: shortcuts::space(
                    &state.keymap,
                    state.view.uses_dates() || state.view == View::Outline,
                ),
            },
            ScreenData::Settings(form) => Content::Settings(SettingsScreenModel::project(form)),
            ScreenData::Views(viewport) => Content::Menu {
                title: " Switch view ",
                viewport,
                text: shortcuts::views(&state.keymap, state.view),
            },
            ScreenData::Create(viewport, can_create_task) => {
                let mut options = vec![
                    "p  Project",
                    "e  All-day event",
                    "d  Deadline",
                    "r  Reminder",
                    "b  Time block",
                ];
                if can_create_task {
                    options.insert(1, "t  Task in selected Project");
                }
                Content::Menu {
                    title: " Create ",
                    viewport,
                    text: options.join("\n"),
                }
            }
            ScreenData::CreateForm(form) => {
                Content::CreateForm(CreationScreenModel::from_form(form))
            }
            ScreenData::Deadline(form) => Content::Deadline(DeadlineScreenModel::from_form(form)),
            ScreenData::TimeBlock(form, choices) => {
                Content::TimeBlock(TimeBlockScreenModel::from_form(form, choices, timezone))
            }
            ScreenData::GraphEdit(edit, viewport) => {
                Content::GraphEdit(GraphEditScreenModel::project(edit, viewport))
            }
            ScreenData::Picker(query, row, Ok(matches)) => {
                Content::Picker(PickerScreenModel::documents(query, row, matches))
            }
            ScreenData::Picker(_, _, Err(error)) => Self::view_error(" Picker error ", error),
            ScreenData::InboxProject(projects, row) => {
                Content::InboxProject(ProjectPickerScreenModel {
                    projects: projects
                        .iter()
                        .map(|(_, title)| display_line(title))
                        .collect(),
                    focused: row,
                })
            }
            ScreenData::Proposals => Content::Proposals(ProposalScreenModel::project(
                &state.proposals,
                !state.message.is_empty(),
            )),
            ScreenData::Help(viewport) => Content::Help {
                viewport,
                text: shortcuts::help(&state.keymap).join("\n"),
            },
        };
        Self {
            content,
            chrome: ScreenChrome::project(state, timezone),
        }
    }

    fn view_error(title: &'static str, error: anyhow::Error) -> Content<'a> {
        let mut message = display_line(&error.root_cause().to_string());
        if error.source().is_some() {
            message.push('\n');
            message.push_str(&display_line(&error.to_string()));
        }
        Content::ViewError { title, message }
    }

    pub(super) fn repair_settings(draft: &'a settings::Preferences, feedback: &str) -> Self {
        Self {
            content: Content::Settings(SettingsScreenModel::preferences(draft, true)),
            chrome: ScreenChrome {
                title: "mcbtodo",
                context: " · Settings".into(),
                subtitle: if draft.replaces_invalid_file() {
                    "Replace config.toml to open the Vault"
                } else {
                    "Confirm config.toml to open the Vault"
                }
                .into(),
                mode: String::new(),
                active_mode: false,
                feedback: feedback.into(),
                warning: None,
                footer: ScreenFooter::Panel,
            },
        }
    }

    fn project_view(state: &'a State, timezone: Tz, rows: ViewRows) -> Content<'a> {
        match rows {
            ViewRows::Month(rows) => Content::Month(MonthScreenModel::from_entries(
                state.day,
                state.selected_range(),
                timezone,
                &rows,
            )),
            ViewRows::Week(rows) => Content::Week(Box::new(WeekScreenModel::from_week(
                Week::from_entries(state.day, timezone, rows),
                state.day,
                state.selected_range(),
                state.week_hour,
            ))),
            ViewRows::Day(rows) => Content::Day(DayScreenModel::from_entries(
                state.day, timezone, rows, state.row,
            )),
            ViewRows::Agenda(rows) => {
                Content::Agenda(AgendaScreenModel::from_entries(timezone, rows, state.row))
            }
            ViewRows::Outline(rows) => {
                Content::Outline(OutlineScreenModel::project(&rows, state.row))
            }
            ViewRows::Inbox(rows) => {
                Content::Inbox(InboxScreenModel::project(&rows, timezone, state.row))
            }
            ViewRows::Graph(rows) => Content::Graph(GraphScreenModel::project(
                rows,
                state.row,
                state.graph_selection.as_ref(),
            )),
            ViewRows::Timeline(rows) => Content::Timeline(TimelineScreenModel::project(
                rows, state.day, timezone, state.row,
            )),
            ViewRows::Workflow => Content::Workflow(WorkflowScreenModel::project(
                &state.workflow,
                timezone,
                &state.keymap,
            )),
        }
    }
}

pub(super) struct InboxScreenModel {
    pub(super) rows: Vec<InboxRow>,
    pub(super) focused: usize,
}

pub(super) struct InboxRow {
    pub(super) title: String,
    pub(super) metadata: String,
}

impl InboxScreenModel {
    pub(super) fn project(entries: &[InboxSummary], timezone: Tz, focused: usize) -> Self {
        let rows = entries
            .iter()
            .filter(|entry| entry.project_id.is_none())
            .map(|entry| {
                let captured = entry.captured_at.with_timezone(&timezone);
                let mut metadata = format!(" · {}", captured.format("%Y-%m-%d %H:%M"));
                if !entry.tags.is_empty() {
                    metadata.push_str(" · #");
                    metadata.push_str(&display_line(&entry.tags.join(" #")));
                }
                InboxRow {
                    title: display_line(&entry.title),
                    metadata,
                }
            })
            .collect();
        Self { rows, focused }
    }
}

pub(super) struct ProjectPickerScreenModel {
    pub(super) projects: Vec<String>,
    pub(super) focused: usize,
}

impl ProjectPickerScreenModel {
    pub(super) fn hint(&self, width: u16) -> &'static str {
        if self.projects.is_empty() || width < 18 {
            "Esc cancel"
        } else if width < 44 {
            "Esc  Enter assign"
        } else {
            "Esc cancel  Enter assign  jk select"
        }
    }
}

pub(super) struct OutlineScreenModel {
    pub(super) rows: Vec<OutlineRow>,
    pub(super) focused: usize,
    pub(super) has_blocked_work: bool,
}

pub(super) struct OutlineRow {
    pub(super) indent: &'static str,
    pub(super) title: String,
    pub(super) is_project: bool,
    pub(super) blocked: bool,
}

impl OutlineScreenModel {
    pub(super) fn project(rows: &[OutlineItem], focused: usize) -> Self {
        let has_blocked_work = rows.iter().any(|row| row.blocked);
        Self {
            rows: rows
                .iter()
                .map(|row| OutlineRow {
                    indent: ["", "  ", "    "][row.depth.min(2)],
                    title: display_line(&row.title),
                    is_project: row.depth == 0,
                    blocked: row.blocked,
                })
                .collect(),
            focused,
            has_blocked_work,
        }
    }
}

pub(super) struct PickerScreenModel {
    pub(super) query: String,
    pub(super) labels: Vec<String>,
    pub(super) selected: usize,
    pub(super) empty: &'static str,
}

impl PickerScreenModel {
    fn documents(query: &str, selected: usize, matches: Vec<picker::Match>) -> Self {
        Self {
            query: format!("/{}▏", display_line(query)),
            labels: matches
                .into_iter()
                .map(|row| format!("{}{}", "  ".repeat(row.depth), display_line(&row.title)))
                .collect(),
            selected,
            empty: "No matching documents",
        }
    }

    pub(super) fn document_hint(width: u16) -> &'static str {
        if width >= 47 {
            "Type to search  ↑↓ move  Enter open  Esc cancel"
        } else if width >= 22 {
            "↑↓  Enter open  Esc"
        } else {
            "Enter  Esc"
        }
    }
}

pub(super) struct DeadlineScreenModel<'a> {
    title: String,
    pub(super) value: &'a str,
}

impl<'a> DeadlineScreenModel<'a> {
    fn from_form(form: &'a deadline::Form) -> Self {
        Self {
            title: format!(" Deadline · {} ", form.id),
            value: if form.cleared { "" } else { &form.value },
        }
    }

    pub(super) fn title(&self, width: u16) -> &str {
        if width < 51 {
            " Deadline "
        } else {
            &self.title
        }
    }

    pub(super) fn instructions(width: u16) -> &'static str {
        if width < 45 {
            "YYYY-MM-DD"
        } else {
            "YYYY-MM-DD · Delete clears the deadline"
        }
    }

    pub(super) fn hint(width: u16) -> &'static str {
        match width {
            0..=25 => "Enter  Del  Esc",
            26..=37 => "Enter save  Del clear  Esc",
            _ => "Enter save  Delete clear  Esc cancel",
        }
    }
}

pub(super) struct CreationScreenModel<'a> {
    pub(super) title: String,
    pub(super) fields: Vec<CreationField<'a>>,
    pub(super) selected: usize,
    pub(super) guidance: Option<&'static str>,
    pub(super) enter_action: &'static str,
}

pub(super) struct CreationField<'a> {
    pub(super) label: &'static str,
    pub(super) value: std::borrow::Cow<'a, str>,
    pub(super) uses_default: bool,
}

impl<'a> CreationScreenModel<'a> {
    pub(super) fn from_form(form: &'a creation::Form) -> Self {
        let fields = form
            .fields
            .iter()
            .map(|field| {
                let default = field.default_date.filter(|_| field.value.is_empty());
                CreationField {
                    label: field.label,
                    value: default.map_or_else(
                        || std::borrow::Cow::Borrowed(field.value.as_str()),
                        |day| std::borrow::Cow::Owned(day.to_string()),
                    ),
                    uses_default: default.is_some(),
                }
            })
            .collect();
        let guidance = match form.kind {
            creation::Kind::Reminder => Some("RFC3339 with the Vault timezone offset"),
            creation::Kind::Project | creation::Kind::Event | creation::Kind::Deadline => {
                Some("Type a date to replace the default (YYYY-MM-DD).")
            }
            creation::Kind::Task => None,
        };
        Self {
            title: format!(" New {} ", form.kind.label().to_lowercase()),
            fields,
            selected: form.selected,
            guidance,
            enter_action: if form.selected + 1 == form.fields.len() {
                "Enter save"
            } else {
                "Enter next"
            },
        }
    }
}

pub(super) struct TimelineScreenModel {
    pub(super) title: String,
    pub(super) compact_title: String,
    pub(super) day_labels: Vec<String>,
    pub(super) day_column: usize,
    pub(super) focused_row: usize,
    pub(super) rows: Vec<TimelineRow>,
}

pub(super) struct TimelineRow {
    pub(super) label: String,
    pub(super) compact_label: String,
    pub(super) cells: [&'static str; timeline::WINDOW_DAYS as usize],
}

impl TimelineScreenModel {
    pub(super) fn project(
        entries: Vec<CalendarItem>,
        day: NaiveDate,
        timezone: Tz,
        row: usize,
    ) -> Self {
        let start = timeline::window_start(day);
        let dates: [NaiveDate; timeline::WINDOW_DAYS as usize] =
            std::array::from_fn(|index| start + Days::new(index as u64));
        let day_column = (day - start).num_days() as usize;
        let rows: Vec<_> = entries
            .into_iter()
            .map(|entry| {
                let marker = match entry.kind {
                    CalendarKind::ActivePeriod => "A",
                    CalendarKind::Event => "E",
                    CalendarKind::TaskOccurrence => "T",
                    CalendarKind::TimeBlock => "B",
                    CalendarKind::Deadline => "!",
                };
                let cells = dates.map(|date| timeline::cell(&entry, date, timezone));
                let label = format!("{marker} {}", entry.title);
                let day_marker = match cells[day_column] {
                    "  " => "--",
                    cell => cell,
                };
                TimelineRow {
                    compact_label: format!("{day_marker}  {label}"),
                    label,
                    cells,
                }
            })
            .collect();
        Self {
            title: format!(" {start} → {} ", dates[dates.len() - 1]),
            compact_title: format!(" Plan · {day} "),
            day_labels: dates
                .iter()
                .map(|date| date.format("%d").to_string())
                .collect(),
            day_column,
            focused_row: row.min(rows.len().saturating_sub(1)),
            rows,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MonthCell {
    pub(super) compact_label: String,
    pub(super) grid_label: String,
    pub(super) detail: String,
    pub(super) selected: bool,
    pub(super) in_selection: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MonthScreenModel {
    pub(super) title: String,
    pub(super) weeks: usize,
    pub(super) cells: Vec<Option<MonthCell>>,
    pub(super) focused_row: usize,
}

impl MonthScreenModel {
    fn from_entries<'a>(
        selected_day: NaiveDate,
        selection: (NaiveDate, NaiveDate),
        timezone: chrono_tz::Tz,
        entries: impl IntoIterator<Item = &'a CalendarItem>,
    ) -> Self {
        let first = selected_day.with_day(1).expect("valid day of month");
        let monday_offset = first.weekday().num_days_from_monday() as usize;
        let days: Vec<_> = first
            .iter_days()
            .take_while(|day| day.month() == first.month())
            .collect();
        let weeks = (days.len() + monday_offset).div_ceil(7);
        let (start, end) = selection;
        let entries: Vec<_> = entries.into_iter().collect();
        let cells = (0..weeks * 7)
            .map(|index| {
                let &day = index
                    .checked_sub(monday_offset)
                    .and_then(|index| days.get(index))?;

                let mut busy = 0;
                let mut due = false;
                let mut active_or_free = false;
                for entry in entries.iter().filter(|entry| entry.on_day(day, timezone)) {
                    busy += usize::from(entry.busy);
                    match entry.kind {
                        CalendarKind::ActivePeriod => active_or_free = true,
                        CalendarKind::Deadline => due = true,
                        _ => active_or_free |= !entry.busy,
                    }
                }

                let marker = if busy > 0 {
                    '*'
                } else if due {
                    '!'
                } else if active_or_free {
                    '.'
                } else {
                    ' '
                };
                let counts = if busy > 0 {
                    format!("{busy} busy")
                } else {
                    String::new()
                };
                let due_mark = if due { "!" } else { "" };
                let activity_mark = if active_or_free { "." } else { "" };
                Some(MonthCell {
                    compact_label: format!("{}{marker}", day.format("%a %d")),
                    grid_label: format!("{:>2}{marker}", day.day()),
                    detail: format!("{counts} {due_mark}{activity_mark}"),
                    selected: day == selected_day,
                    in_selection: (start..=end).contains(&day),
                })
            })
            .collect();

        Self {
            title: format!(" {} ", first.format("%Y-%m")),
            weeks,
            cells,
            focused_row: selected_day.day0() as usize,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DayRow {
    Section(&'static str),
    Entry(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DayScreenModel {
    pub(super) rows: Vec<DayRow>,
    pub(super) focused: usize,
}

impl DayScreenModel {
    fn from_entries(
        date: NaiveDate,
        timezone: Tz,
        entries: Vec<CalendarItem>,
        focus: usize,
    ) -> Self {
        let mut rows = Vec::new();
        let mut previous_lane = None;
        let mut focused = 0;
        for (entry_index, entry) in entries.into_iter().enumerate() {
            let lane = calendar::lane(&entry);
            if previous_lane != Some(lane) {
                rows.push(DayRow::Section(match lane {
                    calendar::Lane::AllDay => "All-day",
                    calendar::Lane::Timed => "Timed",
                    calendar::Lane::Deadline => "Deadlines",
                    calendar::Lane::Ongoing => "Ongoing",
                }));
                previous_lane = Some(lane);
            }
            if entry_index == focus {
                focused = rows.len();
            }
            rows.push(DayRow::Entry(calendar::day_label(&entry, date, timezone)));
        }
        Self { rows, focused }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgendaScreenModel {
    pub(super) labels: Vec<String>,
    pub(super) focused: usize,
}

impl AgendaScreenModel {
    fn from_entries(timezone: Tz, entries: Vec<CalendarItem>, focused: usize) -> Self {
        Self {
            labels: entries
                .iter()
                .map(|entry| calendar_format::item_label(entry, timezone))
                .collect(),
            focused,
        }
    }
}

pub(super) struct WeekScreenModel {
    pub(super) title: String,
    pub(super) legend: String,
    pub(super) headers: [WeekDayHeading; 7],
    pub(super) compact_title: String,
    pub(super) compact_rows: [String; 7],
    pub(super) compact_focus: Option<String>,
    pub(super) fixed_rows: [WeekRow; 3],
    pub(super) timed_rows: Vec<WeekRow>,
    pub(super) selected_day: usize,
    pub(super) selected_hour: usize,
    pub(super) preview: Option<String>,
}

pub(super) struct WeekDayHeading {
    pub(super) label: String,
    pub(super) selected: bool,
    pub(super) in_selection: bool,
}

pub(super) struct WeekRow {
    pub(super) label: String,
    pub(super) cells: [String; 7],
}

impl WeekScreenModel {
    fn from_week(
        week: Week,
        date: NaiveDate,
        (start, end): (NaiveDate, NaiveDate),
        hour_index: usize,
    ) -> Self {
        let zone = week.timezone();
        let columns: [_; 7] = std::array::from_fn(|index| week.column(index));
        let selected_day = (date - week.monday).num_days() as usize;
        let selected_hour = hour_index.min(week.hours.len().saturating_sub(1));
        let focused_entry = week.focused_timed(date, selected_hour, zone);
        let headers: [_; 7] = std::array::from_fn(|index| {
            let day = week.days[index];
            let selected = index == selected_day;
            let in_selection = (start..=end).contains(&day);
            let marker = if selected {
                '>'
            } else if in_selection {
                '+'
            } else {
                ' '
            };
            WeekDayHeading {
                label: format!("{marker}{}", day.format("%a %d")),
                selected,
                in_selection,
            }
        });
        let compact_rows = std::array::from_fn(|index| {
            let counts = [
                (Lane::AllDay, "all"),
                (Lane::Timed, "timed"),
                (Lane::Deadline, "due"),
                (Lane::Ongoing, "active"),
            ]
            .into_iter()
            .filter_map(|(lane, label)| {
                let count = columns[index].in_lane(lane).count();
                (count > 0).then(|| format!("{count} {label}"))
            })
            .collect::<Vec<_>>()
            .join(" · ");
            format!("{}  {counts}", headers[index].label)
        });
        let fixed_rows = [
            (Lane::AllDay, "All-day"),
            (Lane::Deadline, "Due"),
            (Lane::Ongoing, "Ongoing"),
        ]
        .map(|(lane, label)| WeekRow {
            label: label.into(),
            cells: std::array::from_fn(|index| {
                summarize_week_cell(columns[index].in_lane(lane), |entry| {
                    let mark = match lane {
                        Lane::AllDay if entry.busy => '*',
                        Lane::Deadline => '!',
                        _ => '·',
                    };
                    format!("{mark}{}", entry.title)
                })
            }),
        });
        let timed_rows = week
            .hours
            .iter()
            .enumerate()
            .map(|(index, &hour)| {
                let pointer = if index == selected_hour { '>' } else { ' ' };
                WeekRow {
                    label: format!("{pointer} {hour:02}:00"),
                    cells: std::array::from_fn(|column| {
                        let column = &columns[column];
                        summarize_week_cell(column.at_hour(hour, zone), |entry| {
                            let time = calendar::grid_time(entry, column.date, zone);
                            let mark = if entry.busy { '*' } else { '·' };
                            format!("{mark}{time} {}", entry.title)
                        })
                    }),
                }
            })
            .collect();
        let timed = if week.hours.is_empty() {
            "No timed entries".into()
        } else {
            format!("Timed hours {}/{}", selected_hour + 1, week.hours.len())
        };
        Self {
            title: format!(" {} → {} ", week.monday, week.days[6]),
            legend: format!(" {timed}  * Busy  · Free  ! Due "),
            headers,
            compact_title: week
                .hours
                .get(selected_hour)
                .map_or_else(|| " Week ".into(), |hour| format!(" Week · {hour:02}:00 ")),
            compact_rows,
            compact_focus: focused_entry
                .map(|entry| format!("{}  {}", date.format("%a %d"), entry.title)),
            fixed_rows,
            timed_rows,
            selected_day,
            selected_hour,
            preview: focused_entry.map(|entry| calendar::day_label(entry, date, zone)),
        }
    }
}

fn summarize_week_cell<'a>(
    mut entries: impl Iterator<Item = &'a CalendarItem>,
    label: impl FnOnce(&CalendarItem) -> String,
) -> String {
    let Some(first) = entries.next() else {
        return String::new();
    };
    let label = label(first);
    match entries.count() {
        0 => label,
        additional => format!("+{additional} {label}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcbtodo_domain::{
        CalendarKind, Id, Occurrence,
        time::{DateSpan, Deadline, Span},
    };

    #[test]
    fn wide_graph_keeps_the_title_visible_when_details_cannot_fit() {
        use std::rc::Rc;

        use crate::tui::{calendar_tests::projected_screen, graph::Node};

        let title = "实现地址转换 and verify the page-table walk · 边界检查完成";
        let nodes: Rc<[_]> = [
            Node {
                id: Id::from_u128(1),
                title: "Prepare fixtures".into(),
                blocked: false,
                prerequisites: Vec::new(),
                dependents: vec![1],
            },
            Node {
                id: Id::from_u128(2),
                title: title.into(),
                blocked: true,
                prerequisites: vec![0],
                dependents: Vec::new(),
            },
        ]
        .into();
        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Graph;
        state.row = 1;
        let model = ScreenModel::project(
            &state,
            chrono_tz::UTC,
            ScreenData::Main(Ok(ViewRows::Graph(nodes))),
        );

        let short = projected_screen(&model, 96, 5);
        let selected = short.lines().nth(2).unwrap();
        assert!(selected.starts_with("│› "), "{short}");
        assert!(selected.contains(title), "{short}");
        assert!(selected.ends_with('│'), "{short}");
        assert!(!short.contains("Prerequisites"), "{short}");

        let tall = projected_screen(&model, 96, 12);
        assert!(tall.contains("Prerequisites (1)"), "{tall}");
        assert!(tall.contains("← 1  Prepare fixtures"), "{tall}");
        assert!(tall.contains("Dependents (0)"), "{tall}");
    }

    #[test]
    fn model_computes_calendar_markers_and_selection_without_rendering() {
        let selected_day = "2026-11-20".parse().unwrap();
        let busy_day = DateSpan::new("2026-11-20".parse().unwrap(), selected_day).unwrap();
        let free_day =
            DateSpan::new("2026-11-25".parse().unwrap(), "2026-11-25".parse().unwrap()).unwrap();
        let entries = [
            CalendarItem {
                id: "10000000-0000-4000-8000-000000000001".parse().unwrap(),
                title: "Busy event".into(),
                kind: CalendarKind::Event,
                busy: true,
                occurrence: Occurrence::Span(Span::DateSpan(busy_day)),
            },
            CalendarItem {
                id: "10000000-0000-4000-8000-000000000002".parse().unwrap(),
                title: "Free event".into(),
                kind: CalendarKind::Event,
                busy: false,
                occurrence: Occurrence::Span(Span::DateSpan(free_day)),
            },
            CalendarItem {
                id: "10000000-0000-4000-8000-000000000003".parse().unwrap(),
                title: "Due on the busy day".into(),
                kind: CalendarKind::Deadline,
                busy: false,
                occurrence: Occurrence::Deadline(Deadline::Date {
                    value: selected_day,
                }),
            },
        ];
        let model = MonthScreenModel::from_entries(
            selected_day,
            ("2026-11-19".parse().unwrap(), "2026-11-21".parse().unwrap()),
            "Asia/Shanghai".parse().unwrap(),
            entries.iter(),
        );

        assert_eq!(model.title, " 2026-11 ");
        assert_eq!(model.weeks, 6);
        assert!(model.cells[0].is_none());
        let days: Vec<_> = model.cells.iter().flatten().collect();
        let selected = days[model.focused_row];
        assert_eq!(selected.compact_label, "Fri 20*");
        assert_eq!(selected.grid_label, "20*");
        assert_eq!(selected.detail, "1 busy !");
        assert!(selected.selected);
        assert!(selected.in_selection);
        let free = days[24];
        assert_eq!(free.compact_label, "Wed 25.");
        assert!(!free.selected);
        assert!(!free.in_selection);
    }

    #[test]
    fn week_model_owns_projection_and_exposes_only_display_data() {
        let selected_day = "2026-11-25".parse().unwrap();
        let timezone = "Asia/Shanghai".parse().unwrap();
        let timed = mcbtodo_domain::time::DateTimeSpan::new(
            "2026-11-24T23:30:00+08:00".parse().unwrap(),
            "2026-11-25T01:00:00+08:00".parse().unwrap(),
            timezone,
        )
        .unwrap();
        let entries = vec![CalendarItem {
            id: "10000000-0000-4000-8000-000000000003".parse().unwrap(),
            title: "计划 Rust".into(),
            kind: CalendarKind::Event,
            busy: true,
            occurrence: Occurrence::Span(Span::DateTimeSpan(timed)),
        }];

        let model = WeekScreenModel::from_week(
            Week::from_entries(selected_day, timezone, entries),
            selected_day,
            ("2026-11-24".parse().unwrap(), selected_day),
            0,
        );

        assert_eq!(model.title, " 2026-11-23 → 2026-11-29 ");
        assert!(model.headers[1].in_selection);
        assert!(!model.headers[1].selected);
        assert_eq!(model.headers[2].label, ">Wed 25");
        assert_eq!(model.compact_rows[2], ">Wed 25  1 timed");
        assert_eq!(model.selected_day, 2);
        assert_eq!(model.selected_hour, 0);
        assert_eq!(model.timed_rows.len(), 2);
        assert_eq!(model.timed_rows[0].label, "> 00:00");
        assert_eq!(model.timed_rows[0].cells[2], "*← cont. 计划 Rust");
        assert_eq!(model.timed_rows[1].cells[1], "*23:30 计划 Rust");
        assert!(
            model
                .preview
                .as_deref()
                .is_some_and(|text| text.contains("2026-11-24 23:30–01:00  计划 Rust"))
        );
    }

    #[test]
    fn timeline_model_marks_timed_occurrences_across_midnight() {
        let timezone = chrono_tz::Asia::Shanghai;
        let selected_day = "2026-11-25".parse().unwrap();
        let span = mcbtodo_domain::time::DateTimeSpan::new(
            "2026-11-24T23:30:00+08:00".parse().unwrap(),
            "2026-11-25T01:00:00+08:00".parse().unwrap(),
            timezone,
        )
        .unwrap();
        let model = TimelineScreenModel::project(
            vec![CalendarItem {
                id: uuid::Uuid::nil(),
                title: "计划 Rust".into(),
                kind: CalendarKind::Event,
                busy: true,
                occurrence: Occurrence::Span(Span::DateTimeSpan(span)),
            }],
            selected_day,
            timezone,
            10,
        );

        assert_eq!(model.title, " 2026-11-23 → 2026-12-06 ");
        assert_eq!(model.compact_title, " Plan · 2026-11-25 ");
        assert_eq!(model.day_column, 2);
        assert_eq!(model.day_labels[model.day_column], "25");
        assert_eq!(model.focused_row, 0);
        assert_eq!(model.rows[0].label, "E 计划 Rust");
        assert_eq!(model.rows[0].compact_label, "==  E 计划 Rust");
        assert_eq!(&model.rows[0].cells[..4], &["  ", ">>", "==", "  "]);
    }

    #[test]
    fn day_model_owns_sections_and_selection_mapping() {
        let date = "2026-11-25".parse().unwrap();
        let timezone = "Asia/Shanghai".parse().unwrap();
        let entries = vec![
            CalendarItem {
                id: "10000000-0000-4000-8000-000000000004".parse().unwrap(),
                title: "All day".into(),
                kind: CalendarKind::Event,
                busy: false,
                occurrence: Occurrence::Span(Span::DateSpan(DateSpan::new(date, date).unwrap())),
            },
            CalendarItem {
                id: "10000000-0000-4000-8000-000000000005".parse().unwrap(),
                title: "Due".into(),
                kind: CalendarKind::Deadline,
                busy: false,
                occurrence: Occurrence::Deadline(mcbtodo_domain::time::Deadline::Date {
                    value: date,
                }),
            },
        ];
        let model = DayScreenModel::from_entries(date, timezone, entries.clone(), 0);

        assert_eq!(
            model.rows,
            vec![
                DayRow::Section("All-day"),
                DayRow::Entry("All day · free".into()),
                DayRow::Section("Deadlines"),
                DayRow::Entry("Due".into()),
            ]
        );
        assert_eq!(model.focused, 1);
        let model = DayScreenModel::from_entries(date, timezone, entries, 1);
        assert_eq!(model.focused, 3);
    }

    #[test]
    fn agenda_model_exposes_owned_labels_without_rendering_vault_data() {
        let date = "2026-11-25".parse().unwrap();
        let timezone = "Asia/Shanghai".parse().unwrap();
        let entries = vec![CalendarItem {
            id: "10000000-0000-4000-8000-000000000006".parse().unwrap(),
            title: "Agenda entry".into(),
            kind: CalendarKind::Event,
            busy: true,
            occurrence: Occurrence::Span(Span::DateSpan(DateSpan::new(date, date).unwrap())),
        }];

        let model = AgendaScreenModel::from_entries(timezone, entries, 0);

        assert_eq!(
            model.labels,
            vec!["event · busy · Agenda entry · 2026-11-25"]
        );
    }
}
