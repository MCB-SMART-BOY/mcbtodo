//! Collect the rows and form context needed for the active terminal screen.

use std::rc::Rc;

use anyhow::Result;
use mcbtodo_application::{CalendarItem, InboxSummary, OutlineItem};
use mcbtodo_domain::Id;

use super::{
    Overlay, State, View, Workspace, creation, deadline, graph, picker, review::ReviewViewport,
    settings, time_block, workflow,
};

pub(super) enum ScreenData<'a> {
    Main(Result<ViewRows>),
    Space(&'a ReviewViewport),
    Settings(&'a settings::Form),
    Views(&'a ReviewViewport),
    Create(&'a ReviewViewport, bool),
    CreateForm(&'a creation::Form),
    Deadline(&'a deadline::Form),
    TimeBlock(&'a time_block::Form),
    GraphEdit(&'a graph::Edit, &'a ReviewViewport),
    Picker(&'a str, usize, Result<Vec<picker::Match>>),
    InboxProject(&'a [(Id, String)], usize),
    Proposals,
    Help(&'a ReviewViewport),
}

pub(super) enum ViewRows {
    Month(Vec<CalendarItem>),
    Week(Vec<CalendarItem>),
    Day(Vec<CalendarItem>),
    Agenda(Vec<CalendarItem>),
    Outline(Rc<[OutlineItem]>),
    Inbox(Rc<[InboxSummary]>),
    Graph(Rc<[graph::Node]>),
    Timeline(Vec<CalendarItem>),
    Workflow,
}

impl<'a> ScreenData<'a> {
    pub(super) fn load(state: &'a State, vault: &Workspace) -> Self {
        if state.view == View::Workflow
            && matches!(state.workflow.panel, workflow::Panel::Confirm(_))
        {
            return Self::Main(Ok(ViewRows::Workflow));
        }
        match &state.overlay {
            Overlay::None | Overlay::Time | Overlay::Command(_) => {
                Self::Main(ViewRows::load(state, vault))
            }
            Overlay::Space(viewport) => Self::Space(viewport),
            Overlay::Settings(form) => Self::Settings(form),
            Overlay::Views(viewport) => Self::Views(viewport),
            Overlay::Create(viewport) => Self::Create(
                viewport,
                state.view == View::Outline && state.selected_project_revision(vault).is_some(),
            ),
            Overlay::CreateForm(form) => Self::CreateForm(form),
            Overlay::Deadline(form) => Self::Deadline(form),
            Overlay::TimeBlock(form) => Self::TimeBlock(form),
            Overlay::GraphEdit { edit, viewport } => Self::GraphEdit(edit, viewport),
            Overlay::Picker { query, row } => {
                Self::Picker(query, *row, picker::matches(vault, query))
            }
            Overlay::InboxProject { row, projects, .. } => Self::InboxProject(projects, *row),
            Overlay::Proposals => Self::Proposals,
            Overlay::Help(viewport) => Self::Help(viewport),
        }
    }
}

impl ViewRows {
    fn load(state: &State, vault: &Workspace) -> Result<Self> {
        Ok(match state.view {
            View::Month => Self::Month(state.calendar_entries(vault, View::Month)?),
            View::Week => Self::Week(state.calendar_entries(vault, View::Week)?),
            View::Day => Self::Day(state.day_entries(vault)?),
            View::Agenda => Self::Agenda(state.agenda(vault)?),
            View::Outline => Self::Outline(state.outline_rows(vault)?),
            View::Inbox => Self::Inbox(state.inbox_rows(vault)?),
            View::Graph => Self::Graph(state.graph_nodes(vault)?),
            View::Timeline => Self::Timeline(state.calendar_entries(vault, View::Timeline)?),
            View::Workflow => Self::Workflow,
        })
    }
}
