use chrono_tz::Tz;
use mcbtodo_protocol::workflow::RunState;
use ratatui::{
    Frame,
    buffer::CellWidth,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, ListItem, Paragraph, Wrap},
};

use crate::tui::workflow::{Decision, Panel, State};

pub(super) fn hint(state: &State) -> &'static str {
    match state.panel {
        Panel::Runs | Panel::InspectDecision { .. } => {
            "jk runs  Enter trace  a approve  d reject  n older  r refresh"
        }
        Panel::Trace { .. } => "hl step  jk scroll  HL pan  Esc runs",
        Panel::Confirm(_) => "",
    }
}

pub(super) fn draw(frame: &mut Frame<'_>, area: Rect, state: &State, zone: Tz) {
    match (&state.panel, state.selected()) {
        (Panel::Confirm(decision), _) => draw_confirmation(frame, area, decision),
        (_, None) => {
            let label = if state.loaded {
                "No workflow runs"
            } else {
                "Run list unavailable · r retry"
            };
            frame.render_widget(
                Paragraph::new(label).block(Block::bordered().title(" Runs ")),
                area,
            );
        }
        (
            Panel::Trace {
                index,
                text,
                scroll,
            },
            Some(run),
        ) => frame.render_widget(
            Paragraph::new(text.as_str())
                .block(Block::bordered().title(format!(
                    " Step {}/{} · {} ",
                    index + 1,
                    run.step_count,
                    run.id
                )))
                .scroll(*scroll),
            area,
        ),
        (Panel::Runs | Panel::InspectDecision { .. }, Some(run)) => {
            let [list_area, details] = if area.width >= 96 && area.height >= 6 {
                Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
                    .areas(area)
            } else if area.height >= 9 {
                Layout::vertical([Constraint::Min(3), Constraint::Length(6)]).areas(area)
            } else {
                [area, Rect::ZERO]
            };
            let items = state
                .runs
                .iter()
                .map(|run| {
                    let label = state_label(run.state);
                    let node = run.current_node.as_deref().map(visible).unwrap_or_default();
                    let id = run.id.to_string();
                    ListItem::new(format!("{label}  {}  {node}", &id[..8]))
                })
                .collect();
            let title = if state.next_before.is_some() {
                " Runs · n older "
            } else {
                " Runs "
            };
            super::render_list(
                frame,
                list_area,
                items,
                state.row,
                Block::bordered().title(title),
            );
            let mut lines = vec![
                Line::from(run.id.to_string()),
                Line::from(format!(
                    "{}  ·  {} recorded steps",
                    state_label(run.state),
                    run.step_count
                )),
            ];
            if let Some(node) = &run.current_node {
                lines.push(Line::from(format!("Node  {}", visible(node))));
            }
            if let Some(due) = run.due_at {
                lines.push(Line::from(format!(
                    "Due   {}",
                    due.with_timezone(&zone).format("%Y-%m-%d %H:%M:%S %:z")
                )));
            }
            if let Some(detail) = &run.detail {
                lines.push(Line::from(visible(detail)));
            }
            lines.push(Line::from(format!("Revision  {}", visible(&run.revision))));
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(Block::bordered().title(" Selected run ")),
                details,
            );
        }
    }
}

fn draw_confirmation(frame: &mut Frame<'_>, area: Rect, decision: &Decision) {
    let [body, footer] = if area.height >= 2 {
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area)
    } else {
        [area, Rect::ZERO]
    };
    let content = if body.width >= 5 && body.height >= 3 {
        let block = Block::bordered().title(" Confirm decision ");
        let content = block.inner(body);
        frame.render_widget(block, body);
        content
    } else {
        body
    };
    if content.is_empty() {
        return;
    }

    let rows = review_rows(decision.text(), usize::from(content.width));
    let viewport = decision.visible_rows(rows.len(), content.height);
    let lines = rows[viewport].iter().map(Line::raw).collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), content);

    let verb = decision.verb().to_ascii_lowercase();
    let keys = match footer.width {
        50.. => format!("y {verb}  Esc cancel  ↑↓/jk  PgUp/PgDn  Home/End"),
        25.. => format!("y {verb}  Esc cancel  ↑↓"),
        17.. => format!("↑↓  y {verb}  Esc"),
        _ => "↑↓ y Esc".to_owned(),
    };
    frame.render_widget(Paragraph::new(keys), footer);
}

fn review_rows(text: &str, width: usize) -> Vec<String> {
    // Wrap at grapheme boundaries so exact values retain spaces and long tokens fit.
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        let mut used = 0;
        let mut push = |symbol: &str, columns: usize| {
            if used + columns > width {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push_str(symbol);
            used += columns;
        };
        let span = Span::raw(line);
        for grapheme in span.styled_graphemes(Style::default()) {
            let columns = usize::from(grapheme.symbol.cell_width());
            if columns == 0 || columns > width {
                for ch in grapheme.symbol.chars().flat_map(char::escape_unicode) {
                    push(ch.encode_utf8(&mut [0; 4]), 1);
                }
            } else {
                push(grapheme.symbol, columns);
            }
        }
        rows.push(row);
    }
    rows
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

fn visible(text: &str) -> String {
    text.replace(char::is_control, " ")
}
