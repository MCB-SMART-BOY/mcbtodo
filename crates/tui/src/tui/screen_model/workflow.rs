use chrono_tz::Tz;
use crossterm::event::KeyCode;
use mcbtodo_workflow::run::RunState;
use ratatui::text::Line;

use crate::tui::{
    keymap::{Action, Keymap},
    review::{PannableReview, ReviewViewport, append_review_line},
    workflow::{Decision, Panel, State},
};

pub(in crate::tui) struct WorkflowScreenModel<'a> {
    pub(in crate::tui) panel: WorkflowPanel<'a>,
    pub(in crate::tui) hint: String,
    pub(in crate::tui) compact_hint: String,
}

pub(in crate::tui) enum WorkflowPanel<'a> {
    Confirm(WorkflowConfirmation<'a>),
    Empty(String),
    Trace {
        title: String,
        text: &'a str,
        viewport: &'a PannableReview,
    },
    Runs {
        title: &'static str,
        rows: Vec<String>,
        selected: usize,
        details: Vec<String>,
    },
}

pub(in crate::tui) struct WorkflowConfirmation<'a> {
    pub(in crate::tui) title: &'static str,
    pub(in crate::tui) text: String,
    pub(in crate::tui) viewport: &'a ReviewViewport,
    hints: [String; 4],
}

impl<'a> WorkflowConfirmation<'a> {
    fn project(decision: &'a Decision, keys: &Keymap) -> Self {
        let (verb, action) = if decision.approved {
            ("Approve", "approve")
        } else {
            ("Reject", "reject")
        };
        let mut text = format!("{verb} workflow run?\nRun  {}\nNode ", decision.run_id);
        append_review_line(&mut text, &decision.node);
        text.push('\n');
        // Literal escapes from the daemon are already display text.
        for line in decision
            .detail
            .as_deref()
            .unwrap_or("No run detail available.")
            .split('\n')
        {
            text.push('\n');
            append_review_line(&mut text, line);
        }
        let confirm = if keys.workflow_confirmation_key() == KeyCode::Enter {
            "Enter"
        } else {
            "y"
        };
        let down = keys.label(Action::MoveDown);
        let up = keys.label(Action::MoveUp);
        Self {
            title: " Confirm decision ",
            text,
            viewport: &decision.viewport,
            hints: [
                format!("{confirm} {action}  Esc cancel  {down}/{up}/↑↓  PgUp/PgDn  Home/End"),
                format!("{confirm} {action}  Esc  {down}/{up}"),
                format!("{confirm} {action}  Esc"),
                format!("{confirm} Esc"),
            ],
        }
    }

    pub(in crate::tui) fn hint(&self, width: u16) -> &str {
        self.hints
            .iter()
            .map(String::as_str)
            .chain(["Esc", ""])
            .find(|hint| Line::raw(*hint).width() <= usize::from(width))
            .unwrap_or_default()
    }
}

impl<'a> WorkflowScreenModel<'a> {
    pub(in crate::tui) fn project(state: &'a State, zone: Tz, keys: &Keymap) -> Self {
        let refresh = keys.label(Action::Reload);
        let help = keys.label(Action::Help);
        let vertical = keys.vertical_label();
        let loading_step = state.is_loading_step();
        let (panel, hint, compact_hint) = match (&state.panel, state.selected()) {
            (Panel::Confirm(decision), _) => (
                WorkflowPanel::Confirm(WorkflowConfirmation::project(decision, keys)),
                String::new(),
                String::new(),
            ),
            (_, None) => {
                let label = if state.is_loading_page() {
                    "Loading workflow runs…".into()
                } else if state.loaded {
                    "No workflow runs".into()
                } else {
                    format!("Run list unavailable · {refresh} retry")
                };
                (
                    WorkflowPanel::Empty(label),
                    format!("{refresh} refresh  {help} help"),
                    format!("{refresh} refresh  {help}"),
                )
            }
            (
                Panel::Trace {
                    index,
                    text,
                    viewport,
                },
                Some(run),
            ) => {
                let horizontal = keys.horizontal_label();
                let (hint, compact_hint) = if loading_step {
                    ("Esc cancel · Loading step…".into(), "Esc cancel".into())
                } else {
                    (
                        format!("Esc runs  {horizontal} step  {vertical} scroll  w wrap  HL pan"),
                        format!("Esc  {horizontal} step  {vertical}"),
                    )
                };
                (
                    WorkflowPanel::Trace {
                        title: format!(" Step {}/{} · {} ", index + 1, run.step_count, run.id),
                        text,
                        viewport,
                    },
                    hint,
                    compact_hint,
                )
            }
            (Panel::Runs | Panel::InspectDecision { .. }, Some(run)) => {
                let rows = state
                    .runs
                    .iter()
                    .map(|run| {
                        let label = state_label(run.state);
                        let node = run
                            .current_node
                            .as_deref()
                            .map(super::display_line)
                            .unwrap_or_default();
                        let id = run.id.to_string();
                        format!("{label}  {}  {node}", &id[..8])
                    })
                    .collect();
                let mut details = vec![
                    run.id.to_string(),
                    format!(
                        "{}  ·  {} recorded steps",
                        state_label(run.state),
                        run.step_count
                    ),
                ];
                if let Some(node) = &run.current_node {
                    details.push(format!("Node  {}", super::display_line(node)));
                }
                if let Some(due) = run.due_at {
                    details.push(format!(
                        "Due   {}",
                        due.with_timezone(&zone).format("%Y-%m-%d %H:%M:%S %:z")
                    ));
                }
                if let Some(detail) = &run.detail {
                    details.push(super::display_line(detail));
                }
                details.push(format!("Revision  {}", super::display_line(&run.revision)));
                let loading_page = state.is_loading_page();
                let inspecting = matches!(state.panel, Panel::InspectDecision { .. });
                let older = state.next_before.is_some();
                let (hint, compact_hint) = if loading_step || inspecting {
                    (
                        format!("Esc cancel  {vertical} runs  {help} help"),
                        format!("Esc cancel  {vertical}"),
                    )
                } else if loading_page {
                    let hint = format!("{vertical} runs  {help} help");
                    (hint.clone(), hint)
                } else {
                    match (run.state == RunState::Waiting, run.step_count > 0, older) {
                        (true, true, _) => (
                            format!(
                                "{vertical} runs  Enter trace  a approve  d reject  {refresh} refresh"
                            ),
                            format!("a approve  d reject  {help}"),
                        ),
                        (true, false, _) => (
                            format!("{vertical} runs  a approve  d reject  {refresh} refresh"),
                            format!("a approve  d reject  {help}"),
                        ),
                        (false, true, true) => (
                            format!("{vertical} runs  Enter trace  n older  {refresh} refresh"),
                            format!("{vertical}  n older  {refresh}"),
                        ),
                        (false, true, false) => (
                            format!("{vertical} runs  Enter trace  {refresh} refresh"),
                            format!("{vertical}  Enter  {refresh}"),
                        ),
                        (false, false, true) => (
                            format!("{vertical} runs  n older  {refresh} refresh"),
                            format!("{vertical}  n older  {refresh}"),
                        ),
                        (false, false, false) => {
                            let hint = format!("{vertical} runs  {refresh} refresh  {help}");
                            (hint.clone(), hint)
                        }
                    }
                };
                (
                    WorkflowPanel::Runs {
                        title: if loading_step {
                            " Loading step… "
                        } else if inspecting {
                            " Loading review… "
                        } else if loading_page {
                            " Loading runs… "
                        } else if older {
                            " Runs · n older "
                        } else {
                            " Runs "
                        },
                        rows,
                        selected: state.row,
                        details,
                    },
                    hint,
                    compact_hint,
                )
            }
        };
        Self {
            panel,
            hint,
            compact_hint,
        }
    }
}

fn state_label(state: RunState) -> &'static str {
    match state {
        RunState::Pending => "Pending",
        RunState::Running => "Running",
        RunState::Waiting => "Waiting",
        RunState::Succeeded => "Succeeded",
        RunState::Failed => "Failed",
        RunState::Cancelling => "Cancelling",
        RunState::Cancelled => "Cancelled",
        RunState::OutcomeUnknown => "Outcome unknown",
    }
}
