mod calendar;
mod creation;
mod deadline;
mod graph;
mod keymap;
mod picker;
mod proposal_review;
mod render;
mod time_block;
mod timeline;
mod watch;
mod workflow;

use std::{
    collections::BTreeSet,
    env,
    io::{self, IsTerminal},
    path::Path,
    process::Command,
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
    CalendarItem, TargetAction, VaultCommands, deadline_edit_context, project_action_targets,
    project_calendar, project_calendar_all, project_inbox, project_outline,
    project_revision_in_snapshot,
};
use mcbtodo_domain::{
    CalendarKind, Id, Occurrence,
    time::{DateSpan, Span},
};
use mcbtodo_platform::{HomeViewPolicy, WorkspacePaths};
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

    fn from_saved_name(name: &str) -> Option<Self> {
        Some(match name.trim() {
            "month" => Self::Month,
            "week" => Self::Week,
            "day" => Self::Day,
            "agenda" => Self::Agenda,
            "outline" => Self::Outline,
            "graph" => Self::Graph,
            "timeline" => Self::Timeline,
            "workflow" => Self::Workflow,
            "inbox" => Self::Inbox,
            _ => return None,
        })
    }
}

fn initial_view(policy: HomeViewPolicy, saved: Option<&str>) -> View {
    match policy {
        HomeViewPolicy::Month => View::Month,
        HomeViewPolicy::RestoreLastView => {
            saved.and_then(View::from_saved_name).unwrap_or(View::Month)
        }
    }
}

#[derive(Default)]
enum Overlay {
    #[default]
    None,
    Space,
    Views,
    Time,
    Create,
    CreateForm(creation::Form),
    Deadline(deadline::Form),
    TimeBlock(time_block::Form),
    GraphEdit(graph::Edit),
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
    Help,
    Command(String),
}

#[derive(PartialEq, Eq, Debug)]
enum Action {
    Continue,
    Quit,
    Reload,
    Edit,
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
    week_hour: usize,
    graph_selection: Option<BTreeSet<Id>>,
    proposals: proposal_review::Review,
    workflow: workflow::State,
    keymap: keymap::Keymap,
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
            week_hour: 0,
            graph_selection: None,
            proposals: proposal_review::Review::default(),
            workflow: workflow::State::default(),
            keymap: keymap::Keymap::default(),
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
            let from_week = self.view == View::Week;
            let selected = self.selected_focus(vault);
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
            if from_week && view == View::Day {
                self.focus_week_entry(vault);
            }
        }
    }

    fn focus_week_entry(&mut self, vault: &Vault) {
        let week = calendar::Week::new(vault, self.day);
        if let Some(entry) = week.focused_timed(self.day, self.week_hour, vault.timezone())
            && let Occurrence::Span(span) = entry.occurrence
            && let Some(row) = calendar::day(vault, self.day).iter().position(|candidate| {
                candidate.id == entry.id
                    && candidate.kind == entry.kind
                    && matches!(candidate.occurrence, Occurrence::Span(other) if other == span)
            })
        {
            self.row = row;
            self.document_id = Some(entry.id);
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
        match self.view {
            View::Month | View::Week | View::Workflow => Vec::new(),
            View::Day => calendar::day(vault, self.day)
                .iter()
                .map(calendar_focus)
                .collect(),
            View::Agenda => self.agenda(vault).iter().map(calendar_focus).collect(),
            View::Timeline => timeline::entries(vault, self.day)
                .iter()
                .map(calendar_focus)
                .collect(),
            View::Outline => project_outline(vault)
                .into_iter()
                .map(|row| FocusIdentity::Document(row.id))
                .collect(),
            View::Graph => graph::nodes(vault)
                .into_iter()
                .map(|node| FocusIdentity::Document(node.id))
                .collect(),
            View::Inbox => project_inbox(vault)
                .into_iter()
                .filter(|entry| entry.project_id.is_none())
                .map(|entry| FocusIdentity::Document(entry.id))
                .collect(),
        }
    }

    fn agenda(&self, vault: &Vault) -> Vec<CalendarItem> {
        let (start, end) = self.selected_range();
        let dates = DateSpan::new(start, end).expect("selection is ordered and in display years");
        project_calendar(vault, dates).expect("selected agenda range is bounded")
    }

    fn handle(&mut self, mut key: KeyEvent, vault: &Vault) -> Action {
        use Action::*;
        if matches!(
            self.overlay,
            Overlay::None | Overlay::Space | Overlay::Views
        ) {
            key = self
                .keymap
                .normalize(key, matches!(self.overlay, Overlay::Views));
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                Quit
            } else {
                Continue
            };
        }
        match &mut self.overlay {
            Overlay::GraphEdit(_) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => self.overlay = Overlay::None,
                    KeyCode::Char('y') | KeyCode::Enter => {
                        let edit = match std::mem::replace(&mut self.overlay, Overlay::None) {
                            Overlay::GraphEdit(edit) => edit,
                            _ => unreachable!(),
                        };
                        return SetGraphDependencies(graph::Submission {
                            target: edit.target,
                            prerequisites: edit.prerequisites,
                            revision: edit.revision,
                        });
                    }
                    _ => {}
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
                            ["help" | "?"] => self.overlay = Overlay::Help,
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
                                let view = match *view {
                                    "month" => Some(View::Month),
                                    "week" => Some(View::Week),
                                    "day" => Some(View::Day),
                                    "agenda" => Some(View::Agenda),
                                    "outline" => Some(View::Outline),
                                    "graph" => Some(View::Graph),
                                    "timeline" => Some(View::Timeline),
                                    "workflow" => Some(View::Workflow),
                                    "inbox" => Some(View::Inbox),
                                    _ => None,
                                };
                                if let Some(view) = view {
                                    self.switch(view, vault);
                                    self.message.clear();
                                    if view == View::Workflow {
                                        return self.workflow.open();
                                    }
                                } else {
                                    self.message =
                                        "Usage: view month|week|day|agenda|outline|graph|timeline|workflow|inbox"
                                            .into();
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
                        let selected = picker::matches(vault, query).get(*row).map(|item| item.id);
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
                        let count = picker::matches(vault, query).len();
                        *row = move_row(*row, true, count);
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
            Overlay::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                    self.overlay = Overlay::None;
                }
                return Continue;
            }
            Overlay::Space => {
                match key.code {
                    KeyCode::Char('u') => self.overlay = Overlay::Views,
                    KeyCode::Char('t') => self.overlay = Overlay::Time,
                    KeyCode::Char('c') if self.view.uses_dates() || self.view == View::Outline => {
                        self.overlay = Overlay::Create;
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
                    KeyCode::Char('?') => self.overlay = Overlay::Help,
                    KeyCode::Char('r') => {
                        self.overlay = Overlay::None;
                        return Reload;
                    }
                    KeyCode::Esc => self.overlay = Overlay::None,
                    _ => {}
                }
                return Continue;
            }
            Overlay::Views => {
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::Space,
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
                            _ => return Continue,
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
            Overlay::Create => {
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
                        self.overlay = Overlay::Space;
                        return Continue;
                    }
                    _ => return Continue,
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
                        self.overlay = Overlay::Space;
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
            && let Some(action) = self.workflow.handle(key, &mut self.message)
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
            KeyCode::Char(' ') => self.overlay = Overlay::Space,
            KeyCode::Char(':') => self.overlay = Overlay::Command(String::new()),
            KeyCode::Char('/') => {
                self.overlay = Overlay::Picker {
                    query: String::new(),
                    row: 0,
                }
            }
            KeyCode::Char('?') => self.overlay = Overlay::Help,
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
                        self.overlay = Overlay::GraphEdit(edit);
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
                let week = calendar::Week::new(vault, self.day);
                let hour = self.week_hour.min(week.hours.len().saturating_sub(1));
                self.week_hour = move_row(hour, down, week.hours.len());
            }
            View::Day | View::Agenda | View::Outline | View::Graph | View::Timeline => {
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
        let nodes = graph::nodes(vault);
        let Some(node) = nodes.get(self.row) else {
            return;
        };
        let neighbors = if forward {
            &node.dependents
        } else {
            &node.prerequisites
        };
        if let Some(&row) = neighbors.first() {
            self.row = row;
            self.document_id = Some(nodes[row].id);
        }
    }

    fn source<'a>(&self, vault: &'a Vault) -> Option<&'a Path> {
        vault.source(self.selected_document(vault)?)
    }

    fn selected_document(&self, vault: &Vault) -> Option<Id> {
        self.row_ids(vault).get(self.row).copied()
    }

    fn selected_project_revision(&self, vault: &Vault) -> Option<(Id, Revision)> {
        let id = self.selected_document(vault)?;
        project_revision_in_snapshot(vault, id).map(|revision| (id, revision))
    }

    fn reload(&mut self, root: &Path, vault: &mut Vault) {
        match Vault::load(root, vault.timezone()) {
            Ok(next) => {
                self.install_vault(vault, next);
                self.vault_warning = None;
                self.message = "Vault reloaded".into();
            }
            Err(error) => self.message = format!("Reload failed · {error:#}"),
        }
    }

    fn refresh_external(&mut self, root: &Path, vault: &mut Vault) -> bool {
        match Vault::load(root, vault.timezone()) {
            Ok(next) => {
                let recovered = self.vault_warning.take().is_some();
                if same_documents(vault, &next) {
                    // A local action may already have loaded these exact bytes.
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
                true
            }
        }
    }

    fn install_vault(&mut self, vault: &mut Vault, next: Vault) {
        let preferred = self
            .selected_focus(vault)
            .or_else(|| self.document_id.map(FocusIdentity::Document));
        *vault = next;
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

    fn create_time_block(&mut self, root: &Path, vault: &mut Vault, block: time_block::Submission) {
        let (unit, when) = (block.unit, block.when);
        let saved = match VaultCommands::new(root, vault.timezone()).create_time_block(
            unit,
            when.start(),
            when.end(),
            mcbtodo_domain::Availability::Busy,
            block.revision,
        ) {
            Ok(_) => "Time block saved".to_owned(),
            Err(WriteError::PublishedButNotSynced { source, .. }) => {
                format!("Time block published · directory sync failed: {source}")
            }
            Err(WriteError::StaleRevision { .. }) => {
                self.message = "WorkUnit changed · Esc twice, r to reload, then retry".into();
                return;
            }
            Err(error) => {
                self.message = format!("Time block failed · {error:#}");
                return;
            }
        };
        // A published write must close the form, even if refreshing the snapshot fails.
        self.overlay = Overlay::None;
        self.reload(root, vault);
        self.message = if self.message == "Vault reloaded" {
            self.switch(View::Day, vault);
            if let Some(row) = calendar::day(vault, self.day).iter().position(|entry| {
                entry.id == unit
                    && matches!(entry.occurrence, Occurrence::Span(Span::DateTimeSpan(span)) if span == when)
            }) {
                self.row = row;
                self.document_id = Some(unit);
            }
            saved
        } else {
            format!("{saved} · {}", self.message)
        };
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

fn document_ids(vault: &Vault) -> BTreeSet<Id> {
    let objects = project_outline(vault).into_iter().map(|row| row.id);
    let calendar = project_calendar_all(vault)
        .into_iter()
        .filter(|entry| matches!(entry.kind, CalendarKind::Event | CalendarKind::Deadline))
        .map(|entry| entry.id);
    objects.chain(calendar).collect()
}

fn project_rows(vault: &Vault) -> Vec<(Id, String)> {
    project_outline(vault)
        .into_iter()
        .filter(|row| row.depth == 0)
        .map(|row| (row.id, row.title))
        .collect()
}

fn same_documents(current: &Vault, next: &Vault) -> bool {
    // Revision compares exact source bytes, including edits to Markdown body
    // text. Include source paths so a rename updates the editor target too.
    let next_ids = document_ids(next);
    next_ids.len() == document_ids(current).len()
        && next_ids.into_iter().all(|id| {
            current.source(id) == next.source(id) && current.revision(id) == next.revision(id)
        })
}

fn move_row(row: usize, down: bool, len: usize) -> usize {
    if down {
        row.saturating_add(1).min(len.saturating_sub(1))
    } else {
        row.saturating_sub(1)
    }
}

pub(super) fn run(
    root: &Path,
    mut vault: Vault,
    day: NaiveDate,
    paths: &WorkspacePaths,
    home_view: HomeViewPolicy,
) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "the TUI requires an interactive terminal"
    );
    let mut state = State::new(day);
    if home_view == HomeViewPolicy::RestoreLastView {
        match paths.read_state_file(&paths.last_view_path()) {
            Ok(saved) => state.view = initial_view(home_view, Some(&saved)),
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error).context("loading the saved home view"),
        }
    }
    if state.view == View::Workflow
        && let Action::Workflow(request) = state.workflow.open()
    {
        state.message = state.workflow.execute(paths, request);
    }
    state.keymap = keymap::Keymap::load_config(paths)?;
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
                terminal.draw(|frame| render::draw(frame, &state, &vault))?;
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
                Action::Timer(operation) => {
                    state.message = match crate::timer(paths, operation) {
                        Ok(status) => crate::timer_label(&status),
                        Err(error) => format!("Timer failed · {error:#}"),
                    };
                }
                Action::CreateCalendar(request) => {
                    state.create_calendar(root, &mut vault, request);
                }
                Action::CreateReminder(request) => {
                    state.create_reminder(root, &mut vault, paths, request);
                }
                Action::CreateTask(request) => {
                    state.create_task(root, &mut vault, request);
                }
                Action::CreateTimeBlock(block) => {
                    state.create_time_block(root, &mut vault, block);
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
        if home_view == HomeViewPolicy::RestoreLastView {
            paths.write_state_file(&paths.last_view_path(), state.view.saved_name().as_bytes())?;
        }
        match handoff {
            TerminalHandoff::Quit => return Ok(()),
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
                .iter()
                .any(|entry| entry.title == "校园活动")
        );
        state.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &vault);
        assert_eq!(state.selected_range(), (state.day, state.day));
    }

    #[test]
    fn saved_home_view_names_are_stable_and_invalid_values_do_not_resolve() {
        let views = [
            View::Month,
            View::Week,
            View::Day,
            View::Agenda,
            View::Outline,
            View::Graph,
            View::Timeline,
            View::Workflow,
            View::Inbox,
        ];
        for view in views {
            assert_eq!(View::from_saved_name(view.saved_name()), Some(view));
        }
        assert_eq!(View::from_saved_name("unknown"), None);
        assert_eq!(View::from_saved_name("../config.toml"), None);
        assert_eq!(
            initial_view(HomeViewPolicy::RestoreLastView, Some("agenda")),
            View::Agenda
        );
        assert_eq!(
            initial_view(HomeViewPolicy::RestoreLastView, Some("unknown")),
            View::Month
        );
        assert_eq!(
            initial_view(HomeViewPolicy::RestoreLastView, None),
            View::Month
        );
        assert_eq!(
            initial_view(HomeViewPolicy::Month, Some("agenda")),
            View::Month
        );
    }

    #[test]
    fn configured_view_keys_switch_views() {
        let vault = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.toml");
        std::fs::write(&path, "[keys]\nview_agenda = '8'\n").unwrap();
        let mut state = State::new("2026-11-24".parse().unwrap());
        state.keymap = keymap::Keymap::load(&path).unwrap();

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
    fn month_renders_deadlines_and_busy_entries_at_normal_and_narrow_sizes() {
        let vault = fixture();
        let state = State::new("2026-11-20".parse().unwrap());
        for (width, height) in [(84, 24), (35, 14)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let full = terminal
                .draw(|frame| render::draw(frame, &state, &vault))
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
