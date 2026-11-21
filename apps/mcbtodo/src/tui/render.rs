mod schedule;
mod workflow;

use chrono::Datelike;
use mcbtodo_application::{project_calendar_all, project_inbox, project_outline};
use mcbtodo_domain::CalendarKind;
use mcbtodo_vault::Vault;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap},
};

use super::{
    Overlay, State, View, creation, graph,
    time_block::{Field, Form},
    timeline,
};

pub(super) fn draw(frame: &mut Frame<'_>, state: &State, vault: &Vault) {
    let area = frame.area();
    let [area, warning] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(state.vault_warning.is_some())),
    ])
    .areas(area);
    if let Some(message) = &state.vault_warning {
        frame.render_widget(
            Paragraph::new(message.replace(char::is_control, " "))
                .style(Style::new().fg(Color::Yellow)),
            warning,
        );
    }
    if state.view == View::Workflow
        && matches!(state.workflow.panel, super::workflow::Panel::Confirm(_))
    {
        workflow::draw(frame, area, &state.workflow, vault.timezone());
        return;
    }
    let [top, body, feedback, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let mode = match state.view {
        View::Graph => state
            .graph_selection
            .as_ref()
            .map(|selection| format!("SELECT · {} nodes", selection.len())),
        view if view.uses_dates() && state.anchor.is_some() => Some("SELECT".into()),
        _ => None,
    };
    let mode = match &state.overlay {
        Overlay::TimeBlock(form) if form.unit.is_none() => "PICKER",
        Overlay::Picker { .. } => "PICKER",
        Overlay::Command(_) => "COMMAND",
        Overlay::CreateForm(_) | Overlay::Deadline(_) | Overlay::TimeBlock(_) => "INSERT",
        Overlay::Proposals => "REVIEW",
        _ => mode.as_deref().unwrap_or("NORMAL"),
    };
    let (start, end) = state.selected_range();
    let range = if start == end {
        start.to_string()
    } else {
        format!(
            "{} → {} · {} days",
            start,
            end,
            (end - start).num_days() + 1
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(state.view.label(), Style::new().fg(Color::Cyan).bold()),
                Span::raw(if state.view == View::Workflow {
                    " runs".into()
                } else if start == end || !state.view.uses_dates() {
                    format!("  {}", state.day)
                } else {
                    format!("  {}  ·  {range}", state.day)
                }),
            ]),
            Line::styled(
                if state.view == View::Workflow {
                    String::from("Local daemon")
                } else {
                    vault.timezone().to_string()
                },
                Style::new().add_modifier(Modifier::DIM),
            ),
        ]),
        top,
    );
    if !state.message.is_empty() {
        let message = state.message.lines().next().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(message.replace(char::is_control, " ")),
            feedback,
        );
    }
    let hint = match &state.overlay {
        Overlay::None => match state.view {
            View::Month => "hjkl move  v select  Enter agenda  / search  Space menu",
            View::Week if body.width < 72 || body.height < 8 => {
                "hl day  PgUp/Dn week  Enter day  v select  / search  Space"
            }
            View::Week => "hl day  jk hours  PgUp/Dn week  v select  Enter day  / search  Space",
            View::Day => {
                "hl date  jk entry  g Graph  v select  e edit  Enter agenda  / search  Space"
            }
            View::Agenda => "hl date  jk move  g Graph  e edit  / search  Space menu",
            View::Outline => "jk move  g Graph  e edit  / search  Space menu",
            View::Graph => "jk nodes  hl edges  v/x select  e edit  Space",
            View::Timeline => "hl date  jk plan  e edit  Space menu",
            View::Workflow => workflow::hint(&state.workflow),
            View::Inbox => "jk select  t assign to Project  e edit  Space menu",
        },
        Overlay::Views => {
            frame.render_widget(
                Paragraph::new(" Views  m Month  w Week  d Day  t Timeline  i Inbox"),
                feedback,
            );
            frame.render_widget(
                Paragraph::new(" a Agenda  l Outline  g Graph  f Workflow"),
                footer,
            );
            render_view(frame, body, state, vault);
            return;
        }
        Overlay::Space if state.view.uses_dates() || state.view == View::Outline => {
            "c create  d deadline  u views  t time  a AI proposals  r reload  ?"
        }
        Overlay::Space => "d deadline  u views  t time  a AI proposals  r reload  ?",
        Overlay::Create => {
            let mut options = vec![
                "p  Project",
                "e  All-day event",
                "d  Deadline",
                "r  Reminder",
                "b  Time block",
            ];
            if state.view == View::Outline && state.selected_project_revision(vault).is_some() {
                options.insert(1, "t  Task in selected Project");
            }
            frame.render_widget(
                List::new(options).block(Block::bordered().title(" Create ")),
                body,
            );
            "Esc back"
        }
        Overlay::CreateForm(form) => {
            draw_creation(frame, body, form);
            if form.selected + 1 == form.fields.len() {
                if matches!(form.kind, creation::Kind::Reminder) {
                    "Enter save  Tab field  RFC3339 with offset  Esc cancel"
                } else {
                    "Enter save  Tab field  Esc cancel"
                }
            } else {
                "Enter next  Tab field  Esc cancel"
            }
        }
        Overlay::Deadline(form) => {
            let block = Block::bordered().title(format!(" Deadline · {} ", form.id));
            let value = if form.cleared {
                String::new()
            } else {
                form.value.clone()
            };
            let lines = vec![
                Line::from(vec![
                    Span::styled("> Due  ", Style::new().fg(Color::Cyan).bold()),
                    Span::raw(value),
                    Span::raw("▏"),
                ]),
                Line::from("YYYY-MM-DD · Delete clears the deadline"),
            ];
            frame.render_widget(Paragraph::new(lines).block(block), body);
            "Enter save  Delete clear  Esc cancel"
        }
        Overlay::Time if state.view.uses_dates() => {
            "b add time block  i status  s start  p pause  x stop  Esc"
        }
        Overlay::Time => "i status  s start  p pause  x stop  Esc",
        Overlay::TimeBlock(form) => {
            draw_time_block(frame, body, form, vault);
            if form.unit.is_none() {
                "↑↓ WorkUnit  Enter choose  Esc cancel"
            } else if form.field == Field::Start {
                "Enter next  Tab switch field  Esc WorkUnits"
            } else {
                "Enter save  Tab switch field  Esc WorkUnits"
            }
        }
        Overlay::GraphEdit(edit) => {
            let title = |id: mcbtodo_domain::Id| {
                vault
                    .work_unit(id)
                    .map(|unit| unit.title.as_str())
                    .unwrap_or("<missing WorkUnit>")
            };
            let mut lines = vec![
                Line::styled(
                    format!("Target: {}", title(edit.target)),
                    Style::new().fg(Color::Cyan).bold(),
                ),
                Line::from("Prerequisite → dependent"),
                Line::styled("Add edges", Style::new().bold()),
            ];
            lines.extend(
                edit.added
                    .iter()
                    .map(|&id| Line::from(format!("+ {} → {}", title(id), title(edit.target)))),
            );
            lines.push(Line::styled("Remove edges", Style::new().bold()));
            lines.extend(
                edit.removed
                    .iter()
                    .map(|&id| Line::from(format!("- {} → {}", title(id), title(edit.target)))),
            );
            lines.push(Line::styled("Readiness impact", Style::new().bold()));
            if edit.newly_blocked.is_empty() && edit.unblocked.is_empty() {
                lines.push(Line::from("No WorkUnit blocking status changes"));
            } else {
                lines.extend(
                    edit.newly_blocked
                        .iter()
                        .map(|&id| Line::from(format!("Blocked: {}", title(id)))),
                );
                lines.extend(
                    edit.unblocked
                        .iter()
                        .map(|&id| Line::from(format!("Unblocked: {}", title(id)))),
                );
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .block(Block::bordered().title(" Review dependency changes "))
                    .wrap(Wrap { trim: true }),
                body,
            );
            "y / Enter apply  Esc / n cancel"
        }
        Overlay::Proposals => {
            let review = &state.proposals;
            let block = Block::bordered().title(" AI proposals ");
            if let Some(detail) = &review.detail {
                let title = match detail.kind {
                    super::proposal_review::DetailKind::Proposal => " AI proposal ",
                    super::proposal_review::DetailKind::GraphDiff => " Graph Diff ",
                    super::proposal_review::DetailKind::WorkflowSnapshot => " Workflow snapshot ",
                };
                let block = Block::bordered()
                    .title(Line::styled(title, Style::new().fg(Color::Cyan).bold()));
                frame.render_widget(
                    Paragraph::new(detail.text.as_str())
                        .block(block)
                        .scroll((review.scroll, review.pan)),
                    body,
                );
                if detail.can_approve() {
                    "a approve  x reject  Esc list  jk scroll  hl pan"
                } else {
                    "Esc list  jk scroll  hl pan"
                }
            } else {
                if review.rows.is_empty() {
                    let empty = if state.message.is_empty() {
                        "No proposals for this Vault and timezone"
                    } else {
                        "Proposal list unavailable"
                    };
                    frame.render_widget(Paragraph::new(empty).block(block), body);
                } else {
                    let items = review
                        .rows
                        .iter()
                        .map(|proposal| ListItem::new(super::proposal_review::label(proposal)))
                        .collect();
                    render_list(frame, body, items, review.row, block);
                }
                "jk select  Enter review  r refresh  Esc back"
            }
        }
        Overlay::Help => "Esc back",
        Overlay::Command(input) => {
            frame.render_widget(Paragraph::new(format!(":{input}")), footer);
            render_view(frame, body, state, vault);
            return;
        }
        Overlay::Picker { query, row } => {
            let [query_area, results_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body);
            frame.render_widget(Paragraph::new(format!("/{query}▏")), query_area);
            let matches = super::picker::matches(vault, query);
            if matches.is_empty() {
                frame.render_widget(
                    Paragraph::new("No matching documents")
                        .block(Block::bordered().title(" Picker ")),
                    results_area,
                );
            } else {
                let items = matches
                    .into_iter()
                    .map(|item| ListItem::new(format!("{}{}", "  ".repeat(item.depth), item.title)))
                    .collect();
                render_list(
                    frame,
                    results_area,
                    items,
                    *row,
                    Block::bordered().title(" Picker "),
                );
            }
            "Type to search  ↑↓ move  Enter open  Esc cancel"
        }
        Overlay::InboxProject { row, .. } => {
            let projects = super::project_rows(vault);
            let items = projects
                .iter()
                .map(|(_, title)| ListItem::new(title.as_str()))
                .collect();
            list(frame, body, items, *row, "No Projects available");
            "jk select Project  Enter assign  Esc cancel"
        }
    };
    let footer_text = format!(" {mode}  {hint}");
    let footer_widget = if matches!(state.overlay, Overlay::Proposals) {
        Paragraph::new(Line::styled(footer_text, Style::new().fg(Color::Cyan)))
    } else {
        Paragraph::new(footer_text)
    };
    frame.render_widget(footer_widget, footer);
    if matches!(state.overlay, Overlay::Help) {
        let help = [
            "/   Search Projects, Tasks and WorkUnits; Enter focuses in Outline",
            "Space u m/w/d/a/l/g/f/i   Switch views, including Inbox",
            "Space u t   Timeline",
            "Space c p/e/d/b   Create project / all-day event / deadline / time block",
            "Outline: select a Project, then Space c t to capture a Task",
            "Space d   Edit deadline on the selected Project, Task or WorkUnit",
            "Space t   Time operations",
            "Space t b   Add a time block on the calendar date",
            "Space a   Review AI proposals",
            "Inbox: jk select, t assign entry to a Project",
            "v   Select an inclusive date range",
            "Week: Enter opens Day, PgUp/PgDn moves by week",
            "Month / Day: Enter opens selection in Agenda",
            "Day / Agenda / Outline: g opens Graph on the focused WorkUnit",
            "Graph: jk nodes, hl edges, v/x select sources, a add edges, d remove edges",
            "Graph dependency edit: review edge and blocking changes, y apply, Esc cancel",
            "Workflow runs: jk select, n older, r refresh, Enter trace",
            "Workflow decisions: a approve, d reject, y confirm",
            "Workflow confirmation: jk scroll, PgUp/PgDn page, Home/End jump, Esc cancel",
            "Workflow trace: hl step, jk scroll, HL pan, Esc back",
            "e   Open source in $VISUAL / $EDITOR / hx",
            "r   Reload vault",
            ":   view, search, timer, proposals, date, today, reload, quit",
            ":help   Show this help",
            "q   Quit",
        ];
        frame.render_widget(
            List::new(help).block(Block::bordered().title(" Help ")),
            body,
        );
    } else if !matches!(
        state.overlay,
        Overlay::Create
            | Overlay::CreateForm(_)
            | Overlay::Deadline(_)
            | Overlay::TimeBlock(_)
            | Overlay::GraphEdit(_)
            | Overlay::Proposals
            | Overlay::Picker { .. }
            | Overlay::InboxProject { .. }
    ) {
        render_view(frame, body, state, vault);
    }
}

fn draw_creation(frame: &mut Frame<'_>, area: Rect, form: &creation::Form) {
    let mut lines = Vec::new();
    for (index, field) in form.fields.iter().enumerate() {
        let selected = index == form.selected;
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} {}  ", if selected { ">" } else { " " }, field.label),
                if selected {
                    Style::new().fg(Color::Cyan).bold()
                } else {
                    Style::new()
                },
            ),
            if let Some(date) = field.default_date.filter(|_| field.value.is_empty()) {
                Span::styled(date.to_string(), Style::new().dim())
            } else {
                Span::raw(field.value.as_str())
            },
            Span::raw(if selected { "▏" } else { "" }),
        ]));
    }
    lines.push(Line::from(""));
    let hint = if matches!(form.kind, creation::Kind::Reminder) {
        "Use YYYY-MM-DDTHH:MM:SS+HH:MM with the Vault timezone offset."
    } else {
        "Type a date to replace the default (YYYY-MM-DD)."
    };
    lines.push(Line::from(hint));
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(format!(" New {} ", form.kind.label().to_lowercase())))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_time_block(frame: &mut Frame<'_>, area: Rect, form: &Form, vault: &Vault) {
    let block = Block::bordered().title(format!(" Time block · {} ", form.day));
    let body = block.inner(area);
    frame.render_widget(block, area);
    let choices = form.choices(vault);
    if let Some(unit) = form.unit {
        let title = choices
            .iter()
            .find(|choice| choice.id == unit)
            .map_or("Unavailable", |choice| choice.title.as_str());
        let field = |label, value, active| {
            if active {
                Line::styled(
                    format!("> {label} {value}▏"),
                    Style::new().fg(Color::Cyan).bold(),
                )
            } else {
                Line::from(format!("  {label} {value}"))
            }
        };
        let rows = vec![
            Line::from(format!("{title} · busy · {}", vault.timezone())),
            field("Start", &form.start, form.field == Field::Start),
            field("End  ", &form.end, form.field == Field::End),
            Line::from("HH:MM · append ±HH:MM when a clock time occurs twice"),
            Line::from("End accepts YYYY-MM-DD HH:MM or 24:00 for midnight"),
        ];
        frame.render_widget(Paragraph::new(rows).wrap(Wrap { trim: false }), body);
    } else {
        let [query, choices_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body);
        frame.render_widget(
            Paragraph::new(format!("Find WorkUnit: {}▏", form.query)),
            query,
        );
        let items = choices
            .into_iter()
            .map(|choice| {
                ListItem::new(format!(
                    "{}  ·  {} / {}",
                    choice.title, choice.project, choice.task
                ))
            })
            .collect();
        if form.query.is_empty() {
            list(frame, choices_area, items, form.row, "No WorkUnits");
        } else {
            list(
                frame,
                choices_area,
                items,
                form.row,
                "No matching WorkUnits",
            );
        }
    }
}

fn render_view(frame: &mut Frame<'_>, area: Rect, state: &State, vault: &Vault) {
    match state.view {
        View::Month => month(frame, area, state, vault),
        View::Week => schedule::week(frame, area, state, vault),
        View::Day => schedule::day(frame, area, state, vault),
        View::Agenda => {
            let items = state
                .agenda(vault)
                .iter()
                .map(|entry| {
                    ListItem::new(crate::calendar_format::item_label(entry, vault.timezone()))
                })
                .collect();
            list(
                frame,
                area,
                items,
                state.row,
                "No entries for the selection",
            );
        }
        View::Outline => {
            let items = project_outline(vault)
                .into_iter()
                .map(|row| {
                    let blocked = if row.blocked { "  [blocked]" } else { "" };
                    ListItem::new(format!("{}{}{blocked}", "  ".repeat(row.depth), row.title))
                })
                .collect();
            list(frame, area, items, state.row, "No projects in the vault");
        }
        View::Graph => draw_graph(frame, area, state, vault),
        View::Timeline => draw_timeline(frame, area, state, vault),
        View::Workflow => workflow::draw(frame, area, &state.workflow, vault.timezone()),
        View::Inbox => {
            let items = project_inbox(vault)
                .into_iter()
                .filter(|entry| entry.project_id.is_none())
                .map(|entry| {
                    let captured = entry.captured_at.with_timezone(&vault.timezone());
                    let tags = if entry.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" · #{}", entry.tags.join(" #"))
                    };
                    ListItem::new(format!(
                        "{} · {}{}",
                        captured.format("%Y-%m-%d %H:%M"),
                        entry.title,
                        tags
                    ))
                })
                .collect();
            list(frame, area, items, state.row, "Inbox is empty");
        }
    }
}

fn draw_timeline(frame: &mut Frame<'_>, area: Rect, state: &State, vault: &Vault) {
    let start = timeline::window_start(state.day);
    let entries = timeline::entries(vault, state.day);
    if entries.is_empty() {
        list(
            frame,
            area,
            Vec::new(),
            0,
            "No planned entries in this window",
        );
        return;
    }
    let dates = (0..timeline::WINDOW_DAYS)
        .map(|offset| start + chrono::Days::new(offset))
        .collect::<Vec<_>>();
    let header = Row::new(
        std::iter::once(Cell::from("Plan")).chain(dates.iter().map(|date| {
            let style = if *date == state.day {
                Style::new().fg(Color::Black).bg(Color::Cyan).bold()
            } else {
                Style::new().add_modifier(Modifier::DIM)
            };
            Cell::from(date.format("%d").to_string()).style(style)
        })),
    );
    let rows = entries
        .iter()
        .map(|entry| {
            let marker = match entry.kind {
                CalendarKind::ActivePeriod => "A",
                CalendarKind::Event => "E",
                CalendarKind::TaskOccurrence => "T",
                CalendarKind::TimeBlock => "B",
                CalendarKind::Deadline => "!",
            };
            Row::new(
                std::iter::once(Cell::from(format!("{marker} {}", entry.title))).chain(
                    dates.iter().map(|date| {
                        let style = if *date == state.day {
                            Style::new()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::REVERSED)
                        } else {
                            Style::default()
                        };
                        Cell::from(timeline::cell(entry, *date, vault)).style(style)
                    }),
                ),
            )
        })
        .collect::<Vec<_>>();
    let constraints = std::iter::once(Constraint::Min(20))
        .chain(std::iter::repeat_n(Constraint::Length(2), dates.len()));
    let table = Table::new(rows, constraints)
        .column_spacing(0)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">");
    let mut selected = TableState::default();
    selected.select(Some(state.row));
    frame.render_stateful_widget(
        table.block(
            Block::bordered()
                .title(format!(
                    " {} → {} ",
                    start,
                    dates.last().expect("timeline has dates")
                ))
                .title_bottom(
                    " A active  E event  T task  B block  !! deadline  >> starts  == continues ",
                ),
        ),
        area,
        &mut selected,
    );
}

fn draw_graph(frame: &mut Frame<'_>, area: Rect, state: &State, vault: &Vault) {
    let nodes = graph::nodes(vault);
    let Some(focused) = nodes.get(state.row) else {
        list(frame, area, Vec::new(), 0, "No work units in the vault");
        return;
    };
    let [list_area, details] = if area.width >= 92 {
        Layout::horizontal([Constraint::Percentage(54), Constraint::Percentage(46)]).areas(area)
    } else if area.height >= 10 {
        Layout::vertical([
            Constraint::Min(3),
            Constraint::Length((area.height / 2).min(9)),
        ])
        .areas(area)
    } else {
        [area, Rect::ZERO]
    };
    let items = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let marked = state
                .graph_selection
                .as_ref()
                .is_some_and(|selection| selection.contains(&node.id));
            let marker = if marked { '*' } else { ' ' };
            let blocked = if node.blocked { " [blocked]" } else { "" };
            ListItem::new(format!("{marker} {}  {}{blocked}", index + 1, node.title))
        })
        .collect();
    render_list(
        frame,
        list_area,
        items,
        state.row,
        Block::bordered().title(" Work units "),
    );
    if details.height < 4 {
        return;
    }
    let available = usize::from(details.height - 4);
    let prerequisites = focused.prerequisites.len();
    let dependents = focused.dependents.len();
    let incoming_rows = prerequisites.min(available - dependents.min(available / 2));
    let outgoing_rows = dependents.min(available - incoming_rows);
    let mut lines = Vec::new();
    for (label, arrow, neighbors, limit) in [
        ("Prerequisites", "←", &focused.prerequisites, incoming_rows),
        ("Dependents", "→", &focused.dependents, outgoing_rows),
    ] {
        let count = neighbors.len();
        let overflow = if count > limit {
            format!(" · +{} more", count - limit)
        } else {
            String::new()
        };
        lines.push(Line::styled(
            format!("{label} ({count}){overflow}"),
            Style::new().bold(),
        ));
        for &neighbor in neighbors.iter().take(limit) {
            lines.push(Line::from(format!(
                "{arrow} {}  {}",
                neighbor + 1,
                nodes[neighbor].title
            )));
        }
    }
    let block = Block::bordered().title(format!(" {}  {} ", state.row + 1, focused.title));
    frame.render_widget(Paragraph::new(lines).block(block), details);
}

fn month(frame: &mut Frame<'_>, area: Rect, state: &State, vault: &Vault) {
    let first = state.day.with_day(1).expect("valid day of month");
    let monday_offset = first.weekday().num_days_from_monday() as usize;
    let days: Vec<_> = first
        .iter_days()
        .take_while(|day| day.month() == first.month())
        .collect();
    let weeks = (days.len() + monday_offset).div_ceil(7);
    let entries = project_calendar_all(vault);
    let zone = vault.timezone();
    let (start, end) = state.selected_range();
    let compact = area.width < 58;
    let tall = !compact && usize::from(area.height) > 2 + 2 * weeks;
    let rows = (0..weeks).map(|week| {
        let cells = (0..7).map(|weekday| {
            let index = week * 7 + weekday;
            let Some(&day) = index
                .checked_sub(monday_offset)
                .and_then(|index| days.get(index))
            else {
                return Cell::default();
            };
            let mut busy = 0;
            let mut due = false;
            let mut active = false;
            let mut other = false;
            for entry in entries.iter().filter(|entry| entry.on_day(day, zone)) {
                busy += usize::from(entry.busy);
                match entry.kind {
                    CalendarKind::ActivePeriod => active = true,
                    CalendarKind::Deadline => due = true,
                    _ => other |= !entry.busy,
                }
            }
            let style = if day == state.day {
                Style::new().fg(Color::Black).bg(Color::Cyan).bold()
            } else if (start..=end).contains(&day) {
                Style::new()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let marker = if busy > 0 {
                "*"
            } else if due {
                "!"
            } else if active || other {
                "."
            } else {
                " "
            };
            let mut lines = vec![Line::from(format!("{:>2}{marker}", day.day()))];
            if tall {
                let counts = if busy > 0 {
                    format!("{busy} busy")
                } else {
                    String::new()
                };
                let marks = format!(
                    "{}{}",
                    if due { "!" } else { "" },
                    if active || other { "." } else { "" }
                );
                lines.push(Line::from(format!("{counts} {marks}")));
            }
            Cell::from(lines).style(style)
        });
        Row::new(cells).height(if tall { 2 } else { 1 })
    });
    let [grid, legend] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let headings = if compact {
        ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
    } else {
        ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
    };
    let table = Table::new(rows, [Constraint::Ratio(1, 7); 7])
        .column_spacing(0)
        .header(Row::new(headings).style(Style::new().bold()))
        .block(Block::bordered().title(format!(" {} ", first.format("%Y-%m"))));
    frame.render_widget(table, grid);
    frame.render_widget(Paragraph::new("* Busy   ! Due   . Active / free"), legend);
}

fn list(frame: &mut Frame<'_>, area: Rect, items: Vec<ListItem<'_>>, row: usize, empty: &str) {
    let block = Block::bordered();
    if items.is_empty() {
        frame.render_widget(Paragraph::new(empty).block(block), area);
        return;
    }
    render_list(frame, area, items, row, block);
}

fn render_list(
    frame: &mut Frame<'_>,
    area: Rect,
    items: Vec<ListItem<'_>>,
    row: usize,
    block: Block<'_>,
) {
    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(Style::new().fg(Color::Cyan).bold());
    let mut state = ListState::default().with_selected(Some(row));
    frame.render_stateful_widget(list, area, &mut state);
}
