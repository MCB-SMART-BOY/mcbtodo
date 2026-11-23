mod calendar;
mod creation;
mod daemon_views;
mod deadline;
mod graph;
mod keymap;
mod picker;
mod proposal_review;
mod render;
mod review;
mod screen_model;
mod settings;
mod theme;
mod time_block;
mod timeline;
mod watch;
mod workflow;

use std::{
    cell::RefCell,
    collections::BTreeSet,
    env,
    io::{self, IsTerminal},
    path::Path,
    process::Command,
    rc::Rc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use chrono::{Datelike, Days, NaiveDate};
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use mcbtodo_application::{
    CalendarItem, OutlineItem, TargetAction, VaultCommands, deadline_edit_context,
    project_action_targets, project_calendar, project_inbox, project_outline,
    project_revision_in_snapshot,
};
use mcbtodo_domain::{CalendarKind, Id, Occurrence, time::Span};
use mcbtodo_platform::{AppConfig, AppConfigSnapshot, HomeViewPolicy, TuiTheme, WorkspacePaths};
use mcbtodo_protocol::{Request, workflow::WorkflowRequest};
use mcbtodo_vault::{Revision, Vault, WriteError};
use ratatui::{DefaultTerminal, Terminal, backend::CrosstermBackend};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Month,
    Week,
    Day,
    Agenda,
    Outline,
    Graph,
    Timeline,
    Workflow,
    Inbox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusIdentity {
    Document(Id),
    Calendar {
        id: Id,
        kind: CalendarKind,
        occurrence: Occurrence,
    },
}

impl FocusIdentity {
    fn document_id(self) -> Id {
        match self {
            Self::Document(id) | Self::Calendar { id, .. } => id,
        }
    }
}

fn calendar_focus(item: &CalendarItem) -> FocusIdentity {
    FocusIdentity::Calendar {
        id: item.id,
        kind: item.kind,
        occurrence: item.occurrence,
    }
}

fn focus_date(focus: FocusIdentity, timezone: chrono_tz::Tz) -> Option<NaiveDate> {
    match focus {
        FocusIdentity::Calendar {
            occurrence: Occurrence::Span(Span::DateSpan(span)),
            ..
        } => Some(span.start()),
        FocusIdentity::Calendar {
            occurrence: Occurrence::Span(Span::DateTimeSpan(span)),
            ..
        } => Some(span.start().with_timezone(&timezone).date_naive()),
        FocusIdentity::Calendar {
            occurrence: Occurrence::Deadline(deadline),
            ..
        } => deadline.local_date(timezone),
        FocusIdentity::Document(_) => None,
    }
}

impl View {
    const ALL: [Self; 9] = [
        Self::Month,
        Self::Week,
        Self::Day,
        Self::Agenda,
        Self::Outline,
        Self::Graph,
        Self::Timeline,
        Self::Workflow,
        Self::Inbox,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Month => "Month",
            Self::Week => "Week",
            Self::Day => "Day",
            Self::Agenda => "Agenda",
            Self::Outline => "Outline",
            Self::Graph => "Graph",
            Self::Timeline => "Timeline",
            Self::Workflow => "Workflow",
            Self::Inbox => "Inbox",
        }
    }

    fn uses_dates(self) -> bool {
        matches!(self, Self::Month | Self::Week | Self::Day | Self::Agenda)
    }

    fn saved_name(self) -> &'static str {
        match self {
            Self::Month => "month",
            Self::Week => "week",
            Self::Day => "day",
            Self::Agenda => "agenda",
            Self::Outline => "outline",
            Self::Graph => "graph",
            Self::Timeline => "timeline",
            Self::Workflow => "workflow",
            Self::Inbox => "inbox",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL.into_iter().find(|view| view.saved_name() == name)
    }
}

fn initial_view(paths: &WorkspacePaths, policy: HomeViewPolicy) -> Result<View> {
    if policy == HomeViewPolicy::Month {
        return Ok(View::Month);
    }
    match paths.read_state_file(&paths.last_view_path()) {
        Ok(saved) => Ok(View::from_name(&saved).unwrap_or(View::Month)),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(View::Month)
        }
        Err(error) => Err(error).context("loading the saved home view"),
    }
}

#[derive(Default)]
enum Overlay {
    #[default]
    None,
    Space(review::ReviewViewport),
    Settings(settings::Form),
    Views(review::ReviewViewport),
    Time,
    Create(review::ReviewViewport),
    CreateForm(creation::Form),
    Deadline(deadline::Form),
    TimeBlock(time_block::Form),
    GraphEdit {
        edit: graph::Edit,
        viewport: review::ReviewViewport,
    },
    Picker {
        query: String,
        row: usize,
    },
    Proposals,
    InboxProject {
        inbox_id: Id,
        revision: Revision,
        row: usize,
    },
    Help(review::ReviewViewport),
    Command(String),
}

#[derive(PartialEq, Eq, Debug)]
enum Action {
    Continue,
    Quit,
    Reload,
    Edit,
    OpenSettings,
    Settings(settings::Command),
    Timer(Request),
    CreateCalendar(creation::Submission),
    CreateReminder(creation::Submission),
    CreateTimeBlock(time_block::Submission),
    SetDeadline(deadline::Submission),
    SetGraphDependencies(graph::Submission),
    CreateTask(creation::Submission),
    Proposals(proposal_review::Request),
    ApplyProposal(Id),
    Workflow(WorkflowRequest),
    TriageInbox {
        inbox_id: Id,
        project_id: Id,
        revision: Revision,
    },
}

enum TerminalHandoff<'a> {
    Quit,
    Edit(&'a Path),
    ApplyProposal(Id),
}

struct State {
    day: NaiveDate,
    anchor: Option<NaiveDate>,
    view: View,
    overlay: Overlay,
    row: usize,
    document_id: Option<Id>,
    calendar_focus: Option<FocusIdentity>,
    daemon_views: Option<daemon_views::DaemonViews>,
    week_hour: usize,
    graph_selection: Option<BTreeSet<Id>>,
    graph_nodes: RefCell<Option<Rc<[graph::Node]>>>,
    proposals: proposal_review::Review,
    workflow: workflow::State,
    keymap: keymap::Keymap,
    home_view: HomeViewPolicy,
    no_color: bool,
    has_timezone_override: bool,
    message: String,
    vault_warning: Option<String>,
}

impl State {
    fn new(day: NaiveDate) -> Self {
        Self {
            day,
            anchor: None,
            view: View::Month,
            overlay: Overlay::None,
            row: 0,
            document_id: None,
            calendar_focus: None,
            daemon_views: None,
            week_hour: 0,
            graph_selection: None,
            graph_nodes: RefCell::new(None),
            proposals: proposal_review::Review::default(),
            workflow: workflow::State::default(),
            keymap: keymap::Keymap::default(),
            home_view: HomeViewPolicy::default(),
            no_color: false,
            has_timezone_override: false,
            message: String::new(),
            vault_warning: None,
        }
    }

    fn selected_range(&self) -> (NaiveDate, NaiveDate) {
        let anchor = self.anchor.unwrap_or(self.day);
        (anchor.min(self.day), anchor.max(self.day))
    }

    fn change_day(&mut self, delta: i32) {
        let next = if delta >= 0 {
            self.day.checked_add_days(Days::new(delta as u64))
        } else {
            self.day
                .checked_sub_days(Days::new(u64::from(delta.unsigned_abs())))
        };
        if let Some(next) = next.filter(|day| (1..=9999).contains(&day.year())) {
            self.set_day(next);
        }
    }

    fn set_day(&mut self, day: NaiveDate) {
        if calendar::monday_of(self.day) != calendar::monday_of(day) {
            self.week_hour = 0;
        }
        self.day = day;
        if matches!(self.view, View::Day | View::Agenda) {
            self.row = 0;
        }
    }

    fn switch(&mut self, view: View, vault: &Vault) {
        if self.view != view {
            let selected = if self.view == View::Week && view == View::Day {
                self.week(vault).ok().and_then(|week| {
                    week.focused_timed(self.day, self.week_hour, vault.timezone())
                        .map(calendar_focus)
                })
            } else {
                self.selected_focus(vault)
            };
            match selected {
                Some(focus @ FocusIdentity::Calendar { .. }) => {
                    self.calendar_focus = Some(focus);
                }
                Some(FocusIdentity::Document(_)) => self.calendar_focus = None,
                None => {}
            }
            let preferred = selected.or_else(|| {
                (matches!(self.view, View::Month | View::Week)
                    && matches!(view, View::Agenda | View::Day | View::Month | View::Week))
                .then_some(self.calendar_focus)
                .flatten()
                .or_else(|| self.document_id.map(FocusIdentity::Document))
            });
            self.document_id = preferred.map(FocusIdentity::document_id);
            if view == View::Day
                && let Some(focus @ FocusIdentity::Calendar { occurrence, .. }) = preferred
                && !occurrence.on_day(self.day, vault.timezone())
                && let Some(day) = focus_date(focus, vault.timezone())
            {
                self.set_day(day);
            }
            self.view = view;
            self.row = 0;
            self.sync_row(vault, preferred);
        }
    }

    fn sync_row(&mut self, vault: &Vault, preferred: Option<FocusIdentity>) {
        let focuses = self.row_focuses(vault);
        if let Some(focus) = preferred {
            if focuses.get(self.row) == Some(&focus) {
                return;
            }
            if let Some(index) = focuses.iter().position(|&candidate| candidate == focus) {
                self.row = index;
                return;
            }
            if let Some(index) = focuses
                .iter()
                .position(|candidate| candidate.document_id() == focus.document_id())
            {
                self.row = index;
                return;
            }
        }
        self.row = self.row.min(focuses.len().saturating_sub(1));
    }

    fn row_ids(&self, vault: &Vault) -> Vec<Id> {
        self.row_focuses(vault)
            .into_iter()
            .map(FocusIdentity::document_id)
            .collect()
    }

    fn selected_focus(&self, vault: &Vault) -> Option<FocusIdentity> {
        self.row_focuses(vault).get(self.row).copied()
    }

    fn row_focuses(&self, vault: &Vault) -> Vec<FocusIdentity> {
        let calendar = match self.view {
            View::Month | View::Week | View::Workflow => return Vec::new(),
            View::Day => self.day_entries(vault),
            View::Agenda => self.agenda(vault),
            View::Timeline => self.calendar_entries(vault, View::Timeline),
            View::Outline => {
                return self
                    .outline_rows(vault)
                    .unwrap_or_default()
                    .iter()
                    .map(|row| FocusIdentity::Document(row.id))
                    .collect();
            }
            View::Graph => {
                return self
                    .graph_nodes(vault)
                    .iter()
                    .map(|node| FocusIdentity::Document(node.id))
                    .collect();
            }
            View::Inbox => {
                return project_inbox(vault)
                    .into_iter()
                    .filter(|entry| entry.project_id.is_none())
                    .map(|entry| FocusIdentity::Document(entry.id))
                    .collect();
            }
        };
        // Projection failures have no actionable rows. The presenter displays the error.
        calendar
            .unwrap_or_default()
            .iter()
            .map(calendar_focus)
            .collect()
    }

    fn calendar_entries(&self, vault: &Vault, view: View) -> Result<Vec<CalendarItem>> {
        let dates = calendar::window(view, self.day, self.selected_range());
        match &self.daemon_views {
            Some(service) => Ok(service.calendar(dates, vault.timezone())?.to_vec()),
            None => project_calendar(vault, dates),
        }
    }

    fn outline_rows(&self, vault: &Vault) -> Result<Rc<[OutlineItem]>> {
        daemon_views::outline_rows(vault, self.daemon_views.as_ref())
    }

    fn day_entries(&self, vault: &Vault) -> Result<Vec<CalendarItem>> {
        let mut entries = self.calendar_entries(vault, View::Day)?;
        calendar::sort_day(&mut entries, vault.timezone());
        Ok(entries)
    }

    fn agenda(&self, vault: &Vault) -> anyhow::Result<Vec<CalendarItem>> {
        self.calendar_entries(vault, View::Agenda)
    }

    fn week(&self, vault: &Vault) -> Result<calendar::Week> {
        Ok(calendar::Week::from_entries(
            self.day,
            vault.timezone(),
            self.calendar_entries(vault, View::Week)?,
        ))
    }

    fn invalidate_daemon_views(&mut self) {
        if let Some(service) = &mut self.daemon_views {
            service.invalidate();
        }
    }

    fn handle(&mut self, mut key: KeyEvent, vault: &Vault) -> Action {
        use Action::*;
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                Quit
            } else {
                Continue
            };
        }
        if matches!(
            self.overlay,
            Overlay::None | Overlay::Space(_) | Overlay::Views(_) | Overlay::Help(_)
        ) {
            key = self
                .keymap
                .normalize(key, matches!(self.overlay, Overlay::Views(_)));
        }
        match &mut self.overlay {
            Overlay::Settings(form) => {
                self.message.clear();
                match form.handle(key.code) {
                    settings::Input::Continue => {}
                    settings::Input::Close => {
                        self.overlay = Overlay::Space(Default::default());
                    }
                    settings::Input::Command(command) => return Settings(command),
                }
                return Continue;
            }
            Overlay::GraphEdit { viewport, .. } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => self.overlay = Overlay::None,
                    KeyCode::Char('y') | KeyCode::Enter => {
                        let edit = match std::mem::replace(&mut self.overlay, Overlay::None) {
                            Overlay::GraphEdit { edit, .. } => edit,
                            _ => unreachable!(),
                        };
                        return SetGraphDependencies(graph::Submission {
                            target: edit.target,
                            prerequisites: edit.prerequisites,
                            revision: edit.revision,
                        });
                    }
                    _ => viewport.scroll(key.code),
                }
                return Continue;
            }
            Overlay::Command(input) => {
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Enter => {
                        let input = std::mem::take(input);
                        self.overlay = Overlay::None;
                        let parts = input.split_whitespace().collect::<Vec<_>>();
                        match parts.as_slice() {
                            ["q" | "quit"] => return Quit,
                            ["reload"] => return Reload,
                            ["today"] => self.set_day(crate::today(vault)),
                            ["date", value] => match crate::parse_day(value) {
                                Ok(day) => self.set_day(day),
                                Err(error) => self.message = error,
                            },
                            ["help" | "?"] => self.overlay = Overlay::Help(Default::default()),
                            ["settings"] => return OpenSettings,
                            ["search", query @ ..] if !query.is_empty() => {
                                self.overlay = Overlay::Picker {
                                    query: query.join(" "),
                                    row: 0,
                                };
                            }
                            ["proposals" | "proposal"] => {
                                self.overlay = Overlay::Proposals;
                                self.proposals.detail = None;
                                self.message.clear();
                                return Proposals(proposal_review::Request::List);
                            }
                            ["inbox"] => self.switch(View::Inbox, vault),
                            ["timer", operation] => {
                                let request = match *operation {
                                    "status" => Some(Request::Status),
                                    "start" => Some(Request::Start {
                                        work_unit_id: self
                                            .selected_document(vault)
                                            .filter(|&id| vault.is_work_unit(id)),
                                    }),
                                    "pause" => Some(Request::Pause),
                                    "stop" => Some(Request::Stop),
                                    _ => None,
                                };
                                if let Some(request) = request {
                                    return Timer(request);
                                }
                                self.message = "Usage: timer status|start|pause|stop".into();
                            }
                            ["view", view] | [view] => {
                                if let Some(view) = View::from_name(view) {
                                    self.switch(view, vault);
                                    self.message.clear();
                                    if view == View::Workflow {
                                        return self.workflow.open();
                                    }
                                } else {
                                    let names = View::ALL.map(View::saved_name).join("|");
                                    self.message = format!("Usage: view {names}");
                                }
                            }
                            _ => self.message = "Type :help for available commands".into(),
                        }
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(character) => input.push(character),
                    _ => {}
                }
                return Continue;
            }
            Overlay::Picker { query, row } => {
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Enter => {
                        let Ok(matches) = picker::matches(vault, self.daemon_views.as_ref(), query)
                        else {
                            return Continue;
                        };
                        let selected = matches.get(*row).map(|item| item.id);
                        self.overlay = Overlay::None;
                        if let Some(id) = selected {
                            self.switch(View::Outline, vault);
                            self.document_id = Some(id);
                            self.sync_row(vault, Some(FocusIdentity::Document(id)));
                            self.message.clear();
                        } else {
                            self.message = "No matching documents".into();
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *row = row.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Ok(matches) =
                            picker::matches(vault, self.daemon_views.as_ref(), query)
                        {
                            *row = move_row(*row, true, matches.len());
                        }
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        *row = 0;
                    }
                    KeyCode::Char(character) => {
                        query.push(character);
                        *row = 0;
                    }
                    _ => {}
                }
                return Continue;
            }
            Overlay::InboxProject {
                inbox_id,
                revision,
                row,
            } => {
                let projects = project_rows(vault);
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Enter => {
                        if let Some((project_id, _)) = projects.get(*row) {
                            let action = TriageInbox {
                                inbox_id: *inbox_id,
                                project_id: *project_id,
                                revision: revision.clone(),
                            };
                            self.overlay = Overlay::None;
                            return action;
                        }
                        self.message = "No Project available".into();
                    }
                    KeyCode::Up | KeyCode::Char('k') => *row = row.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        *row = move_row(*row, true, projects.len());
                    }
                    _ => {}
                }
                return Continue;
            }
            Overlay::Help(viewport) => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                    self.overlay = Overlay::None;
                } else {
                    viewport.scroll(key.code);
                }
                return Continue;
            }
            Overlay::Space(viewport) => {
                match key.code {
                    KeyCode::Enter => return OpenSettings,
                    KeyCode::Char('u') => self.overlay = Overlay::Views(Default::default()),
                    KeyCode::Char('t') => self.overlay = Overlay::Time,
                    KeyCode::Char('c') if self.view.uses_dates() || self.view == View::Outline => {
                        self.overlay = Overlay::Create(Default::default());
                        self.message.clear();
                    }
                    KeyCode::Char('d') => {
                        let Some(id) = self.selected_document(vault) else {
                            self.message = "Select a Project, Task or WorkUnit first".into();
                            return Continue;
                        };
                        let Some(context) = deadline_edit_context(vault, id) else {
                            self.message = "Selection does not support deadlines".into();
                            return Continue;
                        };
                        self.overlay = Overlay::Deadline(deadline::Form::new(
                            id,
                            context.revision,
                            context.deadline,
                            vault.timezone(),
                        ));
                        self.message.clear();
                        return Continue;
                    }
                    KeyCode::Char('a') => {
                        self.overlay = Overlay::Proposals;
                        self.proposals.detail = None;
                        self.message.clear();
                        return Proposals(proposal_review::Request::List);
                    }
                    KeyCode::Char('?') => self.overlay = Overlay::Help(Default::default()),
                    KeyCode::Char('r') => {
                        self.overlay = Overlay::None;
                        return Reload;
                    }
                    KeyCode::Esc => self.overlay = Overlay::None,
                    code => viewport.scroll(code),
                }
                return Continue;
            }
            Overlay::Views(viewport) => {
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::Space(Default::default()),
                    code => {
                        let view = match code {
                            KeyCode::Char('m') => View::Month,
                            KeyCode::Char('w') => View::Week,
                            KeyCode::Char('d') => View::Day,
                            KeyCode::Char('a') => View::Agenda,
                            KeyCode::Char('l') => View::Outline,
                            KeyCode::Char('g') => View::Graph,
                            KeyCode::Char('t') => View::Timeline,
                            KeyCode::Char('i') => View::Inbox,
                            KeyCode::Char('f') => View::Workflow,
                            _ => {
                                viewport.scroll(code);
                                return Continue;
                            }
                        };
                        self.switch(view, vault);
                        self.overlay = Overlay::None;
                        if view == View::Workflow {
                            return self.workflow.open();
                        }
                    }
                }
                return Continue;
            }
            Overlay::Create(viewport) => {
                let kind = match key.code {
                    KeyCode::Char('p') => creation::Kind::Project,
                    KeyCode::Char('t') => {
                        let Some((project_id, revision)) = self.selected_project_revision(vault)
                        else {
                            self.message = "Select a Project in Outline to capture a Task".into();
                            return Continue;
                        };
                        self.overlay =
                            Overlay::CreateForm(creation::Form::task(project_id, revision));
                        return Continue;
                    }
                    KeyCode::Char('e') => creation::Kind::Event,
                    KeyCode::Char('d') => creation::Kind::Deadline,
                    KeyCode::Char('r') => {
                        let target = self.selected_document(vault).filter(|id| {
                            project_action_targets(vault, TargetAction::CreateReminder)
                                .iter()
                                .any(|candidate| candidate.id == *id)
                        });
                        self.overlay =
                            Overlay::CreateForm(creation::Form::reminder(vault.timezone(), target));
                        return Continue;
                    }
                    KeyCode::Char('b') => {
                        self.overlay = Overlay::TimeBlock(time_block::Form::new(self.day));
                        return Continue;
                    }
                    KeyCode::Esc => {
                        self.overlay = Overlay::Space(Default::default());
                        return Continue;
                    }
                    _ => {
                        viewport.scroll(key.code);
                        return Continue;
                    }
                };
                self.overlay =
                    Overlay::CreateForm(creation::Form::new(kind, self.day, self.selected_range()));
                return Continue;
            }
            Overlay::CreateForm(form) => {
                if let Some(action) = form.handle(key.code, &mut self.message) {
                    return action;
                }
                self.message.clear();
                self.overlay = Overlay::None;
                return Continue;
            }
            Overlay::Deadline(form) => {
                if let Some(action) = form.handle(key.code, vault, &mut self.message) {
                    return action;
                }
                self.message.clear();
                self.overlay = Overlay::None;
                return Continue;
            }
            Overlay::Time => {
                let request = match key.code {
                    KeyCode::Char('b') if self.view.uses_dates() => {
                        self.message.clear();
                        self.overlay = Overlay::TimeBlock(time_block::Form::new(self.day));
                        return Continue;
                    }
                    KeyCode::Char('i') => Request::Status,
                    KeyCode::Char('s') => Request::Start {
                        work_unit_id: self
                            .selected_document(vault)
                            .filter(|&id| vault.is_work_unit(id)),
                    },
                    KeyCode::Char('p') => Request::Pause,
                    KeyCode::Char('x') => Request::Stop,
                    KeyCode::Esc => {
                        self.overlay = Overlay::Space(Default::default());
                        return Continue;
                    }
                    _ => return Continue,
                };
                self.overlay = Overlay::None;
                return Timer(request);
            }
            Overlay::TimeBlock(form) => {
                if let Some(action) = form.handle(key.code, vault, &mut self.message) {
                    return action;
                }
                self.message.clear();
                self.overlay = Overlay::None;
                return Continue;
            }
            Overlay::Proposals => {
                if let Some(action) = self.proposals.handle(key, &mut self.message) {
                    return action;
                }
                self.message.clear();
                self.overlay = Overlay::None;
                return Continue;
            }
            Overlay::None => {}
        }
        if self.view == View::Workflow
            && let Some(action) = self.workflow.handle(key, &self.keymap, &mut self.message)
        {
            return action;
        }
        match key.code {
            KeyCode::Char('q') => return Quit,
            KeyCode::Char('r') => return Reload,
            KeyCode::Char('e') if self.view != View::Workflow => return Edit,
            KeyCode::Char('t') if self.view == View::Inbox => {
                let Some(inbox_id) = self.selected_document(vault) else {
                    self.message = "Inbox is empty".into();
                    return Continue;
                };
                let Some(revision) = vault.revision(inbox_id).cloned() else {
                    self.message = "Inbox entry has no revision; reload the Vault".into();
                    return Continue;
                };
                self.overlay = Overlay::InboxProject {
                    inbox_id,
                    revision,
                    row: 0,
                };
                self.message.clear();
            }
            KeyCode::Char('g') if matches!(self.view, View::Day | View::Agenda | View::Outline) => {
                if let Some(id) = self
                    .selected_document(vault)
                    .filter(|&id| vault.is_work_unit(id))
                {
                    self.document_id = Some(id);
                    self.switch(View::Graph, vault);
                    self.message.clear();
                } else {
                    self.message = "Select a WorkUnit to open Graph".into();
                }
            }
            KeyCode::Char(' ') => self.overlay = Overlay::Space(Default::default()),
            KeyCode::Char(':') => self.overlay = Overlay::Command(String::new()),
            KeyCode::Char('/') => {
                self.overlay = Overlay::Picker {
                    query: String::new(),
                    row: 0,
                }
            }
            KeyCode::Char('?') => self.overlay = Overlay::Help(Default::default()),
            KeyCode::Esc if self.view == View::Graph => self.graph_selection = None,
            KeyCode::Esc if self.view.uses_dates() => {
                self.anchor = None;
                if self.view == View::Agenda {
                    self.row = 0;
                }
            }
            KeyCode::Char('v') if self.view.uses_dates() => {
                self.anchor = self.anchor.is_none().then_some(self.day);
                if self.view == View::Agenda {
                    self.row = 0;
                }
            }
            KeyCode::Char('v') if self.view == View::Graph => {
                self.graph_selection = if self.graph_selection.is_some() {
                    None
                } else {
                    self.selected_document(vault).map(|id| BTreeSet::from([id]))
                };
            }
            KeyCode::Char('x') if self.view == View::Graph => {
                if let Some(id) = self.selected_document(vault) {
                    let selected = self.graph_selection.get_or_insert_with(BTreeSet::new);
                    if !selected.remove(&id) {
                        selected.insert(id);
                    }
                }
            }
            KeyCode::Char('a' | 'd') if self.view == View::Graph => {
                let Some(target) = self.selected_document(vault) else {
                    return Continue;
                };
                let Some(sources) = self.graph_selection.as_ref() else {
                    self.message = "Select source WorkUnits with v/x first".into();
                    return Continue;
                };
                match graph::edit(vault, target, sources, key.code == KeyCode::Char('a')) {
                    Ok(edit) => {
                        self.overlay = Overlay::GraphEdit {
                            edit,
                            viewport: review::ReviewViewport::default(),
                        };
                        self.message.clear();
                    }
                    Err(error) => self.message = format!("Graph edit unavailable · {error:#}"),
                }
            }
            KeyCode::Enter if matches!(self.view, View::Month | View::Day) => {
                self.switch(View::Agenda, vault);
            }
            KeyCode::Enter if self.view == View::Week => self.switch(View::Day, vault),
            KeyCode::PageUp if self.view == View::Week => self.change_day(-7),
            KeyCode::PageDown if self.view == View::Week => self.change_day(7),
            KeyCode::Char('h') | KeyCode::Left if self.view == View::Timeline => {
                self.change_day(-1);
            }
            KeyCode::Char('l') | KeyCode::Right if self.view == View::Timeline => {
                self.change_day(1);
            }
            KeyCode::Char('h') | KeyCode::Left if self.view.uses_dates() => self.change_day(-1),
            KeyCode::Char('l') | KeyCode::Right if self.view.uses_dates() => self.change_day(1),
            KeyCode::Char('h') | KeyCode::Left if self.view == View::Graph => {
                self.follow_edge(false, vault);
            }
            KeyCode::Char('l') | KeyCode::Right if self.view == View::Graph => {
                self.follow_edge(true, vault);
            }
            KeyCode::Char('j') | KeyCode::Down => self.vertical(true, vault),
            KeyCode::Char('k') | KeyCode::Up => self.vertical(false, vault),
            _ => {}
        }
        Continue
    }

    fn vertical(&mut self, down: bool, vault: &Vault) {
        match self.view {
            View::Month => self.change_day(if down { 7 } else { -7 }),
            View::Week => {
                if let Ok(week) = self.week(vault) {
                    let hour = self.week_hour.min(week.hours.len().saturating_sub(1));
                    self.week_hour = move_row(hour, down, week.hours.len());
                }
            }
            View::Graph => {
                let (row, id) = {
                    let nodes = self.graph_nodes(vault);
                    let row = move_row(self.row, down, nodes.len());
                    (row, nodes.get(row).map(|node| node.id))
                };
                self.row = row;
                if let Some(id) = id {
                    self.document_id = Some(id);
                }
            }
            View::Day | View::Agenda | View::Outline | View::Timeline => {
                let ids = self.row_ids(vault);
                self.row = move_row(self.row, down, ids.len());
                if let Some(&id) = ids.get(self.row) {
                    self.document_id = Some(id);
                }
            }
            View::Workflow => {}
            View::Inbox => {
                let ids = self.row_ids(vault);
                self.row = move_row(self.row, down, ids.len());
                if let Some(&id) = ids.get(self.row) {
                    self.document_id = Some(id);
                }
            }
        }
    }

    fn triage_inbox(
        &mut self,
        root: &Path,
        vault: &mut Vault,
        inbox_id: Id,
        project_id: Id,
        revision: Revision,
    ) {
        match VaultCommands::new(root, vault.timezone())
            .triage_inbox_entry(inbox_id, project_id, revision)
        {
            Ok(_) => {
                self.reload(root, vault);
                if self.message == "Vault reloaded" {
                    self.message = "Inbox entry converted to Task".into();
                }
            }
            Err(WriteError::StaleRevision { .. }) => {
                self.message = "Inbox entry changed · r to reload, then retry".into();
            }
            Err(error) => self.message = format!("Inbox triage failed · {error:#}"),
        }
    }

    fn follow_edge(&mut self, forward: bool, vault: &Vault) {
        let next = {
            let nodes = self.graph_nodes(vault);
            nodes.get(self.row).and_then(|node| {
                let neighbors = if forward {
                    &node.dependents
                } else {
                    &node.prerequisites
                };
                neighbors.first().map(|&row| (row, nodes[row].id))
            })
        };
        if let Some((row, id)) = next {
            self.row = row;
            self.document_id = Some(id);
        }
    }

    fn source<'a>(&self, vault: &'a Vault) -> Option<&'a Path> {
        vault.source(self.selected_document(vault)?)
    }

    fn selected_document(&self, vault: &Vault) -> Option<Id> {
        if self.view == View::Graph {
            return self.graph_nodes(vault).get(self.row).map(|node| node.id);
        }
        self.row_ids(vault).get(self.row).copied()
    }

    fn graph_nodes(&self, vault: &Vault) -> Rc<[graph::Node]> {
        let mut cached = self.graph_nodes.borrow_mut();
        cached
            .get_or_insert_with(|| graph::nodes(vault).into())
            .clone()
    }

    fn selected_project_revision(&self, vault: &Vault) -> Option<(Id, creation::ProjectRevision)> {
        if self.view != View::Outline {
            return None;
        }
        if self.daemon_views.is_some() {
            let rows = self.outline_rows(vault).ok()?;
            let row = rows.get(self.row).filter(|row| row.depth == 0)?;
            return Some((
                row.id,
                creation::ProjectRevision::Daemon(row.revision.clone()),
            ));
        }
        let id = self.selected_document(vault)?;
        project_revision_in_snapshot(vault, id)
            .map(|revision| (id, creation::ProjectRevision::Local(revision)))
    }

    fn reload(&mut self, root: &Path, vault: &mut Vault) {
        match Vault::load(root, vault.timezone()) {
            Ok(next) => {
                self.install_vault(vault, next);
                self.vault_warning = None;
                self.message = "Vault reloaded".into();
            }
            Err(error) => {
                self.invalidate_daemon_views();
                self.message = format!("Reload failed · {error:#}");
            }
        }
    }

    fn refresh_external(&mut self, root: &Path, vault: &mut Vault) -> bool {
        match Vault::load(root, vault.timezone()) {
            Ok(next) => {
                let recovered = self.vault_warning.take().is_some();
                if vault.has_same_documents(&next) {
                    // A local action may already have loaded these exact bytes.
                    if recovered {
                        self.invalidate_daemon_views();
                    }
                    return recovered;
                }
                self.install_vault(vault, next);
                self.message = "Vault reloaded".into();
                true
            }
            Err(error) => {
                let warning = format!("Reload failed · {error:#}");
                if self.vault_warning.as_ref() == Some(&warning) {
                    return false;
                }
                self.vault_warning = Some(warning);
                self.invalidate_daemon_views();
                true
            }
        }
    }

    fn install_vault(&mut self, vault: &mut Vault, next: Vault) {
        let preferred = self
            .selected_focus(vault)
            .or_else(|| self.document_id.map(FocusIdentity::Document));
        *vault = next;
        *self.graph_nodes.get_mut() = None;
        self.invalidate_daemon_views();
        self.document_id = preferred.map(FocusIdentity::document_id);
        self.sync_row(vault, preferred);
        if let Some(selected) = &mut self.graph_selection {
            selected.retain(|&id| vault.is_work_unit(id));
        }
    }

    fn apply_proposal(&mut self, root: &Path, vault: &mut Vault, paths: &WorkspacePaths, id: Id) {
        let zone = vault.timezone();
        // The CLI re-reads and displays the exact record before accepting
        // confirmation. The queue verifies that record under its lock.
        let apply = crate::proposal::run(crate::proposal::Command::Apply { id }, root, zone, paths);
        if let Err(error) = apply {
            self.message = format!("Proposal apply failed · {error:#}");
            return;
        }

        self.reload(root, vault);
        let mut message = format!("Applied proposal {id}");
        if self.message != "Vault reloaded" {
            message.push_str(" · ");
            message.push_str(&self.message);
        }
        self.proposals.detail = None;
        if let Err(error) =
            self.proposals
                .execute(root, zone, paths, proposal_review::Request::List)
        {
            // The write succeeded even if the queue could not be refreshed.
            self.proposals.rows.clear();
            self.proposals.row = 0;
            message.push_str(&format!(" · Proposal list refresh failed · {error:#}"));
        }
        self.message = message;
    }

    fn set_graph_dependencies(&mut self, root: &Path, vault: &mut Vault, edit: graph::Submission) {
        match VaultCommands::new(root, vault.timezone()).set_work_unit_dependencies(
            edit.target,
            edit.prerequisites,
            edit.revision,
        ) {
            Ok(_) => {
                self.graph_selection = None;
                self.reload(root, vault);
                if self.message == "Vault reloaded" {
                    self.message = "Dependencies updated".into();
                }
            }
            Err(WriteError::StaleRevision { .. }) => {
                self.message = "WorkUnit changed · r to reload, then retry".into();
            }
            Err(error) => self.message = format!("Graph edit failed · {error:#}"),
        }
    }

    fn set_deadline(&mut self, root: &Path, vault: &mut Vault, request: deadline::Submission) {
        match VaultCommands::new(root, vault.timezone()).set_deadline(
            request.id,
            request.deadline,
            request.revision,
        ) {
            Ok(_) => {
                self.overlay = Overlay::None;
                self.reload(root, vault);
                if self.message == "Vault reloaded" {
                    self.message = "Deadline saved".into();
                }
            }
            Err(WriteError::StaleRevision { .. }) => {
                self.message =
                    "Document changed · reload the Vault, then press r to refresh the revision"
                        .into();
            }
            Err(WriteError::PublishedButNotSynced { source, .. }) => {
                self.overlay = Overlay::None;
                self.reload(root, vault);
                self.message = format!("Deadline published · directory sync failed: {source}");
            }
            Err(error) => self.message = format!("Deadline update failed · {error:#}"),
        }
    }
}

fn project_rows(vault: &Vault) -> Vec<(Id, String)> {
    project_outline(vault)
        .into_iter()
        .filter(|row| row.depth == 0)
        .map(|row| (row.id, row.title))
        .collect()
}

fn move_row(row: usize, down: bool, len: usize) -> usize {
    if down {
        row.saturating_add(1).min(len.saturating_sub(1))
    } else {
        row.saturating_sub(1)
    }
}

impl State {
    fn open_settings(&mut self, paths: &WorkspacePaths) {
        self.message.clear();
        self.overlay = Overlay::Settings(settings::Form::load(paths));
    }

    fn apply_settings(
        &mut self,
        command: settings::Command,
        paths: &WorkspacePaths,
        active_timezone: chrono_tz::Tz,
    ) -> Option<TuiTheme> {
        let Overlay::Settings(form) = &mut self.overlay else {
            unreachable!("Settings commands originate from the Settings overlay");
        };
        match command {
            settings::Command::OpenKeyboard => {
                form.subpage = Some(settings::Subpage::Keyboard(settings::ShortcutForm::load(
                    paths,
                )));
                return None;
            }
            settings::Command::OpenAgents => {
                form.subpage = Some(settings::Subpage::Agents(settings::AgentPage::load(paths)));
                return None;
            }
            #[cfg(target_os = "linux")]
            settings::Command::OpenCodex => {
                self.message.clear();
                form.subpage = Some(settings::Subpage::Codex(settings::CodexForm::load(paths)));
                return None;
            }
            settings::Command::Reload => {
                self.message = settings::reload_feedback(form.reload(paths));
                return None;
            }
            settings::Command::Save => {}
        }

        match &mut form.subpage {
            Some(settings::Subpage::Keyboard(Ok(shortcuts))) => {
                self.message = match shortcuts.save(paths) {
                    Ok(written) => {
                        self.keymap = shortcuts.draft.clone();
                        form.subpage = None;
                        if written {
                            "Key bindings saved"
                        } else {
                            "Key bindings unchanged"
                        }
                        .into()
                    }
                    Err(error) => settings::save_error(&error),
                };
                return None;
            }
            Some(settings::Subpage::Keyboard(Err(_))) => return None,
            #[cfg(target_os = "linux")]
            Some(settings::Subpage::Agents(page)) => {
                self.message = match page.save(paths) {
                    Ok(message) => message.into(),
                    Err(error) => settings::save_error(&error),
                };
                return None;
            }
            #[cfg(not(target_os = "linux"))]
            Some(settings::Subpage::Agents(_)) => return None,
            #[cfg(target_os = "linux")]
            Some(settings::Subpage::Codex(draft)) => {
                if let Ok(draft) = draft {
                    self.message = match draft.save(paths) {
                        Ok(written) => {
                            form.subpage =
                                Some(settings::Subpage::Agents(settings::AgentPage::load(paths)));
                            if written {
                                "Codex settings saved · Used on the next attempt"
                            } else {
                                "Codex settings unchanged"
                            }
                            .into()
                        }
                        Err(error) => settings::save_error(&error),
                    };
                }
                return None;
            }
            None => {}
        }

        let preferences = form.preferences.as_mut().ok()?;
        let worker_limit_changed = preferences.worker_limit_changed();
        let (updated, written) = match preferences.save(paths) {
            Ok(saved) => saved,
            Err(error) => {
                self.message = settings::save_error(&error);
                return None;
            }
        };
        self.message = if written {
            "Settings saved"
        } else {
            "Settings unchanged"
        }
        .into();
        if updated.timezone != active_timezone {
            self.message.push_str(if self.has_timezone_override {
                " · Restart without --timezone to apply"
            } else {
                " · Restart for the new timezone"
            });
        }
        if worker_limit_changed {
            self.message.push_str(" · Restart daemon for Worker slots");
        }
        self.home_view = updated.home_view;
        self.overlay = Overlay::None;
        Some(updated.tui_theme)
    }
}

pub(super) fn repair_config(
    paths: &WorkspacePaths,
    snapshot: AppConfigSnapshot,
) -> Result<Option<AppConfig>> {
    require_terminal().with_context(|| format!("Invalid {}", paths.config_path().display()))?;
    let mut preferences = settings::Preferences::from_snapshot(snapshot);
    let theme = theme::Theme::for_session(TuiTheme::Default, no_color_requested());
    let mut feedback = "Invalid config.toml".to_owned();
    let (_screen, mut terminal) = Screen::enter()?;
    loop {
        let model = screen_model::ScreenModel::repair_settings(&preferences, &feedback);
        terminal.draw(|frame| render::draw(frame, &model, theme))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                return Ok(None);
            }
            continue;
        }
        match settings::Input::editor_key(key.code) {
            Some(settings::Input::Close) => return Ok(None),
            Some(settings::Input::Command(settings::Command::Reload)) => {
                feedback = settings::reload_feedback(preferences.reload(paths));
            }
            Some(settings::Input::Command(settings::Command::Save)) => {
                match preferences.save(paths) {
                    Ok((config, _)) => return Ok(Some(config)),
                    Err(error) => feedback = settings::save_error(&error),
                }
            }
            _ => preferences.edit(key.code),
        }
    }
}

fn require_terminal() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "the TUI requires an interactive terminal"
    );
    Ok(())
}

fn no_color_requested() -> bool {
    env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

pub(super) fn run(
    root: &Path,
    mut vault: Vault,
    day: NaiveDate,
    paths: &WorkspacePaths,
    config: AppConfig,
    has_timezone_override: bool,
) -> Result<()> {
    require_terminal()?;
    let mut state = State::new(day);
    if creation::is_daemon_vault(paths, root)? {
        state.daemon_views = Some(daemon_views::DaemonViews::new(paths.clone()));
    }
    state.has_timezone_override = has_timezone_override;
    state.no_color = no_color_requested();
    let mut theme = theme::Theme::for_session(config.tui_theme, state.no_color);
    state.home_view = config.home_view;
    let restored_view = initial_view(paths, config.home_view);
    if let Ok(view) = &restored_view {
        state.view = *view;
    }
    if state.view == View::Workflow
        && let Action::Workflow(request) = state.workflow.open()
    {
        state.message = state.workflow.execute(paths, request);
    }
    let mut watcher = match watch::Watcher::new(root) {
        Ok(watcher) => {
            // Close the gap between the initial CLI load and watch registration.
            state.refresh_external(root, &mut vault);
            Some(watcher)
        }
        Err(error) => {
            state.message = format!("Auto-reload unavailable · {error:#} · r reload");
            None
        }
    };
    match keymap::Keymap::load_config(paths) {
        Ok(keymap) => state.keymap = keymap,
        Err(_) => state.message = "Cannot load keymap · Default keys active · Settings F2".into(),
    }
    if restored_view.is_err() {
        if !state.message.is_empty() {
            state.message.push_str(" · ");
        }
        state
            .message
            .push_str("Saved view unavailable · Showing Month");
    }
    loop {
        let (screen, mut terminal) = Screen::enter()?;
        let mut redraw = true;
        let handoff = loop {
            if let Some(active) = watcher.as_mut() {
                match active.changed() {
                    Ok(true) => redraw |= state.refresh_external(root, &mut vault),
                    Ok(false) => {}
                    Err(error) => {
                        state.message = format!("Auto-reload unavailable · {error:#} · r reload");
                        watcher = None;
                        redraw = true;
                    }
                }
            }
            if redraw {
                let model = screen_model::ScreenModel::project(&state, &vault);
                terminal.draw(|frame| render::draw(frame, &model, theme))?;
                redraw = false;
            }
            // crossterm owns terminal input and resize notifications. Give the
            // nonblocking inotify queue time even when no keys are pressed.
            if watcher.is_some() && !event::poll(Duration::from_millis(50))? {
                continue;
            }
            redraw = true;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match state.handle(key, &vault) {
                Action::Continue => {}
                Action::Quit => break TerminalHandoff::Quit,
                Action::Reload => state.reload(root, &mut vault),
                Action::OpenSettings => state.open_settings(paths),
                Action::Settings(command) => {
                    if let Some(palette) = state.apply_settings(command, paths, vault.timezone()) {
                        theme = theme::Theme::for_session(palette, state.no_color);
                    }
                }
                Action::Timer(operation) => {
                    state.message = match crate::timer(paths, operation) {
                        Ok(status) => crate::timer_label(&status),
                        Err(error) => format!("Timer failed · {error:#}"),
                    };
                }
                Action::CreateCalendar(request) => {
                    state.create_calendar(root, &mut vault, paths, request);
                }
                Action::CreateReminder(request) => {
                    state.create_reminder(root, &mut vault, paths, request);
                }
                Action::CreateTask(request) => {
                    state.create_task(root, &mut vault, paths, request);
                }
                Action::CreateTimeBlock(block) => {
                    state.create_time_block(root, &mut vault, paths, block);
                }
                Action::SetDeadline(request) => {
                    state.set_deadline(root, &mut vault, request);
                }
                Action::SetGraphDependencies(request) => {
                    state.set_graph_dependencies(root, &mut vault, request);
                }
                Action::Proposals(request) => {
                    state.message =
                        match state
                            .proposals
                            .execute(root, vault.timezone(), paths, request)
                        {
                            Ok(()) if let proposal_review::Request::Reject(id) = request => {
                                format!("Rejected proposal {id}")
                            }
                            Ok(()) => String::new(),
                            Err(error) => format!("Proposal review failed · {error:#}"),
                        };
                }
                Action::ApplyProposal(id) => break TerminalHandoff::ApplyProposal(id),
                Action::Workflow(command) => {
                    state.message = state.workflow.execute(paths, command);
                }
                Action::TriageInbox {
                    inbox_id,
                    project_id,
                    revision,
                } => state.triage_inbox(root, &mut vault, inbox_id, project_id, revision),
                Action::Edit => {
                    let Some(path) = state.source(&vault) else {
                        state.message =
                            "Select a document in Day, Agenda, Outline or Graph to edit".into();
                        continue;
                    };
                    break TerminalHandoff::Edit(path);
                }
            }
        };
        drop(terminal);
        drop(screen);
        match handoff {
            TerminalHandoff::Quit => {
                if state.home_view == HomeViewPolicy::RestoreLastView
                    && let Err(error) = paths.write_state_file(
                        &paths.last_view_path(),
                        state.view.saved_name().as_bytes(),
                    )
                {
                    eprintln!("mcbtodo: could not save the last view: {error:#}");
                }
                return Ok(());
            }
            TerminalHandoff::ApplyProposal(id) => {
                state.apply_proposal(root, &mut vault, paths, id);
            }
            TerminalHandoff::Edit(path) => {
                let edit_result = edit(path);
                state.reload(root, &mut vault);
                if let Err(error) = edit_result {
                    state.message = format!("Editor failed · {error:#} · {}", state.message);
                }
            }
        }
    }
}

struct Screen;

impl Screen {
    fn enter() -> io::Result<(Self, DefaultTerminal)> {
        enable_raw_mode()?;
        let screen = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok((screen, terminal))
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

fn edit(path: &Path) -> Result<()> {
    let editor = env::var_os("VISUAL")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("EDITOR").filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "hx".into());
    let status = Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("starting {}", editor.to_string_lossy()))?;
    ensure!(status.success(), "editor exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    pub(super) fn fixture() -> Vault {
        Vault::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault"),
            "Asia/Shanghai".parse().unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn settings_save_shortcuts_and_update_the_live_hints_and_input() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let vault = Vault::load(home.path(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let screen = |state: &State, width, height| -> Result<String> {
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            let model = screen_model::ScreenModel::project(state, &vault);
            let frame =
                terminal.draw(|frame| render::draw(frame, &model, theme::Theme::default()))?;
            Ok(frame
                .buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect())
        };
        let open = |state: &mut State| {
            assert_eq!(
                state.handle(key(KeyCode::F(2)), &vault),
                Action::Settings(settings::Command::OpenKeyboard)
            );
            state.apply_settings(settings::Command::OpenKeyboard, &paths, vault.timezone());
        };
        let save = |state: &mut State| {
            assert_eq!(
                state.handle(key(KeyCode::Enter), &vault),
                Action::Settings(settings::Command::Save)
            );
            state.apply_settings(settings::Command::Save, &paths, vault.timezone());
        };

        state.open_settings(&paths);
        assert!(screen(&state, 24, 5)?.contains("F2"));
        state.handle(key(KeyCode::Char('x')), &vault);
        open(&mut state);
        assert!(screen(&state, 24, 5)?.contains("> Quit q"));
        save(&mut state);
        assert_eq!(state.message, "Key bindings unchanged");
        assert!(!paths.keymap_path().exists());
        assert!(!paths.keymap_path().with_extension("lock").exists());

        open(&mut state);
        state.handle(key(KeyCode::Char('Q')), &vault);
        state.handle(key(KeyCode::Esc), &vault);
        assert!(!paths.keymap_path().exists());

        open(&mut state);
        state.handle(key(KeyCode::Char('r')), &vault);
        save(&mut state);
        assert!(state.message.contains("assigned"));
        assert!(!paths.keymap_path().exists());
        state.handle(key(KeyCode::Char('Q')), &vault);
        save(&mut state);
        assert!(screen(&state, 24, 5)?.contains("> TZ UTCx"));
        assert!(!paths.config_path().exists());
        assert!(std::fs::read_to_string(paths.keymap_path())?.contains("quit = \"Q\""));

        let edited = "# An unrelated comment\n[keys]\nquit = 'Q'\n";
        std::fs::write(paths.keymap_path(), edited)?;
        open(&mut state);
        save(&mut state);
        assert_eq!(state.message, "Key bindings unchanged");
        assert_eq!(std::fs::read_to_string(paths.keymap_path())?, edited);

        open(&mut state);
        state.handle(key(KeyCode::Char('0')), &vault);
        let intervening = "# Edited while Settings was open\n[keys]\nquit = 'Q'\n";
        std::fs::write(paths.keymap_path(), intervening)?;
        save(&mut state);
        assert_eq!(state.message, "Config changed on disk · F5 reload");
        assert_eq!(std::fs::read_to_string(paths.keymap_path())?, intervening);
        assert_eq!(
            state.handle(key(KeyCode::F(5)), &vault),
            Action::Settings(settings::Command::Reload)
        );
        state.apply_settings(settings::Command::Reload, &paths, vault.timezone());
        assert!(screen(&state, 24, 5)?.contains("> Quit Q"));
        state.handle(key(KeyCode::Delete), &vault);
        assert!(screen(&state, 24, 5)?.contains("> Quit q"));
        state.handle(key(KeyCode::Esc), &vault);
        assert!(screen(&state, 24, 5)?.contains("> TZ UTCx"));
        state.handle(key(KeyCode::Esc), &vault);
        state.handle(key(KeyCode::Esc), &vault);
        state.handle(key(KeyCode::Char('?')), &vault);
        screen(&state, 80, 34)?;
        state.handle(key(KeyCode::End), &vault);
        let help = screen(&state, 80, 34)?;
        assert!(help.contains("Q   Quit"), "{help}");
        state.handle(key(KeyCode::Esc), &vault);
        assert_eq!(state.handle(key(KeyCode::Char('Q')), &vault), Action::Quit);
        assert_eq!(
            state.handle(key(KeyCode::Char('0')), &vault),
            Action::Continue
        );
        Ok(())
    }

    #[test]
    fn settings_repairs_an_invalid_keymap_only_on_save() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let path = paths.keymap_path();
        let invalid = "[keys]\nquit = 'r'\n";
        std::fs::write(&path, invalid)?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        state.open_settings(&paths);

        let open = |state: &mut State| {
            assert_eq!(
                state.handle(key(KeyCode::F(2)), &vault),
                Action::Settings(settings::Command::OpenKeyboard)
            );
            state.apply_settings(settings::Command::OpenKeyboard, &paths, vault.timezone());
        };
        open(&mut state);
        let model = screen_model::ScreenModel::project(&state, &vault);
        let mut terminal = Terminal::new(TestBackend::new(64, 10))?;
        let screen: String = terminal
            .draw(|frame| render::draw(frame, &model, theme::Theme::default()))?
            .buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("Invalid keymap"), "{screen}");
        assert!(screen.contains("> Quit q"), "{screen}");
        assert!(screen.contains("Enter replace"), "{screen}");
        state.handle(key(KeyCode::Esc), &vault);
        assert_eq!(std::fs::read_to_string(&path)?, invalid);

        open(&mut state);
        let intervening = "[keys]\nquit = 'Q'\n";
        std::fs::write(&path, intervening)?;
        state.apply_settings(settings::Command::Save, &paths, vault.timezone());
        assert!(state.message.contains("changed on disk"));
        assert_eq!(std::fs::read_to_string(&path)?, intervening);
        state.handle(key(KeyCode::Esc), &vault);

        std::fs::write(&path, invalid)?;
        open(&mut state);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        state.apply_settings(settings::Command::Save, &paths, vault.timezone());
        assert_eq!(state.message, "Key bindings saved");
        assert_eq!(
            keymap::Keymap::load_config(&paths)?,
            keymap::Keymap::default()
        );
        assert_eq!(state.keymap, keymap::Keymap::default());
        Ok(())
    }

    #[test]
    fn keyboard_read_error_keeps_preferences_draft_and_retries_in_place() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let config_before = std::fs::read(paths.config_path())?;
        let path = paths.keymap_path();
        std::fs::create_dir(&path)?;

        state.open_settings(&paths);
        state.handle(key(KeyCode::Char('x')), &vault);
        assert_eq!(
            state.handle(key(KeyCode::F(2)), &vault),
            Action::Settings(settings::Command::OpenKeyboard)
        );
        state.apply_settings(settings::Command::OpenKeyboard, &paths, vault.timezone());

        let mut terminal = Terminal::new(TestBackend::new(64, 10))?;
        let render_settings = |terminal: &mut Terminal<TestBackend>, state: &State| {
            let model = screen_model::ScreenModel::project(state, &vault);
            let frame =
                terminal.draw(|frame| render::draw(frame, &model, theme::Theme::default()))?;
            Ok::<String, anyhow::Error>(frame.buffer.content.iter().map(|c| c.symbol()).collect())
        };
        let screen = render_settings(&mut terminal, &state)?;
        assert!(screen.contains("Cannot read keymap.toml"), "{screen}");
        assert!(screen.contains("F5 retry"), "{screen}");
        assert_eq!(state.handle(key(KeyCode::Enter), &vault), Action::Continue);
        assert_eq!(std::fs::read(paths.config_path())?, config_before);
        assert!(path.is_dir());

        assert_eq!(
            state.handle(key(KeyCode::F(5)), &vault),
            Action::Settings(settings::Command::Reload)
        );
        state.apply_settings(settings::Command::Reload, &paths, vault.timezone());
        assert!(state.message.contains("Cannot reload settings"));
        assert!(render_settings(&mut terminal, &state)?.contains("Cannot read keymap.toml"));

        std::fs::remove_dir(&path)?;
        assert_eq!(
            state.handle(key(KeyCode::F(5)), &vault),
            Action::Settings(settings::Command::Reload)
        );
        state.apply_settings(settings::Command::Reload, &paths, vault.timezone());
        assert!(render_settings(&mut terminal, &state)?.contains("> Quit q"));
        state.handle(key(KeyCode::Char('Q')), &vault);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        state.apply_settings(settings::Command::Save, &paths, vault.timezone());
        assert!(render_settings(&mut terminal, &state)?.contains("> TZ UTCx"));
        assert_eq!(std::fs::read(paths.config_path())?, config_before);
        assert!(std::fs::read_to_string(&path)?.contains("quit = \"Q\""));
        Ok(())
    }

    #[test]
    fn configured_printable_keys_never_remap_control_c() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        std::fs::write(paths.keymap_path(), "[keys]\nreload = 'c'\nquit = 'Q'\n")?;
        let vault = Vault::load(home.path(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        state.keymap = keymap::Keymap::load_config(&paths)?;

        assert_eq!(
            state.handle(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
                &vault
            ),
            Action::Reload
        );
        assert_eq!(
            state.handle(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &vault
            ),
            Action::Quit
        );
        Ok(())
    }

    #[test]
    fn settings_validate_save_and_discard_drafts() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let vault = Vault::load(home.path(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            state.handle(key(KeyCode::Char(' ')), &vault),
            Action::Continue
        );
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::OpenSettings
        );
        state.open_settings(&paths);
        assert_eq!(state.handle(key(KeyCode::Delete), &vault), Action::Continue);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            None
        );
        assert_eq!(state.message, "Invalid IANA timezone");
        assert!(!paths.config_path().exists());
        for code in "Asia/Shanghai"
            .chars()
            .map(KeyCode::Char)
            .chain([KeyCode::Tab, KeyCode::Right])
        {
            assert_eq!(state.handle(key(code), &vault), Action::Continue);
        }
        let config = AppConfig {
            timezone: chrono_tz::Asia::Shanghai,
            home_view: HomeViewPolicy::RestoreLastView,
            ..AppConfig::default()
        };
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            Some(config.tui_theme)
        );
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!(
            state.message,
            "Settings saved · Restart for the new timezone"
        );
        assert_eq!(state.home_view, config.home_view);
        assert_eq!(paths.load_config()?, config);

        for code in [KeyCode::Char(':'), KeyCode::Char('s')]
            .into_iter()
            .chain("ettings".chars().map(KeyCode::Char))
        {
            assert_eq!(state.handle(key(code), &vault), Action::Continue);
        }
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::OpenSettings
        );
        state.open_settings(&paths);
        for code in [KeyCode::Delete, KeyCode::Esc] {
            assert_eq!(state.handle(key(code), &vault), Action::Continue);
        }
        assert!(matches!(state.overlay, Overlay::Space(_)));
        assert_eq!(state.home_view, config.home_view);
        assert_eq!(paths.load_config()?, config);
        assert_eq!(vault.timezone(), chrono_tz::UTC);
        Ok(())
    }

    #[test]
    fn settings_restart_feedback_tracks_the_active_vault_timezone() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let save = |state: &mut State, keys: &[KeyCode]| {
            state.open_settings(&paths);
            for &code in keys {
                assert_eq!(
                    state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault),
                    Action::Continue
                );
            }
            assert_eq!(
                state.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &vault),
                Action::Settings(settings::Command::Save)
            );
            state.apply_settings(settings::Command::Save, &paths, vault.timezone());
        };
        let timezone_keys = |name: &str| {
            [KeyCode::Delete]
                .into_iter()
                .chain(name.chars().map(KeyCode::Char))
                .collect::<Vec<_>>()
        };

        save(&mut state, &timezone_keys("Asia/Shanghai"));
        assert_eq!(
            state.message,
            "Settings saved · Restart for the new timezone"
        );
        save(&mut state, &[KeyCode::Tab, KeyCode::Tab, KeyCode::Right]);
        assert_eq!(
            state.message,
            "Settings saved · Restart for the new timezone"
        );
        save(&mut state, &[]);
        assert_eq!(
            state.message,
            "Settings unchanged · Restart for the new timezone"
        );
        save(&mut state, &timezone_keys("UTC"));
        assert_eq!(state.message, "Settings saved");
        assert_eq!(paths.load_config()?.timezone, vault.timezone());
        Ok(())
    }

    #[test]
    fn reloading_settings_after_a_conflict_edits_the_new_values_in_place() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let initial = paths.initialize_config()?;
        let vault = Vault::load(&paths.vault_root(), initial.timezone)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        state.open_settings(&paths);
        state.handle(key(KeyCode::Tab), &vault);
        state.handle(key(KeyCode::Right), &vault);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        let edited = "timezone = 'Asia/Tokyo'\n";
        std::fs::write(paths.config_path(), edited)?;
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            None
        );
        assert_eq!(state.message, "Config changed on disk · F5 reload");
        assert!(matches!(state.overlay, Overlay::Settings(_)));
        assert_eq!(std::fs::read_to_string(paths.config_path())?, edited);
        assert_eq!(state.home_view, HomeViewPolicy::Month);

        let config_path = paths.config_path();
        std::fs::remove_file(&config_path)?;
        std::fs::create_dir(&config_path)?;
        state.apply_settings(settings::Command::Reload, &paths, vault.timezone());
        assert!(state.message.starts_with("Cannot reload settings"));
        let Overlay::Settings(form) = &state.overlay else {
            panic!("an unreadable config should keep the current draft");
        };
        let preferences = form.preferences.as_ref().expect("loaded draft");
        assert_eq!(preferences.row(preferences.field()).1, "Restore last view");
        std::fs::remove_dir(&config_path)?;
        std::fs::write(&config_path, edited)?;

        assert_eq!(
            state.handle(key(KeyCode::F(5)), &vault),
            Action::Settings(settings::Command::Reload)
        );
        state.apply_settings(settings::Command::Reload, &paths, vault.timezone());
        let Overlay::Settings(form) = &state.overlay else {
            panic!("reloading should stay in Settings");
        };
        let preferences = form.preferences.as_ref().expect("reloaded draft");
        assert!(preferences.field() == settings::Field::HomeView);
        assert_eq!(preferences.row(preferences.field()).1, "Month");
        assert_eq!(preferences.row(settings::Field::Timezone).1, "Asia/Tokyo");
        state.handle(key(KeyCode::Right), &vault);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            Some(initial.tui_theme)
        );
        assert_eq!(paths.load_config()?.timezone, chrono_tz::Asia::Tokyo);
        assert_eq!(
            paths.load_config()?.home_view,
            HomeViewPolicy::RestoreLastView
        );
        assert_eq!(state.home_view, HomeViewPolicy::RestoreLastView);
        assert_eq!(vault.timezone(), chrono_tz::UTC);
        Ok(())
    }

    #[test]
    fn settings_pages_work_when_preferences_cannot_be_read() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let config = paths.config_path();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        std::fs::remove_file(&config)?;
        std::fs::create_dir(&config)?;
        state.open_settings(&paths);
        let mut terminal = Terminal::new(TestBackend::new(64, 12))?;
        let model = screen_model::ScreenModel::project(&state, &vault);
        let buffer = terminal
            .draw(|frame| render::draw(frame, &model, theme::Theme::default()))?
            .buffer;
        let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("Cannot read config.toml"));
        assert!(rendered.contains("F2 keys  F3 agents  F5 retry"));
        assert_eq!(state.handle(key(KeyCode::Enter), &vault), Action::Continue);

        assert_eq!(
            state.handle(key(KeyCode::F(2)), &vault),
            Action::Settings(settings::Command::OpenKeyboard)
        );
        state.apply_settings(settings::Command::OpenKeyboard, &paths, vault.timezone());
        state.handle(key(KeyCode::Char('Q')), &vault);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        state.apply_settings(settings::Command::Save, &paths, vault.timezone());
        assert_eq!(state.message, "Key bindings saved");
        assert!(std::fs::read_to_string(paths.keymap_path())?.contains("quit = \"Q\""));
        assert!(config.is_dir());

        assert_eq!(
            state.handle(key(KeyCode::F(3)), &vault),
            Action::Settings(settings::Command::OpenAgents)
        );
        state.apply_settings(settings::Command::OpenAgents, &paths, vault.timezone());
        assert_eq!(state.handle(key(KeyCode::Enter), &vault), Action::Continue);
        state.handle(key(KeyCode::Esc), &vault);

        std::fs::remove_dir(&config)?;
        let fixed = "timezone = 'Asia/Tokyo'\n";
        std::fs::write(&config, fixed)?;
        assert_eq!(
            state.handle(key(KeyCode::F(5)), &vault),
            Action::Settings(settings::Command::Reload)
        );
        state.apply_settings(settings::Command::Reload, &paths, vault.timezone());
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            Some(TuiTheme::Default)
        );
        assert_eq!(std::fs::read_to_string(&config)?, fixed);
        assert_eq!(state.handle(key(KeyCode::Char('Q')), &vault), Action::Quit);
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn agent_settings_change_default_for_inherited_routes_only() -> Result<()> {
        use mcbtodo_platform::agent_policy::AgentBackend::{Codex, UnixSocket};

        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let revision = "a".repeat(64);
        let original = format!(
            "provider = 'unix_socket'\ntimeout_seconds = 42\nmax_output_bytes = 900\n\
             [providers]\nunix_socket = 'state/local.sock'\ncodex = 'state/model.sock'\n\
             [[allow]]\ncapability = 'summarize'\nrevision = '{revision}'\n\
             [[allow]]\ncapability = 'review'\nrevision = '{revision}'\nprovider = 'unix_socket'\n"
        );
        let path = paths.agent_policy_path();
        std::fs::write(&path, &original)?;
        let press = |state: &mut State, code| {
            if let Action::Settings(command) =
                state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault)
            {
                state.apply_settings(command, &paths, vault.timezone());
            }
        };

        state.open_settings(&paths);
        press(&mut state, KeyCode::F(3));
        press(&mut state, KeyCode::Enter);
        assert_eq!(std::fs::read_to_string(&path)?, original);
        press(&mut state, KeyCode::Down);
        assert_eq!(std::fs::read_to_string(&path)?, original);

        let intervening = format!("{original}# external edit\n");
        std::fs::write(&path, &intervening)?;
        press(&mut state, KeyCode::Enter);
        assert!(state.message.contains("F5 reload"));
        assert_eq!(std::fs::read_to_string(&path)?, intervening);

        press(&mut state, KeyCode::F(5));
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Enter);
        let saved = paths.load_agent_policy()?.context("missing saved policy")?;
        assert_eq!(saved.backend(), Codex);
        assert_eq!(saved.timeout().as_secs(), 42);
        assert_eq!(saved.max_output_bytes(), Some(900));
        assert_eq!(saved.grant_count(Codex), 1);
        assert_eq!(saved.grant_count(UnixSocket), 1);
        let route = saved
            .route("summarize", &revision)
            .context("missing inherited route")?;
        assert_eq!(route.backend, Codex);
        assert_eq!(route.socket, std::path::Path::new("state/model.sock"));
        let route = saved
            .route("review", &revision)
            .context("missing explicit route")?;
        assert_eq!(route.backend, UnixSocket);
        assert_eq!(route.socket, std::path::Path::new("state/local.sock"));
        assert!(saved.route("summarize", &"b".repeat(64)).is_none());

        press(&mut state, KeyCode::Up);
        let mut terminal = Terminal::new(TestBackend::new(24, 9))?;
        let model = screen_model::ScreenModel::project(&state, &vault);
        let buffer = terminal
            .draw(|frame| render::draw(frame, &model, theme::Theme::default()))?
            .buffer;
        let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("> unix_socket 1 grant"), "{rendered}");
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn agent_settings_edit_only_the_selected_endpoint() -> Result<()> {
        use mcbtodo_platform::agent_policy::AgentBackend::Codex;

        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let revision = "a".repeat(64);
        let original = format!(
            "provider = 'codex'\ntimeout_seconds = 42\nmax_output_bytes = 900\n\
             [providers]\nunix_socket = 'state/local.sock'\ncodex = 'state/model.sock'\n\
             [[allow]]\ncapability = 'summarize'\nrevision = '{revision}'\n\
             [[allow]]\ncapability = 'review'\nrevision = '{revision}'\nprovider = 'unix_socket'\n"
        );
        let path = paths.agent_policy_path();
        std::fs::write(&path, &original)?;
        let press = |state: &mut State, code| {
            if let Action::Settings(command) =
                state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault)
            {
                state.apply_settings(command, &paths, vault.timezone());
            }
        };
        let edit = |state: &mut State, text: &str| {
            press(state, KeyCode::Delete);
            for character in text.chars() {
                press(state, KeyCode::Char(character));
            }
        };

        state.open_settings(&paths);
        press(&mut state, KeyCode::F(3));
        press(&mut state, KeyCode::Up);
        press(&mut state, KeyCode::F(6));
        let mut terminal = Terminal::new(TestBackend::new(24, 9))?;
        let model = screen_model::ScreenModel::project(&state, &vault);
        let buffer = terminal
            .draw(|frame| render::draw(frame, &model, theme::Theme::default()))?
            .buffer;
        let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("state/local.sock"), "{rendered}");
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.message, "Agent endpoint unchanged");
        assert_eq!(std::fs::read_to_string(&path)?, original);

        press(&mut state, KeyCode::F(6));
        edit(&mut state, "state/discard.sock");
        press(&mut state, KeyCode::Esc);
        assert_eq!(std::fs::read_to_string(&path)?, original);

        press(&mut state, KeyCode::F(6));
        edit(&mut state, "../outside.sock");
        press(&mut state, KeyCode::Enter);
        assert!(state.message.contains("relative path"));
        assert_eq!(std::fs::read_to_string(&path)?, original);

        edit(&mut state, "state/review.sock");
        let external = format!("{original}# comment-only external edit\n");
        std::fs::write(&path, &external)?;
        press(&mut state, KeyCode::Enter);
        assert!(state.message.contains("F5 reload"));
        assert_eq!(std::fs::read_to_string(&path)?, external);

        let removed = "[providers]\ncodex = 'state/model.sock'\n";
        std::fs::write(&path, removed)?;
        press(&mut state, KeyCode::F(5));
        assert!(state.message.contains("no longer configured"));
        let Overlay::Settings(form) = &state.overlay else {
            panic!("reload should keep the endpoint editor open");
        };
        let Some(settings::Subpage::Agents(page)) = &form.subpage else {
            panic!("reload should keep Agents open");
        };
        assert_eq!(
            page.endpoint_draft(),
            Some(("unix_socket", "state/review.sock"))
        );
        assert_eq!(std::fs::read_to_string(&path)?, removed);
        std::fs::write(&path, &external)?;

        press(&mut state, KeyCode::F(5));
        edit(&mut state, "state/review.sock");
        press(&mut state, KeyCode::Enter);

        let saved = paths.load_agent_policy()?.context("missing saved policy")?;
        assert_eq!(saved.backend(), Codex);
        assert_eq!(saved.timeout().as_secs(), 42);
        assert_eq!(saved.max_output_bytes(), Some(900));
        assert_eq!(
            saved.route("summarize", &revision).unwrap().socket,
            std::path::Path::new("state/model.sock")
        );
        assert_eq!(
            saved.route("review", &revision).unwrap().socket,
            std::path::Path::new("state/review.sock")
        );
        assert!(saved.route("review", &"b".repeat(64)).is_none());
        assert!(!paths.codex_provider_path().exists());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn codex_settings_reject_stale_and_invalid_drafts_without_changing_policy() -> Result<()> {
        use mcbtodo_execution::codex_config::{CodexConfig, ScalarType};

        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let policy = "[providers]\nunix_socket = 'state/agent.sock'\n";
        std::fs::write(paths.agent_policy_path(), policy)?;
        let path = paths.codex_provider_path();
        let original = "executable = '/optional/codex'\ncapability = 'summary'\n\
                        instructions = '整理 tasks'\n[outputs]\ntitle = 'string'\n";

        let press = |state: &mut State, code| {
            if let Action::Settings(command) =
                state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault)
            {
                state.apply_settings(command, &paths, vault.timezone());
            }
        };
        let edit_outputs = |state: &mut State, text: &str| {
            press(state, KeyCode::Delete);
            for character in text.chars() {
                press(state, KeyCode::Char(character));
            }
        };
        state.open_settings(&paths);
        press(&mut state, KeyCode::F(3));
        press(&mut state, KeyCode::F(4));
        press(&mut state, KeyCode::Enter);
        assert!(
            state
                .message
                .contains("Executable must be an absolute path")
        );
        assert!(!path.exists());
        std::fs::write(&path, original)?;
        press(&mut state, KeyCode::F(5));
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.message, "Codex settings unchanged");
        assert_eq!(std::fs::read_to_string(&path)?, original);

        press(&mut state, KeyCode::F(4));
        press(&mut state, KeyCode::BackTab);
        edit_outputs(&mut state, "title=bogus");
        press(&mut state, KeyCode::Enter);
        assert!(state.message.contains("Output types"));
        assert_eq!(std::fs::read_to_string(&path)?, original);

        edit_outputs(&mut state, "title=string, count=integer");
        let external = format!("{original}# externally edited\n");
        std::fs::write(&path, &external)?;
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.message, "Config changed on disk · F5 reload");
        assert_eq!(std::fs::read_to_string(&path)?, external);
        press(&mut state, KeyCode::F(5));
        edit_outputs(&mut state, "title=string, count=integer");
        press(&mut state, KeyCode::Enter);
        assert!(state.message.starts_with("Codex settings saved"));
        let saved = CodexConfig::parse(&std::fs::read_to_string(&path)?)?;
        assert_eq!(saved.instructions, "整理 tasks");
        assert_eq!(saved.outputs["count"], ScalarType::Integer);
        assert_eq!(std::fs::read_to_string(paths.agent_policy_path())?, policy);
        Ok(())
    }

    #[test]
    fn saving_unchanged_settings_preserves_file_bytes_and_absence() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        let vault = Vault::load(home.path(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let save_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let save_unchanged = |state: &mut State| {
            state.open_settings(&paths);
            assert_eq!(
                state.handle(save_key, &vault),
                Action::Settings(settings::Command::Save)
            );
            assert_eq!(
                state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
                Some(TuiTheme::Default)
            );
            assert_eq!(state.message, "Settings unchanged");
        };

        save_unchanged(&mut state);
        assert!(!paths.config_path().exists());
        let default = paths.initialize_config()?;
        let edited = "# Keep this comment\n\ntimezone = 'UTC'\n";
        std::fs::write(paths.config_path(), edited)?;
        assert_eq!(paths.load_config()?, default);
        save_unchanged(&mut state);
        assert_eq!(std::fs::read_to_string(paths.config_path())?, edited);
        assert!(!paths.config_path().with_extension("lock").exists());
        Ok(())
    }

    #[test]
    fn preferences_conflict_on_comment_only_edit() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let mut state = State::new("2026-11-24".parse()?);
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        state.open_settings(&paths);
        for code in [KeyCode::BackTab, KeyCode::Right] {
            state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault);
        }
        let path = paths.config_path();
        let commented = format!(
            "# Edited in an external editor\n{}",
            std::fs::read_to_string(&path)?
        );
        std::fs::write(&path, &commented)?;
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            None
        );
        assert_eq!(state.message, "Config changed on disk · F5 reload");
        assert_eq!(std::fs::read_to_string(&path)?, commented);
        Ok(())
    }

    #[test]
    fn preferences_can_explicitly_replace_a_config_broken_during_the_session() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_owned());
        paths.initialize()?;
        let vault = Vault::load(&paths.vault_root(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let path = paths.config_path();
        let invalid = "timezone = 'invalid'\n";
        std::fs::write(&path, invalid)?;

        state.open_settings(&paths);
        let mut terminal = Terminal::new(TestBackend::new(24, 5))?;
        let model = screen_model::ScreenModel::project(&state, &vault);
        let buffer = terminal
            .draw(|frame| render::draw(frame, &model, theme::Theme::default()))?
            .buffer;
        assert!(buffer.content.chunks(24).any(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("Enter replace")
        }));
        state.handle(key(KeyCode::Esc), &vault);
        assert_eq!(std::fs::read_to_string(&path)?, invalid);

        state.open_settings(&paths);
        let edited = "# Edited after opening Settings\n";
        std::fs::write(&path, edited)?;
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            None
        );
        assert_eq!(std::fs::read_to_string(&path)?, edited);

        std::fs::write(&path, invalid)?;
        state.open_settings(&paths);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            Some(TuiTheme::Default)
        );
        assert_eq!(paths.load_config()?, AppConfig::default());
        Ok(())
    }

    #[test]
    fn settings_groups_preferences_and_keeps_one_focus_order() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(directory.path().to_owned());
        let vault = Vault::load(directory.path(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut terminal = Terminal::new(TestBackend::new(64, 14))?;

        state.open_settings(&paths);
        state.handle(key(KeyCode::BackTab), &vault);
        #[cfg(target_os = "linux")]
        state.handle(key(KeyCode::BackTab), &vault);
        state.handle(key(KeyCode::Right), &vault);
        let model = screen_model::ScreenModel::project(&state, &vault);
        let buffer = terminal
            .draw(|frame| render::draw(frame, &model, theme::Theme::default()))?
            .buffer;
        let rows: Vec<String> = buffer
            .content
            .chunks(64)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let row_of = |text| {
            rows.iter()
                .position(|row| row.contains(text))
                .unwrap_or_else(|| panic!("missing {text:?} in {rows:?}"))
        };
        assert!(row_of("General") < row_of("TZ UTC"));
        assert!(row_of("TZ UTC") < row_of("Home Month"));
        assert!(row_of("Home Month") < row_of("Appearance"));
        assert!(
            row_of("←/→ change") < row_of("> Theme High contrast"),
            "the header should show how to change the focused choice"
        );
        assert!(row_of("Appearance") < row_of("> Theme High contrast"));
        #[cfg(target_os = "linux")]
        {
            assert!(row_of("> Theme High contrast") < row_of("Daemon"));
            assert!(row_of("Daemon") < row_of("Worker slots 1"));
        }
        assert!(row_of("> Theme High contrast") < row_of("F5 reload"));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn settings_save_the_worker_limit_for_the_next_daemon_start() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(directory.path().to_owned());
        let vault = Vault::load(directory.path(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        state.open_settings(&paths);
        for code in [KeyCode::BackTab, KeyCode::Left, KeyCode::Right] {
            assert_eq!(state.handle(key(code), &vault), Action::Continue);
        }
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        state.apply_settings(settings::Command::Save, &paths, vault.timezone());
        assert_eq!(paths.load_config()?.worker_slots.get(), 2);
        assert_eq!(
            state.message,
            "Settings saved · Restart daemon for Worker slots"
        );
        assert!(!paths.state_dir().exists());
        Ok(())
    }

    #[test]
    fn compact_settings_keep_the_field_error_and_actions_visible() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(directory.path().to_owned());
        let vault = Vault::load(directory.path(), chrono_tz::UTC)?;
        let mut state = State::new("2026-11-24".parse()?);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut terminal = Terminal::new(TestBackend::new(24, 5))?;
        let mut draw = |state: &State| -> Result<Vec<String>> {
            let model = screen_model::ScreenModel::project(state, &vault);
            let theme = theme::Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
            Ok(terminal
                .draw(|frame| render::draw(frame, &model, theme))?
                .buffer
                .content
                .chunks(24)
                .map(|row| row.iter().map(|cell| cell.symbol()).collect())
                .collect())
        };

        state.handle(key(KeyCode::Char(' ')), &vault);
        draw(&state)?;
        state.handle(key(KeyCode::Down), &vault);
        assert!(
            draw(&state)?
                .iter()
                .any(|row| row.contains("Enter settings"))
        );
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::OpenSettings
        );
        state.open_settings(&paths);
        let rows = draw(&state)?;
        assert!(rows.iter().any(|row| row.contains("> TZ UTC")));
        assert!(rows.iter().all(|row| !row.contains("General")));
        state.handle(key(KeyCode::Tab), &vault);
        state.handle(key(KeyCode::Right), &vault);
        let rows = draw(&state)?;
        assert!(
            rows.iter()
                .any(|row| row.contains("> Home Restore last view"))
        );
        assert!(rows[4].contains("←/→ Enter Esc"), "{rows:?}");
        state.handle(key(KeyCode::Tab), &vault);
        state.handle(key(KeyCode::Right), &vault);
        let rows = draw(&state)?;
        assert!(
            rows.iter().any(|row| row.contains("> Theme High contrast")),
            "{rows:?}"
        );
        #[cfg(target_os = "linux")]
        {
            state.handle(key(KeyCode::Tab), &vault);
            let rows = draw(&state)?;
            assert!(
                rows.iter().any(|row| row.contains("> Worker slots 1")),
                "{rows:?}"
            );
        }
        state.handle(key(KeyCode::Tab), &vault);
        state.handle(key(KeyCode::Delete), &vault);
        state.handle(key(KeyCode::BackTab), &vault);
        assert_eq!(
            state.handle(key(KeyCode::Enter), &vault),
            Action::Settings(settings::Command::Save)
        );
        assert_eq!(
            state.apply_settings(settings::Command::Save, &paths, vault.timezone()),
            None
        );
        let rows = draw(&state)?;
        assert!(rows.iter().any(|row| row.contains("> TZ ▏")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("Invalid IANA timezone")),
            "{rows:?}"
        );
        assert!(rows[4].contains("Del Enter Esc"), "{rows:?}");
        Ok(())
    }

    #[test]
    fn short_help_scrolls_to_the_last_shortcut_and_returns_to_the_view() {
        let vault = fixture();
        let mut state = State::new("2026-11-24".parse().unwrap());
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let screen = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(28, 6)).unwrap();
            let model = screen_model::ScreenModel::project(state, &vault);
            terminal
                .draw(|frame| render::draw(frame, &model, theme::Theme::default()))
                .unwrap()
                .buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        state.handle(key(KeyCode::Char('?')), &vault);
        let start = screen(&state);
        assert!(start.contains("Search Projects"));
        assert!(!start.contains("q   Quit"));
        state.handle(key(KeyCode::End), &vault);
        let end = screen(&state);
        assert!(end.contains("q   Quit"), "{end}");
        assert!(!end.contains("Search Projects"));
        assert!(end.contains("Esc back"));
        state.handle(key(KeyCode::Home), &vault);
        assert_eq!(screen(&state), start);
        state.handle(key(KeyCode::Esc), &vault);
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!(state.view, View::Month);
    }

    #[test]
    fn narrow_workspace_menu_can_reach_hidden_commands() {
        let root = tempfile::tempdir().unwrap();
        let vault = Vault::load(root.path(), chrono_tz::UTC).unwrap();
        let day = "2026-11-24".parse().unwrap();
        let mut state = State::new(day);
        let press =
            |state: &mut State, code| state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault);
        let screen = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(24, 5)).unwrap();
            let model = screen_model::ScreenModel::project(state, &vault);
            let theme = theme::Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
            terminal
                .draw(|frame| render::draw(frame, &model, theme))
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let row = |buffer: &ratatui::buffer::Buffer, y| {
            (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>()
        };

        press(&mut state, KeyCode::Char(' '));
        let first = screen(&state);
        assert!(row(&first, 0).ends_with("MENU"));
        assert!(row(&first, 2).contains("c create"));
        assert_eq!(first[(23, 2)].symbol(), "│");
        assert!(row(&first, 4).contains("Esc back"));
        press(&mut state, KeyCode::End);
        assert!(row(&screen(&state), 2).contains("? help"));
        press(&mut state, KeyCode::Home);
        assert_eq!(screen(&state), first);
        press(&mut state, KeyCode::Esc);
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!((state.view, state.day), (View::Month, day));

        press(&mut state, KeyCode::Char(' '));
        let _ = screen(&state);
        press(&mut state, KeyCode::End);
        assert!(matches!(
            press(&mut state, KeyCode::Char('a')),
            Action::Proposals(_)
        ));
    }

    #[test]
    fn compact_view_selector_reaches_every_view_and_preserves_back_navigation() {
        let root = tempfile::tempdir().unwrap();
        let vault = Vault::load(root.path(), chrono_tz::UTC).unwrap();
        let mut state = State::new("2026-11-24".parse().unwrap());
        let press =
            |state: &mut State, code| state.handle(KeyEvent::new(code, KeyModifiers::NONE), &vault);
        let screen = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(24, 5)).unwrap();
            let model = screen_model::ScreenModel::project(state, &vault);
            let theme = theme::Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
            terminal
                .draw(|frame| render::draw(frame, &model, theme))
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let row = |buffer: &ratatui::buffer::Buffer, y| {
            (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>()
        };

        press(&mut state, KeyCode::Char(' '));
        press(&mut state, KeyCode::Char('u'));
        let first = screen(&state);
        assert!(row(&first, 0).ends_with("VIEWS"));
        assert!(row(&first, 2).contains("m Month · current"));
        assert_eq!(first[(23, 2)].symbol(), "│");
        assert!(row(&first, 4).contains("Esc back"));
        for label in [
            "w Week",
            "d Day",
            "a Agenda",
            "t Timeline",
            "l Outline",
            "g Graph",
            "i Inbox",
            "f Workflow",
        ] {
            press(&mut state, KeyCode::Down);
            assert!(row(&screen(&state), 2).contains(label), "{label}");
        }
        press(&mut state, KeyCode::Home);
        press(&mut state, KeyCode::End);
        assert!(row(&screen(&state), 2).contains("f Workflow"));
        press(&mut state, KeyCode::Home);
        assert_eq!(screen(&state), first);
        press(&mut state, KeyCode::Esc);
        assert!(matches!(state.overlay, Overlay::Space(_)));
        assert_eq!(state.view, View::Month);
        press(&mut state, KeyCode::Char('u'));
        assert!(matches!(
            press(&mut state, KeyCode::Char('f')),
            Action::Workflow(_)
        ));
        assert!(matches!(state.overlay, Overlay::None));
        assert_eq!(state.view, View::Workflow);
    }

    #[test]
    fn menu_switches_views_and_preserves_inclusive_selection() {
        let vault = fixture();
        let mut state = State::new("2026-11-24".parse().unwrap());
        for key in ['v', 'l', ' ', 'u', 'a'] {
            state.handle(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &vault,
            );
        }
        assert!(state.view == View::Agenda);
        assert_eq!(
            state.selected_range(),
            ("2026-11-24".parse().unwrap(), "2026-11-25".parse().unwrap())
        );
        assert!(
            state
                .agenda(&vault)
                .unwrap()
                .iter()
                .any(|entry| entry.title == "校园活动")
        );
        state.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &vault);
        assert_eq!(state.selected_range(), (state.day, state.day));
    }

    #[test]
    fn saved_home_view_names_are_stable_and_invalid_values_do_not_resolve() {
        for (view, name) in [
            (View::Month, "month"),
            (View::Week, "week"),
            (View::Day, "day"),
            (View::Agenda, "agenda"),
            (View::Outline, "outline"),
            (View::Graph, "graph"),
            (View::Timeline, "timeline"),
            (View::Workflow, "workflow"),
            (View::Inbox, "inbox"),
        ] {
            assert_eq!(view.saved_name(), name);
            assert_eq!(View::from_name(name), Some(view));
        }
        assert_eq!(View::from_name("unknown"), None);
        assert_eq!(View::from_name("../config.toml"), None);
    }

    #[test]
    fn startup_view_reads_state_only_when_restoring_the_last_view() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_path_buf());
        paths.initialize()?;
        assert_eq!(
            initial_view(&paths, HomeViewPolicy::RestoreLastView)?,
            View::Month
        );
        let last_view = paths.last_view_path();
        paths.write_state_file(&last_view, b"agenda\n")?;
        assert_eq!(
            initial_view(&paths, HomeViewPolicy::RestoreLastView)?,
            View::Agenda
        );
        paths.write_state_file(&last_view, b"unknown")?;
        assert_eq!(
            initial_view(&paths, HomeViewPolicy::RestoreLastView)?,
            View::Month
        );
        std::fs::remove_file(&last_view)?;
        std::fs::create_dir(&last_view)?;
        assert!(initial_view(&paths, HomeViewPolicy::RestoreLastView).is_err());
        assert_eq!(initial_view(&paths, HomeViewPolicy::Month)?, View::Month);
        Ok(())
    }

    #[test]
    fn configured_view_keys_switch_views() {
        let vault = fixture();
        let mut state = State::new("2026-11-24".parse().unwrap());
        state.keymap = keymap::Keymap::from_test_config("[keys]\nview_agenda = '8'\n");

        for key in [' ', 'u', '8'] {
            state.handle(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &vault,
            );
        }

        assert_eq!(state.view.label(), "Agenda");
        assert!(matches!(state.overlay, Overlay::None));
    }

    #[test]
    fn shortcut_hints_follow_the_same_keymap_as_input() {
        let vault = fixture();
        let mut state = State::new("2026-11-24".parse().unwrap());
        state.keymap = keymap::Keymap::from_test_config(
            "[keys]\nspace = 'M'\nhelp = '!'\nmove_left = 'H'\nmove_down = 'J'\nmove_up = 'K'\nmove_right = 'L'\n\
             picker = 'P'\nreload = 'R'\nquit = 'Q'\nview_agenda = '8'\n",
        );

        let screen = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(92, 35)).unwrap();
            let model = screen_model::ScreenModel::project(state, &vault);
            terminal
                .draw(|frame| render::draw(frame, &model, theme::Theme::default()))
                .unwrap()
                .buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let press = |state: &mut State, ch| {
            state.handle(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &vault)
        };

        let month = screen(&state);
        assert!(month.contains("HJKL move"));
        assert!(month.contains("P search  M menu"));
        press(&mut state, 'M');
        let space = screen(&state);
        assert!(space.contains("R reload"));
        assert!(space.contains("! help"));
        press(&mut state, 'u');
        assert!(screen(&state).contains("8 Agenda"));
        press(&mut state, '8');
        assert_eq!(state.view, View::Agenda);

        press(&mut state, '!');
        let help = screen(&state);
        assert!(help.contains("P   Search Projects"));
        assert!(help.contains("M u m/w/d/8/l/g/f/i"));
        assert!(help.contains("Q   Quit"));
        assert!(help.contains("R   Reload vault"));

        state.overlay = Overlay::None;
        state.view = View::Workflow;
        let workflow = screen(&state);
        assert!(workflow.contains("Run list unavailable · R retry"));
        assert!(workflow.contains("R refresh  ! help"));
        assert!(matches!(press(&mut state, 'R'), Action::Workflow(_)));
    }

    #[test]
    fn month_renders_deadlines_and_busy_entries_at_normal_and_narrow_sizes() {
        let vault = fixture();
        let state = State::new("2026-11-20".parse().unwrap());
        for (width, height) in [(84, 24), (35, 14)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let model = screen_model::ScreenModel::project(&state, &vault);
            let full = terminal
                .draw(|frame| render::draw(frame, &model, theme::Theme::default()))
                .unwrap();
            let text = full
                .buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("Month"));
            assert!(text.contains("20*"));
            assert!(text.contains("25*"));
        }
    }

    #[test]
    fn month_keeps_focused_day_visible_in_short_and_very_narrow_terminals() {
        use ratatui::style::Modifier;

        let vault = fixture();
        let state = State::new("2026-11-25".parse().unwrap());
        let model = screen_model::ScreenModel::project(&state, &vault);
        for (width, height) in [(19, 12), (68, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let frame = terminal
                .draw(|frame| {
                    render::draw(
                        frame,
                        &model,
                        theme::Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
                    )
                })
                .unwrap();
            let row = (2..height - 2)
                .find(|&row| frame.buffer[(1, row)].symbol() == "›")
                .expect("focused day must be scrolled into view");
            let selected = (3..9)
                .map(|col| frame.buffer[(col, row)].symbol())
                .collect::<String>();
            assert_eq!(selected, "Wed 25");
            assert!(frame.buffer[(3, row)].modifier.contains(Modifier::BOLD));
            assert_eq!(frame.buffer[(0, row)].symbol(), "│");
            assert_eq!(frame.buffer[(width - 1, row)].symbol(), "│");
        }
    }

    #[test]
    fn failed_reload_preserves_last_valid_snapshot() {
        let mut vault = fixture();
        let temp = tempfile::tempdir().unwrap();
        let mut state = State::new("2026-11-20".parse().unwrap());
        let before = vault.graph_mermaid();
        state.reload(&temp.path().join("missing"), &mut vault);
        assert!(state.message.starts_with("Reload failed"));
        assert_eq!(vault.graph_mermaid(), before);
    }
}

#[cfg(test)]
pub(super) mod calendar_tests;
#[cfg(test)]
pub(super) mod creation_tests;
#[cfg(test)]
mod deadline_tests;
#[cfg(test)]
mod inbox_tests;
#[cfg(test)]
mod picker_tests;
#[cfg(all(test, target_os = "linux"))]
mod watch_tests;
#[cfg(test)]
mod workflow_tests;
