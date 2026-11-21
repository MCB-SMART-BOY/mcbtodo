use std::{cell::Cell, ops::Range};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{
    Request, Response, request,
    workflow::{RunState, RunSummary, WorkflowRequest, WorkflowResponse},
};
use ratatui::text::Line;
use uuid::Uuid;

use super::{Action, move_row};

#[derive(Default)]
pub(super) struct State {
    pub(super) runs: Vec<RunSummary>,
    pub(super) row: usize,
    pub(super) next_before: Option<Uuid>,
    pub(super) loaded: bool,
    pub(super) panel: Panel,
}

#[derive(Default)]
pub(super) enum Panel {
    #[default]
    Runs,
    Trace {
        index: u32,
        text: String,
        scroll: (u16, u16),
    },
    InspectDecision {
        approved: bool,
    },
    Confirm(Decision),
}

pub(super) struct Decision {
    run_id: Uuid,
    node: String,
    approved: bool,
    text: String,
    viewport: Cell<Viewport>,
}

#[derive(Clone, Copy, Default)]
struct Viewport {
    offset: usize,
    height: usize,
    last_offset: usize,
}

impl Decision {
    fn new(run: &RunSummary, node: &str, approved: bool) -> Self {
        let mut decision = Self {
            run_id: run.id,
            node: node.to_owned(),
            approved,
            text: String::new(),
            viewport: Cell::default(),
        };
        let mut text = format!("{} workflow run?\nRun  {}\nNode ", decision.verb(), run.id);
        append_review_line(&mut text, node);
        text.push('\n');
        // Split only at actual newlines. Literal escapes from the daemon are already display text.
        for line in run
            .detail
            .as_deref()
            .unwrap_or("No run detail available.")
            .split('\n')
        {
            text.push('\n');
            append_review_line(&mut text, line);
        }
        decision.text = text;
        decision
    }

    pub(super) fn verb(&self) -> &'static str {
        if self.approved { "Approve" } else { "Reject" }
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn visible_rows(&self, total: usize, height: u16) -> Range<usize> {
        let height = usize::from(height);
        let last_offset = total.saturating_sub(height);
        let offset = self.viewport.get().offset.min(last_offset);
        // The renderer supplies the current bounds after wrapping at the terminal width.
        self.viewport.set(Viewport {
            offset,
            height,
            last_offset,
        });
        offset..(offset + height).min(total)
    }

    fn scroll(&self, key: KeyCode) {
        let mut viewport = self.viewport.get();
        let Viewport {
            offset,
            height,
            last_offset,
        } = viewport;
        viewport.offset = match key {
            KeyCode::Char('j') | KeyCode::Down => offset.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => offset.saturating_sub(1),
            KeyCode::PageDown => offset.saturating_add(height),
            KeyCode::PageUp => offset.saturating_sub(height),
            KeyCode::Home => 0,
            KeyCode::End => last_offset,
            _ => return,
        }
        .min(last_offset);
        self.viewport.set(viewport);
    }
}

impl State {
    pub(super) fn selected(&self) -> Option<&RunSummary> {
        self.runs.get(self.row)
    }

    pub(super) fn open(&mut self) -> Action {
        self.panel = Panel::Runs;
        refresh()
    }

    pub(super) fn handle(&mut self, key: KeyEvent, message: &mut String) -> Option<Action> {
        if let Panel::Confirm(decision) = &self.panel {
            return Some(match key.code {
                KeyCode::Esc => {
                    self.panel = Panel::Runs;
                    Action::Continue
                }
                KeyCode::Char('y') if key.kind == KeyEventKind::Press => {
                    let command = WorkflowRequest::Decide {
                        run_id: decision.run_id,
                        node: decision.node.clone(),
                        approved: decision.approved,
                    };
                    self.panel = Panel::Runs;
                    Action::Workflow(command)
                }
                _ => {
                    decision.scroll(key.code);
                    Action::Continue
                }
            });
        }

        if let Panel::Trace {
            index,
            text,
            scroll,
        } = &mut self.panel
        {
            let index = *index;
            return match key.code {
                KeyCode::Esc => {
                    self.panel = Panel::Runs;
                    Some(Action::Continue)
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let last_line = text.lines().count().saturating_sub(1);
                    scroll.0 = scroll.0.saturating_add(1).min(to_u16(last_line));
                    Some(Action::Continue)
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    scroll.0 = scroll.0.saturating_sub(1);
                    Some(Action::Continue)
                }
                KeyCode::Char('H') => {
                    scroll.1 = scroll.1.saturating_sub(4);
                    Some(Action::Continue)
                }
                KeyCode::Char('L') => {
                    let last_column = text
                        .lines()
                        .map(|line| Line::raw(line).width())
                        .max()
                        .unwrap_or(0);
                    scroll.1 = scroll
                        .1
                        .saturating_add(4)
                        .min(to_u16(last_column.saturating_sub(1)));
                    Some(Action::Continue)
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    Some(self.step(index.saturating_sub(1), message))
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    Some(self.step(index.saturating_add(1), message))
                }
                KeyCode::Char('r') => Some(self.step(index, message)),
                _ => None,
            };
        }

        Some(match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.row = move_row(self.row, true, self.runs.len());
                Action::Continue
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.row = move_row(self.row, false, self.runs.len());
                Action::Continue
            }
            KeyCode::Char('r') => refresh(),
            KeyCode::Char('n') => match self.next_before {
                Some(before) => Action::Workflow(WorkflowRequest::List {
                    before: Some(before),
                }),
                None => {
                    *message = "No older runs".into();
                    Action::Continue
                }
            },
            KeyCode::Enter => self.step(0, message),
            KeyCode::Char(key @ ('a' | 'd')) => {
                if let Some(run_id) = self
                    .selected()
                    .filter(|run| run.state == RunState::Waiting)
                    .map(|run| run.id)
                {
                    self.panel = Panel::InspectDecision {
                        approved: key == 'a',
                    };
                    Action::Workflow(WorkflowRequest::Inspect { run_id })
                } else {
                    *message = "Select a run waiting for approval".into();
                    Action::Continue
                }
            }
            _ => return None,
        })
    }

    fn step(&self, index: u32, message: &mut String) -> Action {
        if let Some(run) = self.selected().filter(|run| index < run.step_count) {
            Action::Workflow(WorkflowRequest::Step {
                run_id: run.id,
                index,
            })
        } else {
            *message = if matches!(self.panel, Panel::Trace { .. }) {
                "No further recorded step"
            } else {
                "No recorded steps for the selected run"
            }
            .into();
            Action::Continue
        }
    }

    pub(super) fn execute(&mut self, paths: &WorkspacePaths, command: WorkflowRequest) -> String {
        let decision = matches!(command, WorkflowRequest::Decide { .. });
        let result = call(paths, &command).and_then(|response| self.apply(command, response));
        if result.is_err() && matches!(self.panel, Panel::InspectDecision { .. }) {
            self.panel = Panel::Runs;
        }
        if decision {
            match result {
                Ok(()) => "Approval decision recorded · r refresh".into(),
                Err(error) => format!("Decision · {error:#} · r refresh"),
            }
        } else {
            match result {
                Ok(()) => String::new(),
                Err(error) => format!("Workflow · {error:#}"),
            }
        }
    }

    pub(super) fn apply(
        &mut self,
        command: WorkflowRequest,
        response: WorkflowResponse,
    ) -> Result<()> {
        match (command, response) {
            (WorkflowRequest::List { before }, WorkflowResponse::Runs { runs, next_before }) => {
                if before.is_some() && before == next_before {
                    bail!("daemon returned the same page cursor");
                }
                if before.is_some() {
                    self.runs.extend(runs);
                } else {
                    let selected = self.selected().map(|run| run.id);
                    self.runs = runs;
                    self.row = self
                        .runs
                        .iter()
                        .position(|run| Some(run.id) == selected)
                        .unwrap_or(0);
                    self.panel = Panel::Runs;
                }
                self.next_before = next_before;
                self.loaded = true;
            }
            (WorkflowRequest::Step { run_id, index }, WorkflowResponse::Step { step })
                if self.selected().is_some_and(|run| run.id == run_id) =>
            {
                self.panel = Panel::Trace {
                    index,
                    text: serde_json::to_string_pretty(&step)?,
                    scroll: (0, 0),
                };
            }
            (WorkflowRequest::Inspect { run_id }, WorkflowResponse::Run(updated))
                if updated.id == run_id
                    && self.selected().is_some_and(|run| run.id == run_id)
                    && let Panel::InspectDecision { approved } = self.panel =>
            {
                let confirmation = updated
                    .current_node
                    .as_deref()
                    .filter(|_| updated.state == RunState::Waiting)
                    .map(|node| Decision::new(&updated, node, approved));
                self.runs[self.row] = updated;
                self.panel = Panel::Runs;
                let Some(confirmation) = confirmation else {
                    bail!("Selected run is no longer waiting for approval");
                };
                self.panel = Panel::Confirm(confirmation);
            }
            (WorkflowRequest::Decide { run_id, .. }, WorkflowResponse::Run(updated))
                if updated.id == run_id =>
            {
                if let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) {
                    *run = updated;
                }
            }
            _ => bail!("unexpected workflow response"),
        }
        Ok(())
    }
}

fn append_review_line(visible: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '\n' => visible.push_str(r"\n"),
            '\r' => visible.push_str(r"\r"),
            '\t' => visible.push_str(r"\t"),
            '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => {
                visible.extend(ch.escape_unicode());
            }
            ch if ch.is_control() => visible.extend(ch.escape_unicode()),
            ch => visible.push(ch),
        }
    }
}

fn to_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn refresh() -> Action {
    Action::Workflow(WorkflowRequest::List { before: None })
}

fn call(paths: &WorkspacePaths, command: &WorkflowRequest) -> Result<WorkflowResponse> {
    let socket = paths.socket_path();
    match request(&socket, Request::Workflow(command.clone()))
        .with_context(|| format!("contacting mcbtodo-daemon at {}", socket.display()))?
    {
        Response::Workflow(response) => Ok(response),
        Response::Error { code, message } => bail!("{code:?} · {message}"),
        _ => bail!("unexpected response to workflow request"),
    }
}
