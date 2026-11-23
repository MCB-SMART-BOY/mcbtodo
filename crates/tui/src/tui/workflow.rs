use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use mcbtodo_client::daemon_client;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::workflow::{RunState, RunSummary, WorkflowRequest, WorkflowResponse};
use uuid::Uuid;

use super::{
    Action,
    command::WorkflowCommand,
    keymap::Keymap,
    move_row,
    review::{PannableReview, ReviewViewport},
};

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
        viewport: PannableReview,
    },
    InspectDecision {
        approved: bool,
    },
    Confirm(Decision),
}

pub(super) struct Decision {
    pub(super) run_id: Uuid,
    pub(super) node: String,
    pub(super) approved: bool,
    pub(super) detail: Option<String>,
    review: Option<String>,
    pub(super) viewport: ReviewViewport,
}

impl Decision {
    fn new(run: &RunSummary, node: &str, approved: bool) -> Self {
        Self {
            run_id: run.id,
            node: node.to_owned(),
            approved,
            detail: run.detail.clone(),
            review: approved.then(|| run.approval_review()).flatten(),
            viewport: ReviewViewport::default(),
        }
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

    pub(super) fn runs_command_for_key(&self, code: KeyCode) -> Option<WorkflowCommand> {
        if !matches!(self.panel, Panel::Runs) {
            return None;
        }
        match code {
            KeyCode::Char('r') => Some(WorkflowCommand::Refresh),
            KeyCode::Char('n') => Some(WorkflowCommand::Older),
            KeyCode::Enter => Some(WorkflowCommand::Trace),
            KeyCode::Char(key @ ('a' | 'd')) => Some(WorkflowCommand::Review {
                approved: key == 'a',
            }),
            _ => None,
        }
    }

    pub(super) fn run_command(&mut self, command: WorkflowCommand, message: &mut String) -> Action {
        match command {
            WorkflowCommand::Refresh => refresh(),
            WorkflowCommand::Older => match self.next_before {
                Some(before) => Action::Workflow(WorkflowRequest::List {
                    before: Some(before),
                }),
                None => {
                    *message = "No older runs".into();
                    Action::Continue
                }
            },
            WorkflowCommand::Trace => self.step(0, message),
            WorkflowCommand::Review { approved } => {
                if let Some(run_id) = self
                    .selected()
                    .filter(|run| run.state == RunState::Waiting)
                    .map(|run| run.id)
                {
                    self.panel = Panel::InspectDecision { approved };
                    Action::Workflow(WorkflowRequest::Inspect { run_id })
                } else {
                    *message = "Select a run waiting for approval".into();
                    Action::Continue
                }
            }
        }
    }

    pub(super) fn handle(
        &mut self,
        key: KeyEvent,
        keys: &Keymap,
        message: &mut String,
    ) -> Option<Action> {
        if let Panel::Confirm(decision) = &self.panel {
            return Some(match key.code {
                KeyCode::Esc => {
                    self.panel = Panel::Runs;
                    Action::Continue
                }
                code if code == keys.workflow_confirmation_key()
                    && key.kind == KeyEventKind::Press =>
                {
                    let command = WorkflowRequest::Decide {
                        run_id: decision.run_id,
                        node: decision.node.clone(),
                        approved: decision.approved,
                        review: decision.review.clone(),
                    };
                    self.panel = Panel::Runs;
                    Action::Workflow(command)
                }
                _ => {
                    decision.viewport.scroll(key.code);
                    Action::Continue
                }
            });
        }

        if let Panel::Trace {
            index,
            text,
            viewport,
        } = &mut self.panel
        {
            let index = *index;
            return match key.code {
                KeyCode::Esc => {
                    self.panel = Panel::Runs;
                    Some(Action::Continue)
                }
                KeyCode::Char('j' | 'k')
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End => {
                    viewport.scroll(key.code, text);
                    Some(Action::Continue)
                }
                KeyCode::Char('H') => {
                    viewport.pan_left(4);
                    Some(Action::Continue)
                }
                KeyCode::Char('L') => {
                    viewport.pan_right(4, text);
                    Some(Action::Continue)
                }
                KeyCode::Char('w') => {
                    if key.kind == KeyEventKind::Press {
                        viewport.toggle_wrap();
                    }
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
        let result = daemon_client::workflow(paths, command.clone())
            .and_then(|response| self.apply(command, response));
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
                    viewport: PannableReview::default(),
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

fn refresh() -> Action {
    Action::Workflow(WorkflowRequest::List { before: None })
}
