mod schedule;
mod workflow;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap},
};

use super::{
    screen_model::{
        Content, CreationScreenModel, DeadlineScreenModel, GraphEditScreenModel, GraphScreenModel,
        MonthScreenModel, OutlineScreenModel, PickerScreenModel, ProposalPanel, ScreenFooter,
        ScreenModel, SettingsRow, SettingsScreenModel, TimeBlockPanel, TimeBlockScreenModel,
        TimelineScreenModel, WorkflowPanel,
    },
    theme::Theme,
};

pub(super) fn draw(frame: &mut Frame<'_>, model: &ScreenModel<'_>, theme: Theme) {
    let chrome = &model.chrome;
    let area = frame.area();
    let [area, warning] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(chrome.warning.is_some())),
    ])
    .areas(area);
    if let Some(message) = &chrome.warning {
        frame.render_widget(
            Paragraph::new(message.as_str()).style(theme.warning()),
            warning,
        );
    }
    if let Content::Workflow(workflow_model) = &model.content
        && matches!(workflow_model.panel, WorkflowPanel::Confirm(_))
    {
        workflow::draw(frame, area, workflow_model, theme);
        return;
    }
    let compact = area.height < 7;
    // Keep a body row before allocating space to the surrounding chrome.
    let footer_rows = u16::from(area.height >= 2);
    let feedback_rows =
        u16::from((!compact || !chrome.feedback.is_empty()) && area.height > 1 + footer_rows);
    let header_rows = area
        .height
        .saturating_sub(1 + footer_rows + feedback_rows)
        .min(if compact { 1 } else { 2 });
    let [top, body, feedback, footer] = Layout::vertical([
        Constraint::Length(header_rows),
        Constraint::Min(0),
        Constraint::Length(feedback_rows),
        Constraint::Length(footer_rows),
    ])
    .areas(area);
    let mode_line = Line::from(chrome.mode.as_str());
    let show_mode = chrome.active_mode || area.width >= 32;
    let mode_width = (mode_line.width() + 1).min(area.width.saturating_sub(10) as usize);
    let [context, mode_badge] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(if show_mode { mode_width as u16 } else { 0 }),
    ])
    .areas(top);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(chrome.title, theme.accent()),
                Span::raw(chrome.context.as_str()),
            ]),
            Line::styled(chrome.subtitle.as_str(), theme.muted()),
        ]),
        context,
    );
    frame.render_widget(
        Paragraph::new(mode_line)
            .right_aligned()
            .style(if chrome.active_mode {
                theme.focus()
            } else {
                theme.muted()
            }),
        mode_badge,
    );
    if !chrome.feedback.is_empty() {
        frame.render_widget(Paragraph::new(chrome.feedback.as_str()), feedback);
    }
    let hint = match &model.content {
        Content::Menu {
            title,
            viewport,
            text,
        } => {
            viewport.draw(frame, body, title, text, theme);
            "Esc back  ↑↓ scroll"
        }
        Content::Settings(settings) => {
            draw_settings(frame, body, settings, theme);
            settings.hint(footer.width)
        }
        Content::CreateForm(form) => {
            draw_creation(frame, body, form, theme);
            let actions = if footer.width < 34 {
                "  Tab  Esc"
            } else {
                "  Tab field  Esc cancel"
            };
            frame.render_widget(
                Paragraph::new(format!(" {}{actions}", form.enter_action)),
                footer,
            );
            return;
        }
        Content::Deadline(form) => {
            draw_deadline(frame, body, form, theme);
            DeadlineScreenModel::hint(footer.width)
        }
        Content::TimeBlock(form) => {
            draw_time_block(frame, body, form, theme);
            form.hint(footer.width)
        }
        Content::GraphEdit(review) => {
            review.viewport.draw(
                frame,
                body,
                " Review dependency changes ",
                &review.text,
                theme,
            );
            GraphEditScreenModel::hint(body.width)
        }
        Content::Proposals(review) => {
            match &review.panel {
                ProposalPanel::Detail {
                    title,
                    text,
                    viewport,
                } => viewport.draw(frame, body, title, text, theme),
                ProposalPanel::List { labels, selected } => render_list(
                    frame,
                    body,
                    labels
                        .iter()
                        .map(|label| ListItem::new(label.as_str()))
                        .collect(),
                    *selected,
                    theme.panel(" AI proposals "),
                    theme,
                ),
                ProposalPanel::Empty(message) => frame.render_widget(
                    Paragraph::new(*message)
                        .block(content_panel(theme.panel(" AI proposals "), body))
                        .wrap(Wrap { trim: true }),
                    body,
                ),
            }
            review.hint(footer.width)
        }
        Content::Help { viewport, text } => {
            viewport.draw(frame, body, " Help ", text, theme);
            if footer.width < 40 {
                "Esc back  ↑↓ scroll"
            } else {
                "Esc back  ↑↓ scroll  PgUp/Dn page  Home/End"
            }
        }
        Content::Picker(picker) => {
            draw_picker(frame, body, picker, Some(" Picker "), theme);
            PickerScreenModel::document_hint(footer.width)
        }
        Content::InboxProject(projects) => {
            let items = projects
                .projects
                .iter()
                .map(|title| ListItem::new(title.as_str()))
                .collect();
            list(
                frame,
                body,
                items,
                projects.focused,
                "No Projects available",
                theme,
            );
            projects.hint(footer.width)
        }
        view => {
            render_view(frame, body, view, theme);
            let hint = model.main_hint(footer.width, body.height);
            if matches!(chrome.footer, ScreenFooter::Command(_)) {
                frame.render_widget(Paragraph::new(hint), footer);
                return;
            }
            hint
        }
    };
    let footer_text = format!(" {hint}");
    let footer_widget = if matches!(model.content, Content::Proposals(_)) {
        Paragraph::new(Line::styled(footer_text, theme.accent()))
    } else {
        Paragraph::new(footer_text)
    };
    frame.render_widget(footer_widget, footer);
}

fn draw_settings(frame: &mut Frame<'_>, area: Rect, model: &SettingsScreenModel<'_>, theme: Theme) {
    let body = if area.width >= 34 && area.height >= 4 {
        let block = theme.panel(model.panel);
        let body = block.inner(area);
        frame.render_widget(block, area);
        body
    } else {
        area
    };
    let lines: Vec<_> = model
        .visible_rows(body.height)
        .map(|row| match row {
            SettingsRow::Section(name) => Line::styled(name, theme.accent()),
            SettingsRow::Preference {
                label,
                value,
                focused,
                cursor,
            } => {
                let marker = if focused { "> " } else { "  " };
                let style = if focused {
                    theme.focus()
                } else {
                    theme.muted()
                };
                let mut line = Line::from(vec![
                    Span::styled(format!("{marker}{label} "), style),
                    Span::raw(value),
                ]);
                if cursor {
                    line.push_span(Span::styled("▏", style));
                    if line.width() > usize::from(body.width) {
                        line = line.right_aligned();
                    }
                }
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
}

fn draw_deadline(frame: &mut Frame<'_>, area: Rect, form: &DeadlineScreenModel<'_>, theme: Theme) {
    let body = if area.height >= 3 && area.width >= 6 {
        let block = theme.panel(form.title(area.width));
        let body = block.inner(area);
        frame.render_widget(block, area);
        body
    } else {
        area
    };
    let label = if body.width >= 18 { "> Due  " } else { "> " };
    let mut value = Line::from(vec![
        Span::styled(label, theme.focus()),
        Span::raw(form.value),
        Span::raw("▏"),
    ]);
    if value.width() > usize::from(body.width) {
        value = value.right_aligned();
    }
    frame.render_widget(
        Paragraph::new(vec![
            value,
            Line::styled(DeadlineScreenModel::instructions(body.width), theme.muted()),
        ]),
        body,
    );
}

fn draw_creation(frame: &mut Frame<'_>, area: Rect, form: &CreationScreenModel<'_>, theme: Theme) {
    let body = if area.height >= 3 && area.width >= 6 {
        let block = theme.panel(form.title.as_str());
        let body = block.inner(area);
        frame.render_widget(block, area);
        body
    } else {
        area
    };
    if body.is_empty() {
        return;
    }

    let visible = usize::from(body.height);
    let first = form.selected.saturating_sub(visible - 1);
    for (row, field) in form.fields.iter().skip(first).take(visible).enumerate() {
        let active = first + row == form.selected;
        let label = Line::styled(
            format!("{} {}  ", if active { ">" } else { " " }, field.label),
            if active {
                theme.focus()
            } else {
                theme.secondary()
            },
        );
        // Leave room for the input and cursor even when the label is long.
        let label_width = label.width().min(usize::from(body.width / 2)) as u16;
        let y = body.y + row as u16;
        frame.render_widget(label, Rect::new(body.x, y, label_width, 1));
        let value_area = Rect::new(body.x + label_width, y, body.width - label_width, 1);
        let mut value = Line::from(vec![
            Span::styled(
                field.value.as_ref(),
                if field.uses_default {
                    theme.muted()
                } else {
                    Style::default()
                },
            ),
            Span::raw(if active { "▏" } else { "" }),
        ]);
        if active && value.width() > usize::from(value_area.width) {
            value = value.right_aligned();
        }
        frame.render_widget(value, value_area);
    }
    if let Some(hint) = form.guidance
        && visible > form.fields.len()
    {
        let remaining = visible - form.fields.len();
        frame.render_widget(
            Paragraph::new(hint)
                .style(theme.muted())
                .wrap(Wrap { trim: true }),
            Rect::new(
                body.x,
                body.y + form.fields.len() as u16,
                body.width,
                remaining as u16,
            ),
        );
    }
}

fn draw_time_block(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &TimeBlockScreenModel<'_>,
    theme: Theme,
) {
    let body = if area.height >= 5 && area.width >= 6 {
        let block = theme.panel(form.title.as_str());
        let body = block.inner(area);
        frame.render_widget(block, area);
        body
    } else {
        area
    };
    match &form.panel {
        TimeBlockPanel::Picker(picker) => draw_picker(frame, body, picker, None, theme),
        TimeBlockPanel::Editor {
            context,
            fields,
            focused,
        } => {
            let [context_area, fields_area, instructions] = Layout::vertical([
                Constraint::Length(u16::from(body.height >= 3)),
                Constraint::Length(body.height.min(2)),
                Constraint::Min(0),
            ])
            .areas(body);
            frame.render_widget(
                Line::styled(context.as_str(), theme.secondary()),
                context_area,
            );
            let first = if fields_area.height == 1 { *focused } else { 0 };
            for (row, &(label, value)) in fields
                .iter()
                .skip(first)
                .take(usize::from(fields_area.height))
                .enumerate()
            {
                let active = first + row == *focused;
                let mut line = Line::styled(
                    format!(
                        "{} {label:5} {value}{}",
                        if active { ">" } else { " " },
                        if active { "▏" } else { "" }
                    ),
                    if active {
                        theme.focus()
                    } else {
                        theme.secondary()
                    },
                );
                if active && line.width() > fields_area.width.into() {
                    line = line.right_aligned();
                }
                frame.render_widget(
                    line,
                    Rect::new(
                        fields_area.x,
                        fields_area.y + row as u16,
                        fields_area.width,
                        1,
                    ),
                );
            }
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("HH:MM · append ±HH:MM when a clock time occurs twice"),
                    Line::from("End accepts YYYY-MM-DD HH:MM or 24:00 for midnight"),
                ])
                .style(theme.muted())
                .wrap(Wrap { trim: true }),
                instructions,
            );
        }
    }
}

fn draw_picker(
    frame: &mut Frame<'_>,
    area: Rect,
    picker: &PickerScreenModel,
    panel_title: Option<&str>,
    theme: Theme,
) {
    let [query_area, results_area] = Layout::vertical([
        Constraint::Length(u16::from(area.height > 1)),
        Constraint::Min(0),
    ])
    .areas(area);
    let mut query = Line::raw(picker.query.as_str());
    if query.width() > area.width.into() {
        query = query.right_aligned();
    }
    frame.render_widget(query, query_area);
    let block = match panel_title {
        Some(title) if results_area.height >= 3 && results_area.width >= 6 => theme.panel(title),
        _ => Block::default(),
    };
    if picker.labels.is_empty() {
        frame.render_widget(
            Paragraph::new(picker.empty)
                .style(theme.muted())
                .block(block),
            results_area,
        );
    } else {
        render_list(
            frame,
            results_area,
            picker
                .labels
                .iter()
                .map(|label| ListItem::new(label.as_str()))
                .collect(),
            picker.selected,
            block,
            theme,
        );
    }
}

fn render_view(frame: &mut Frame<'_>, area: Rect, model: &Content<'_>, theme: Theme) {
    let agenda = match model {
        Content::Month(month_model) => return month(frame, area, month_model, theme),
        Content::Week(week) => return schedule::week(frame, area, week, theme),
        Content::Day(day) => return schedule::day(frame, area, day, theme),
        Content::ViewError { title, message } => {
            frame.render_widget(
                Paragraph::new(message.as_str())
                    .style(Style::new().fg(theme.error))
                    .block(content_panel(theme.panel(*title), area))
                    .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        Content::Agenda(agenda) => agenda,
        Content::Outline(outline) => return draw_outline(frame, area, outline, theme),
        Content::Inbox(inbox) => {
            let items = inbox
                .rows
                .iter()
                .map(|row| {
                    ListItem::new(Line::from(vec![
                        Span::raw(row.title.as_str()),
                        Span::styled(row.metadata.as_str(), theme.muted()),
                    ]))
                })
                .collect();
            return list(frame, area, items, inbox.focused, "Inbox is empty", theme);
        }
        Content::Graph(graph) => return draw_graph(frame, area, graph, theme),
        Content::Timeline(timeline) => return draw_timeline(frame, area, timeline, theme),
        Content::Workflow(workflow_model) => {
            return workflow::draw(frame, area, workflow_model, theme);
        }
        _ => unreachable!("main view requires its projection"),
    };
    let items = agenda
        .labels
        .iter()
        .map(|label| ListItem::new(label.as_str()))
        .collect();
    list(
        frame,
        area,
        items,
        agenda.focused,
        "No entries for the selection",
        theme,
    );
}

fn draw_outline(frame: &mut Frame<'_>, area: Rect, model: &OutlineScreenModel, theme: Theme) {
    let mut block = theme.panel("");
    if model.rows.is_empty() {
        frame.render_widget(
            Paragraph::new("No projects in the vault").block(content_panel(block, area)),
            area,
        );
        return;
    }
    if model.has_blocked_work {
        block = block.title_bottom(Line::styled(" ! Blocked ", theme.warning()));
    }
    let items = model
        .rows
        .iter()
        .map(|row| {
            let style = if row.is_project {
                theme.title()
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(row.indent),
                Span::styled(if row.blocked { "! " } else { "" }, theme.warning()),
                Span::styled(row.title.as_str(), style),
            ]))
        })
        .collect();
    render_list(frame, area, items, model.focused, block, theme);
}

fn draw_timeline(frame: &mut Frame<'_>, area: Rect, model: &TimelineScreenModel, theme: Theme) {
    const LABEL_WIDTH: u16 = 20;
    const DAY_WIDTH: u16 = 2;
    let days = model.day_labels.len();
    let minimum_grid_width = LABEL_WIDTH + DAY_WIDTH * days as u16 + 3;
    if model.rows.is_empty() {
        list(
            frame,
            area,
            Vec::new(),
            0,
            "No planned entries in this window",
            theme,
        );
        return;
    }
    if area.width < minimum_grid_width || area.height < 5 {
        let items = model
            .rows
            .iter()
            .map(|row| ListItem::new(row.compact_label.as_str()))
            .collect();
        render_list(
            frame,
            area,
            items,
            model.focused_row,
            theme.panel(model.compact_title.as_str()),
            theme,
        );
        return;
    }
    let header = Row::new(std::iter::once(Cell::from("Plan")).chain(
        model.day_labels.iter().enumerate().map(|(column, label)| {
            let style = if column == model.day_column {
                theme.selected()
            } else {
                theme.muted()
            };
            Cell::from(label.as_str()).style(style)
        }),
    ));
    let rows = model.rows.iter().map(|row| {
        Row::new(std::iter::once(Cell::from(row.label.as_str())).chain(
            row.cells.iter().enumerate().map(|(column, cell)| {
                let style = if column == model.day_column {
                    theme.selected_cell()
                } else {
                    Style::default()
                };
                Cell::from(*cell).style(style)
            }),
        ))
    });
    let constraints = std::iter::once(Constraint::Min(LABEL_WIDTH))
        .chain(std::iter::repeat_n(Constraint::Length(DAY_WIDTH), days));
    let table = Table::new(rows, constraints)
        .column_spacing(0)
        .header(header)
        .row_highlight_style(theme.selected_cell())
        .highlight_symbol(">");
    let mut selected = TableState::default();
    selected.select(Some(model.focused_row));
    frame.render_stateful_widget(
        table.block(theme.panel(model.title.as_str()).title_bottom(
            " A active  E event  T task  B block  !! deadline  >> starts  == continues ",
        )),
        area,
        &mut selected,
    );
}

fn draw_graph(frame: &mut Frame<'_>, area: Rect, model: &GraphScreenModel<'_>, theme: Theme) {
    const MIN_DETAILS_HEIGHT: u16 = 4;
    let [list_area, details] = if area.width >= 92 && area.height >= MIN_DETAILS_HEIGHT {
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
    let visible_rows = usize::from(list_area.height.saturating_sub(2)).max(1);
    let rows: Vec<_> = model.visible_rows(visible_rows).collect();
    let Some(focus) = rows.iter().position(|row| row.focused) else {
        list(
            frame,
            area,
            Vec::new(),
            0,
            "No work units in the vault",
            theme,
        );
        return;
    };
    let items = rows
        .into_iter()
        .map(|row| ListItem::new(row.label))
        .collect();
    render_list(
        frame,
        list_area,
        items,
        focus,
        theme.panel(" Work units "),
        theme,
    );
    if let Some(capacity) = details.height.checked_sub(MIN_DETAILS_HEIGHT)
        && let Some(model) = model.details(usize::from(capacity))
    {
        let lines: Vec<_> = [model.prerequisites, model.dependents]
            .into_iter()
            .flat_map(|section| {
                std::iter::once(Line::styled(section.heading, theme.title()))
                    .chain(section.rows.into_iter().map(Line::from))
            })
            .collect();
        let block = theme.panel(model.title);
        frame.render_widget(Paragraph::new(lines).block(block), details);
    }
}

fn month(frame: &mut Frame<'_>, area: Rect, model: &MonthScreenModel, theme: Theme) {
    if area.width < 23 || usize::from(area.height) < model.weeks + 4 {
        let items = model
            .cells
            .iter()
            .flatten()
            .map(|cell| {
                let row = ListItem::new(cell.compact_label.as_str());
                if cell.in_selection && !cell.selected {
                    row.style(theme.selected_cell())
                } else {
                    row
                }
            })
            .collect();
        render_list(
            frame,
            area,
            items,
            model.focused_row,
            theme.panel(model.title.as_str()),
            theme,
        );
        return;
    }
    let compact = area.width < 58;
    let tall = !compact && usize::from(area.height) > 2 + 2 * model.weeks;
    let rows = model.cells.as_chunks::<7>().0.iter().map(|week| {
        let cells = week.iter().map(|cell| {
            let Some(cell) = cell else {
                return Cell::default();
            };
            let style = if cell.selected {
                theme.selected()
            } else if cell.in_selection {
                theme.selected_cell()
            } else {
                Style::default()
            };
            let mut lines = vec![Line::from(cell.grid_label.as_str())];
            if tall {
                lines.push(Line::from(cell.detail.as_str()));
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
        .header(Row::new(headings).style(theme.title()))
        .block(theme.panel(model.title.as_str()));
    frame.render_widget(table, grid);
    frame.render_widget(Paragraph::new("* Busy   ! Due   . Active / free"), legend);
}

fn list(
    frame: &mut Frame<'_>,
    area: Rect,
    items: Vec<ListItem<'_>>,
    row: usize,
    empty: &str,
    theme: Theme,
) {
    let block = theme.panel("");
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(empty).block(content_panel(block, area)),
            area,
        );
        return;
    }
    render_list(frame, area, items, row, block, theme);
}

fn render_list(
    frame: &mut Frame<'_>,
    area: Rect,
    items: Vec<ListItem<'_>>,
    row: usize,
    block: Block<'_>,
    theme: Theme,
) {
    let list = List::new(items)
        .block(content_panel(block, area))
        .highlight_symbol("› ")
        .highlight_style(theme.focus());
    let mut state = ListState::default().with_selected(Some(row));
    frame.render_stateful_widget(list, area, &mut state);
}

fn content_panel(block: Block<'_>, area: Rect) -> Block<'_> {
    if block.inner(area).is_empty() {
        Block::default()
    } else {
        block
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::Buffer,
        style::{Color, Modifier},
    };

    use super::*;
    use crate::tui::{Overlay, State, View, creation};

    fn render(content: Content<'_>, state: &State, size: (u16, u16), theme: Theme) -> Buffer {
        let model = ScreenModel {
            content,
            chrome: super::super::screen_model::ScreenChrome::project(state, chrono_tz::UTC),
        };
        let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
        terminal
            .draw(|frame| draw(frame, &model, theme))
            .unwrap()
            .buffer
            .clone()
    }

    #[test]
    fn empty_inbox_offers_only_applicable_actions() {
        use crate::tui::screen_model::InboxScreenModel;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Inbox;
        let theme = Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
        let screen = render(
            Content::Inbox(InboxScreenModel::project(&[], chrono_tz::UTC, 0)),
            &state,
            (24, 5),
            theme,
        );
        let line =
            |screen: &Buffer, y| -> String { (0..24).map(|x| screen[(x, y)].symbol()).collect() };
        assert!(line(&screen, 2).contains("Inbox is empty"));
        let footer = line(&screen, 4);
        assert!(footer.contains("menu") && footer.contains("help"));
        assert!(!footer.contains("assign"));
    }

    #[test]
    fn month_footer_chooses_a_complete_hint_for_wide_key_labels() {
        use crate::tui::keymap::Keymap;

        let day = "2026-11-25".parse().unwrap();
        let mut state = State::new(day);
        state.keymap = Keymap::from_test_config(
            "[keys]\nmove_left = '左'\nmove_right = '右'\nmove_up = '上'\nmove_down = '下'\nselect = '选'\nhelp = '问'\n",
        );
        let footer = |width| {
            let month = MonthScreenModel {
                title: " 2026-11 ".into(),
                weeks: 5,
                cells: vec![None; 35],
                focused_row: 0,
            };
            let buffer = render(
                Content::Month(month),
                &state,
                (width, 6),
                Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
            );
            (0..width)
                .map(|x| buffer[(x, 5)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        };
        let compact = footer(25);
        assert!(
            compact.contains("Space menu") && compact.contains('问') && compact.ends_with("help"),
            "{compact:?}"
        );
        let wider = footer(30);
        assert!(
            wider.contains("select") && wider.ends_with('问'),
            "{wider:?}"
        );
        let full = footer(80);
        assert!(
            full.contains("agenda") && full.ends_with("Space menu"),
            "{full:?}"
        );
    }

    #[test]
    fn outline_projects_hierarchy_and_empty_state_without_a_vault() {
        use mcbtodo_application::OutlineItem;
        use uuid::Uuid;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Outline;
        let theme = Theme::with_palette(mcbtodo_platform::TuiTheme::HighContrast);
        let row = |depth, title: &str, blocked| OutlineItem {
            id: Uuid::new_v4(),
            depth,
            title: title.into(),
            source_path: String::new(),
            project_id: None,
            task_id: None,
            blocked,
            revision: String::new(),
        };
        let model = OutlineScreenModel::project(
            &[
                row(0, "规划 Plan", false),
                row(1, "导入 Import", false),
                row(2, "校验 Review", true),
            ],
            2,
        );
        let screen = render(Content::Outline(model), &state, (72, 12), theme);
        assert_eq!(screen[(3, 3)].symbol(), "规");
        assert!(screen[(3, 3)].modifier.contains(Modifier::BOLD));
        assert_eq!(screen[(5, 4)].symbol(), "导");
        assert_eq!(screen[(1, 5)].symbol(), "›");
        assert_eq!(screen[(7, 5)].symbol(), "!");
        assert_eq!(screen[(9, 5)].symbol(), "校");
        assert_eq!(screen[(71, 5)].fg, theme.border);
        let empty = render(
            Content::Outline(OutlineScreenModel::project(&[], 0)),
            &state,
            (40, 6),
            theme,
        );
        let line: String = (1..39).map(|x| empty[(x, 2)].symbol()).collect();
        assert!(line.contains("No projects in the vault"), "{line}");
    }

    #[test]
    fn compact_workflow_footer_tracks_the_selected_runs_actions() {
        use crate::tui::screen_model::WorkflowScreenModel;
        use mcbtodo_protocol::workflow::{RunState, RunSummary};

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Workflow;
        state.workflow.loaded = true;
        state.workflow.runs.push(RunSummary {
            id: uuid::Uuid::nil(),
            state: RunState::Waiting,
            step_count: 0,
            current_node: Some("审核 Review".into()),
            revision: "revision-hash".into(),
            due_at: None,
            schedule_id: None,
            source_run_id: None,
            source_event_id: None,
            detail: None,
            resolution: None,
        });
        let screen = |state: &State, size: (u16, u16)| {
            let model =
                WorkflowScreenModel::project(&state.workflow, chrono_tz::UTC, &state.keymap);
            render(
                Content::Workflow(model),
                state,
                size,
                Theme::with_palette(mcbtodo_platform::TuiTheme::HighContrast),
            )
        };
        let footer = |buffer: &Buffer| {
            let y = buffer.area.bottom() - 1;
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim()
                .to_owned()
        };

        let buffer = screen(&state, (30, 6));
        assert_eq!(footer(&buffer), "a approve  d reject  ?");
        assert_eq!(buffer[(22, 2)].symbol(), "审");
        assert_eq!(buffer[(24, 2)].symbol(), "核");
        assert_eq!(buffer[(29, 2)].symbol(), "│");
        assert!(buffer[(22, 2)].modifier.contains(Modifier::BOLD));

        state.workflow.runs[0].state = RunState::OutcomeUnknown;
        assert_eq!(footer(&screen(&state, (30, 6))), "jk runs  r refresh  ?");
        assert!(!footer(&screen(&state, (100, 15))).contains("Enter"));
        state.workflow.runs[0].step_count = 1;
        assert_eq!(footer(&screen(&state, (30, 6))), "jk  Enter  r");
        state.workflow.runs.clear();
        assert_eq!(footer(&screen(&state, (30, 6))), "r refresh  ?");
    }

    #[test]
    fn monochrome_agenda_keeps_wide_text_and_focus_inside_narrow_borders() {
        use crate::tui::screen_model::AgendaScreenModel;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Agenda;
        let buffer = render(
            Content::Agenda(AgendaScreenModel {
                labels: vec!["中A文B标题很长，需要在边框前截断".into()],
                focused: 0,
            }),
            &state,
            (20, 7),
            Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
        );
        assert_eq!(buffer[(0, 3)].symbol(), "│");
        assert_eq!(buffer[(1, 3)].symbol(), "›");
        assert_eq!(buffer[(3, 3)].symbol(), "中");
        assert_eq!(buffer[(5, 3)].symbol(), "A");
        assert_eq!(buffer[(6, 3)].symbol(), "文");
        assert_eq!(buffer[(8, 3)].symbol(), "B");
        assert_eq!(buffer[(19, 3)].symbol(), "│");
        assert!(buffer[(3, 3)].modifier.contains(Modifier::BOLD));
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
        );
    }

    #[test]
    fn short_monochrome_timeline_keeps_mixed_width_title_and_focus_inside_panel() {
        use mcbtodo_application::CalendarItem;
        use mcbtodo_domain::{
            CalendarKind, Occurrence,
            time::{DateSpan, Span},
        };

        let day = "2026-11-25".parse().unwrap();
        let mut state = State::new(day);
        state.view = View::Timeline;
        let model = TimelineScreenModel::project(
            vec![CalendarItem {
                id: uuid::Uuid::nil(),
                title: "中A文B计划排期需要在边框前截断".into(),
                kind: CalendarKind::Event,
                busy: true,
                occurrence: Occurrence::Span(Span::DateSpan(DateSpan::new(day, day).unwrap())),
            }],
            day,
            chrono_tz::UTC,
            0,
        );
        let buffer = render(
            Content::Timeline(model),
            &state,
            (30, 6),
            Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
        );
        let row = |y| (0..30).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(1).contains("Plan · 2026-11-25"));
        assert!(row(2).starts_with("│› ██  E "), "{}", row(2));
        assert_eq!(buffer[(9, 2)].symbol(), "中");
        assert_eq!(buffer[(11, 2)].symbol(), "A");
        assert_eq!(buffer[(12, 2)].symbol(), "文");
        assert_eq!(buffer[(14, 2)].symbol(), "B");
        assert_eq!(buffer[(29, 2)].symbol(), "│");
        assert!(!row(2).contains("截断"));
        assert!(buffer[(9, 2)].modifier.contains(Modifier::BOLD));
        assert!(row(5).contains("hl date  jk plan  ?"));
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
        );
    }

    #[test]
    fn high_contrast_help_keeps_border_footer_and_warning_separate() {
        let mut state = State::new("2026-11-25".parse().unwrap());
        state.overlay = Overlay::Help(Default::default());
        state.vault_warning = Some("Reload failed · invalid Markdown".into());
        let theme = Theme::with_palette(mcbtodo_platform::TuiTheme::HighContrast);
        let Overlay::Help(viewport) = &state.overlay else {
            unreachable!();
        };
        let content = Content::Help {
            viewport,
            text: "Use arrows to browse keyboard shortcuts".into(),
        };
        let buffer = render(content, &state, (48, 10), theme);
        assert_eq!(buffer[(0, 2)].fg, Color::White);
        assert_eq!(buffer[(0, 9)].fg, Color::Yellow);
        assert!(buffer[(0, 9)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(0, 9)].symbol(), "R");
        assert_eq!(buffer[(1, 8)].symbol(), "E");
    }

    #[test]
    fn short_terminal_prioritizes_selection_and_content() {
        use crate::tui::screen_model::AgendaScreenModel;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Agenda;
        state.anchor = Some("2026-11-24".parse().unwrap());
        let buffer = render(
            Content::Agenda(AgendaScreenModel {
                labels: vec!["中文 Task".into()],
                focused: 0,
            }),
            &state,
            (27, 5),
            Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
        );
        let row = |y| (0..27).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).starts_with("Agenda  2026-11-25"));
        assert!(row(0).ends_with("SELECT"));
        assert_eq!(buffer[(3, 2)].symbol(), "中");
        assert_eq!(buffer[(5, 2)].symbol(), "文");
        assert!(row(2).contains("Task"));
        assert!(row(4).starts_with(" jk entry  g Graph  ?"));
        assert_eq!(buffer[(26, 2)].symbol(), "│");
    }

    #[test]
    fn short_agenda_keeps_focus_with_feedback_and_a_vault_warning() {
        use crate::tui::screen_model::AgendaScreenModel;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Agenda;
        state.anchor = Some("2026-11-24".parse().unwrap());
        state.message = "Nothing to open".into();
        state.vault_warning = Some("Reload failed · invalid Markdown".into());
        let buffer = render(
            Content::Agenda(AgendaScreenModel {
                labels: vec!["Another entry".into(), "中文 Task".into()],
                focused: 1,
            }),
            &state,
            (24, 5),
            Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
        );

        let row = |y| (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).ends_with("SELECT"));
        assert_eq!(buffer[(0, 1)].symbol(), "›");
        assert_eq!(buffer[(2, 1)].symbol(), "中");
        assert_eq!(buffer[(4, 1)].symbol(), "文");
        assert!(buffer[(2, 1)].modifier.contains(Modifier::BOLD));
        assert!(row(1).contains("Task"));
        assert_eq!(row(2).trim(), "Nothing to open");
        assert!(row(3).contains("jk entry"));
        assert!(row(4).starts_with("Reload failed"));
    }

    #[test]
    fn four_row_agenda_keeps_the_focused_entry_and_actions_with_both_status_lines() {
        use crate::tui::screen_model::AgendaScreenModel;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Agenda;
        state.message = "Nothing to open".into();
        state.vault_warning = Some("Reload failed · invalid Markdown".into());
        let content = || {
            Content::Agenda(AgendaScreenModel {
                labels: vec!["Another entry".into(), "中文 Task".into()],
                focused: 1,
            })
        };
        let theme = Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
        let buffer = render(content(), &state, (24, 4), theme);

        let row = |y| (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert_eq!(buffer[(0, 0)].symbol(), "›");
        assert_eq!(buffer[(2, 0)].symbol(), "中");
        assert_eq!(buffer[(4, 0)].symbol(), "文");
        assert!(row(0).contains("Task"));
        assert_eq!(row(1).trim(), "Nothing to open");
        assert!(row(2).contains("jk entry"));
        assert!(row(3).starts_with("Reload failed"));

        let buffer = render(content(), &state, (24, 3), theme);
        let row = |y| (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).contains("Task"));
        assert!(row(1).contains("jk entry"));
        assert!(row(2).starts_with("Reload failed"));

        let buffer = render(content(), &state, (24, 2), theme);
        let row = |y| (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).contains("Task"));
        assert!(row(1).starts_with("Reload failed"));
    }

    #[test]
    fn short_workflow_keeps_empty_and_unavailable_states_visible() {
        use crate::tui::screen_model::WorkflowScreenModel;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Workflow;
        state.message = "Checking runs".into();
        state.vault_warning = Some("Reload failed · invalid Markdown".into());
        let theme = Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
        for (loaded, label) in [(true, "No workflow runs"), (false, "Run list unavailable")] {
            state.workflow.loaded = loaded;
            let model =
                WorkflowScreenModel::project(&state.workflow, chrono_tz::UTC, &state.keymap);
            let buffer = render(Content::Workflow(model), &state, (24, 5), theme);
            let row = |y| (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            assert!(row(1).starts_with(label), "{}", row(1));
            assert_eq!(row(2).trim(), "Checking runs");
            assert!(row(3).contains("r refresh"));
            assert!(row(4).starts_with("Reload failed"));
        }
    }

    #[test]
    fn compact_timer_menu_keeps_return_and_available_commands_visible() {
        use crate::tui::screen_model::AgendaScreenModel;

        let mut state = State::new("2026-11-25".parse().unwrap());
        state.view = View::Agenda;
        state.overlay = Overlay::Time;
        let content = || {
            Content::Agenda(AgendaScreenModel {
                labels: vec!["中文 Task".into()],
                focused: 0,
            })
        };
        let theme = Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome);
        let buffer = render(content(), &state, (24, 5), theme);
        let footer = (0..24).map(|x| buffer[(x, 4)].symbol()).collect::<String>();
        assert_eq!(footer.trim(), "Esc  b block  i/s/p/x");
        assert_eq!(buffer[(3, 2)].symbol(), "中");
        assert_eq!(buffer[(23, 2)].symbol(), "│");

        state.view = View::Outline;
        let buffer = render(
            Content::Outline(OutlineScreenModel::project(&[], 0)),
            &state,
            (24, 5),
            theme,
        );
        let footer = (0..24).map(|x| buffer[(x, 4)].symbol()).collect::<String>();
        assert_eq!(footer.trim(), "Esc  i/s/p/x timer");
    }

    #[test]
    fn short_creation_form_keeps_the_current_field_and_save_actions_visible() {
        let day = "2026-11-25".parse().unwrap();
        let mut state = State::new(day);
        let mut form = creation::Form::new(creation::Kind::Project, day, (day, day));
        form.fields[0].value = "混合 Rust 标题".repeat(15);
        form.selected = 2;
        state.overlay = Overlay::CreateForm(form);
        state.message = "Review the date".into();
        let Overlay::CreateForm(form) = &state.overlay else {
            unreachable!();
        };
        let buffer = render(
            Content::CreateForm(CreationScreenModel::from_form(form)),
            &state,
            (28, 6),
            Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
        );
        let row = |y| (0..28).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).ends_with("INSERT"));
        assert!(
            row(2).contains("> Through  2026-11-25▏"),
            "focused field and default should fit: {}",
            row(2)
        );
        assert_eq!(buffer[(0, 2)].symbol(), "│");
        assert_eq!(buffer[(27, 2)].symbol(), "│");
        assert!(buffer[(1, 2)].modifier.contains(Modifier::BOLD));
        assert!(row(4).starts_with("Review the date"));
        assert!(row(5).contains("Enter save"));
        assert!(row(5).contains("Esc"));
    }

    #[test]
    fn narrow_creation_form_clips_before_a_grapheme_and_keeps_the_cursor() {
        let day = "2026-11-25".parse().unwrap();
        let mut state = State::new(day);
        let mut form = creation::Form::new(creation::Kind::Project, day, (day, day));
        form.fields[0].value = format!("{}计划 Rust", "前缀".repeat(15));
        state.overlay = Overlay::CreateForm(form);
        let Overlay::CreateForm(form) = &state.overlay else {
            unreachable!();
        };
        let buffer = render(
            Content::CreateForm(CreationScreenModel::from_form(form)),
            &state,
            (28, 10),
            Theme::with_palette(mcbtodo_platform::TuiTheme::HighContrast),
        );
        let row = |y| (0..28).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(3).starts_with("│> Title  "));
        assert_eq!(buffer[(17, 3)].symbol(), "计");
        assert_eq!(buffer[(19, 3)].symbol(), "划");
        assert!(row(3).ends_with(" Rust▏│"));
        assert!(row(4).contains("2026-11-25"));
        assert!(row(5).contains("2026-11-25"));
    }

    #[test]
    fn monochrome_time_block_keeps_the_end_field_and_feedback_in_four_rows() {
        use crate::tui::time_block::{Choice, Field, Form};

        let day = "2026-11-25".parse().unwrap();
        let mut state = State::new(day);
        let mut form = Form::new(day);
        let unit = mcbtodo_domain::Id::from_u128(1);
        form.unit = Some(unit);
        form.start = "13:30".into();
        form.end = "12:30".into();
        form.field = Field::End;
        state.overlay = Overlay::TimeBlock(form);
        state.message = "End must follow start".into();
        let Overlay::TimeBlock(form) = &state.overlay else {
            unreachable!();
        };
        let buffer = render(
            Content::TimeBlock(TimeBlockScreenModel::from_form(
                form,
                vec![Choice {
                    id: unit,
                    title: "计划 Rust".into(),
                    project: "Project".into(),
                    task: "Task".into(),
                }],
                chrono_tz::UTC,
            )),
            &state,
            (28, 4),
            Theme::with_palette(mcbtodo_platform::TuiTheme::Monochrome),
        );
        let row = |y| (0..28).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).ends_with("INSERT"));
        assert!(row(1).starts_with("> End   12:30▏"), "{}", row(1));
        assert!(buffer[(0, 1)].modifier.contains(Modifier::BOLD));
        assert!(row(2).starts_with("End must follow start"));
        assert!(row(3).contains("Enter save") && row(3).contains("Esc"));
    }
}
