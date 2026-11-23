use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use mcbtodo_client::daemon_client::{self, WorkflowRunPage};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_workflow::run::{RunState, RunSummary};
use uuid::Uuid;

use super::{
    Action,
    command::{TraceStep, WorkflowCommand},
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
    pending_page: Option<PendingPage>,
}

struct PendingPage {
    before: Option<Uuid>,
    reply: Receiver<Result<WorkflowRunPage>>,
    refresh_after: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Operation {
    List {
        before: Option<Uuid>,
    },
    Step {
        run_id: Uuid,
        index: u32,
    },
    Review {
        run_id: Uuid,
    },
    Decide {
        run_id: Uuid,
        node: String,
        approved: bool,
        review: Option<String>,
    },
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

    pub(super) fn command_for_key(&self, code: KeyCode) -> Option<WorkflowCommand> {
        match (&self.panel, code) {
            (Panel::Runs, KeyCode::Char('r')) => Some(WorkflowCommand::Refresh),
            (Panel::Runs, KeyCode::Char('n')) => Some(WorkflowCommand::Older),
            (Panel::Runs, KeyCode::Enter) => Some(WorkflowCommand::Trace),
            (Panel::Runs, KeyCode::Char(key @ ('a' | 'd'))) => Some(WorkflowCommand::Review {
                approved: key == 'a',
            }),
            (Panel::Trace { .. }, KeyCode::Char('h') | KeyCode::Left) => {
                Some(WorkflowCommand::Step(TraceStep::Previous))
            }
            (Panel::Trace { .. }, KeyCode::Char('l') | KeyCode::Right) => {
                Some(WorkflowCommand::Step(TraceStep::Next))
            }
            (Panel::Trace { .. }, KeyCode::Char('r')) => {
                Some(WorkflowCommand::Step(TraceStep::Refresh))
            }
            _ => None,
        }
    }

    pub(super) fn run_command(&mut self, command: WorkflowCommand, message: &mut String) -> Action {
        match command {
            WorkflowCommand::Step(direction) => {
                let Panel::Trace { index, .. } = self.panel else {
                    *message = "Open a Workflow trace first".into();
                    return Action::Continue;
                };
                let index = match direction {
                    TraceStep::Previous => index.saturating_sub(1),
                    TraceStep::Next => index.saturating_add(1),
                    TraceStep::Refresh => index,
                };
                self.step(index, message)
            }
            _ if !matches!(self.panel, Panel::Runs) => {
                *message = "Open the Workflow runs list first".into();
                Action::Continue
            }
            WorkflowCommand::Trace | WorkflowCommand::Review { .. } if self.is_loading_page() => {
                Action::Continue
            }
            WorkflowCommand::Refresh => refresh(),
            WorkflowCommand::Older => match self.next_before {
                Some(before) => Action::Workflow(Operation::List {
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
                    Action::Workflow(Operation::Review { run_id })
                } else {
                    *message = "Select a run waiting for approval".into();
                    Action::Continue
                }
            }
        }
    }

    pub(super) fn handle(&mut self, key: KeyEvent, keys: &Keymap) -> Option<Action> {
        if let Panel::Confirm(decision) = &self.panel {
            return Some(match key.code {
                KeyCode::Esc => {
                    self.panel = Panel::Runs;
                    Action::Continue
                }
                code if code == keys.workflow_confirmation_key()
                    && key.kind == KeyEventKind::Press =>
                {
                    let command = Operation::Decide {
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

        if let Panel::Trace { text, viewport, .. } = &mut self.panel {
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
            Action::Workflow(Operation::Step {
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

    pub(super) fn is_loading_page(&self) -> bool {
        self.pending_page.is_some()
    }

    fn request_page(&mut self, paths: &WorkspacePaths, before: Option<Uuid>) -> Result<()> {
        if let Some(pending) = &mut self.pending_page {
            // A first-page refresh supersedes an older-page request.
            pending.refresh_after |= before.is_none() && pending.before.is_some();
            return Ok(());
        }
        let (sender, reply) = mpsc::channel();
        let paths = paths.clone();
        thread::Builder::new()
            .name("mcbtodo-workflow-list".into())
            .spawn(move || {
                let _ = sender.send(daemon_client::workflow_runs(&paths, before));
            })?;
        self.pending_page = Some(PendingPage {
            before,
            reply,
            refresh_after: false,
        });
        Ok(())
    }

    pub(super) fn poll_page(
        &mut self,
        paths: &WorkspacePaths,
        visible: bool,
        message: &mut String,
    ) -> bool {
        let Some(pending) = &self.pending_page else {
            return false;
        };
        let result = match pending.reply.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("Workflow reader disconnected")),
        };
        let before = pending.before;
        let refresh_after = pending.refresh_after;
        self.pending_page = None;
        if !visible {
            return false;
        }
        let result = if refresh_after {
            self.request_page(paths, None)
        } else {
            result.map(|page| self.install_page(before, page))
        };
        if let Err(error) = result {
            *message = format!("Workflow · {error:#}");
        }
        true
    }

    pub(super) fn execute(&mut self, paths: &WorkspacePaths, command: Operation) -> String {
        let decision = matches!(command, Operation::Decide { .. });
        let result = match command {
            Operation::List { before } => self.request_page(paths, before),
            Operation::Step { run_id, index } => daemon_client::workflow_step(paths, run_id, index)
                .and_then(|step| self.show_step(run_id, index, step)),
            Operation::Review { run_id } => {
                daemon_client::workflow_run(paths, run_id).and_then(|run| self.review_decision(run))
            }
            Operation::Decide {
                run_id,
                node,
                approved,
                review,
            } => daemon_client::decide_workflow(paths, run_id, node, approved, review)
                .map(|run| self.record_decision(run)),
        };
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

    pub(super) fn install_page(&mut self, before: Option<Uuid>, page: WorkflowRunPage) {
        if before.is_some() {
            self.runs.extend(page.runs);
        } else {
            let selected = self.selected().map(|run| run.id);
            self.runs = page.runs;
            self.row = self
                .runs
                .iter()
                .position(|run| Some(run.id) == selected)
                .unwrap_or(0);
            self.panel = Panel::Runs;
        }
        self.next_before = page.next_before;
        self.loaded = true;
    }

    pub(super) fn show_step(
        &mut self,
        run_id: Uuid,
        index: u32,
        step: serde_json::Value,
    ) -> Result<()> {
        if self.selected().map(|run| run.id) != Some(run_id) {
            bail!("Workflow selection changed");
        }
        self.panel = Panel::Trace {
            index,
            text: serde_json::to_string_pretty(&step)?,
            viewport: PannableReview::default(),
        };
        Ok(())
    }

    pub(super) fn review_decision(&mut self, run: RunSummary) -> Result<()> {
        let Panel::InspectDecision { approved } = self.panel else {
            bail!("No workflow review requested");
        };
        if self.selected().map(|selected| selected.id) != Some(run.id) {
            bail!("Workflow selection changed");
        }
        let confirmation = run
            .current_node
            .as_deref()
            .filter(|_| run.state == RunState::Waiting)
            .map(|node| Decision::new(&run, node, approved));
        self.runs[self.row] = run;
        self.panel = Panel::Runs;
        let Some(confirmation) = confirmation else {
            bail!("Selected run is no longer waiting for approval");
        };
        self.panel = Panel::Confirm(confirmation);
        Ok(())
    }

    pub(super) fn record_decision(&mut self, updated: RunSummary) {
        if let Some(run) = self.runs.iter_mut().find(|run| run.id == updated.id) {
            *run = updated;
        }
    }
}

fn refresh() -> Action {
    Action::Workflow(Operation::List { before: None })
}
