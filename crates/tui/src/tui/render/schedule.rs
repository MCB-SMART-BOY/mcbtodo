use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    widgets::{Cell, ListItem, Paragraph, Row, Table},
};

use super::{list, render_list};
use crate::tui::{
    screen_model::{DayRow, DayScreenModel, WeekRow, WeekScreenModel},
    theme::Theme,
};

pub(super) fn day(frame: &mut Frame<'_>, area: Rect, model: &DayScreenModel, theme: Theme) {
    let items = model
        .rows
        .iter()
        .map(|row| match row {
            DayRow::Section(title) => ListItem::new(*title).style(theme.accent()),
            DayRow::Entry(label) => ListItem::new(label.clone()),
        })
        .collect();
    list(
        frame,
        area,
        items,
        model.focused,
        "No entries for this day",
        theme,
    );
}

pub(super) fn week(frame: &mut Frame<'_>, area: Rect, week: &WeekScreenModel, theme: Theme) {
    if area.width < 72 || area.height < 8 {
        let focus = week.compact_focus.as_deref();
        let [days, preview] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(u16::from(area.height >= 4 && focus.is_some())),
        ])
        .areas(area);
        let items = week
            .compact_rows
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let label = if preview.is_empty() && index == week.selected_day {
                    focus.unwrap_or(label)
                } else {
                    label
                };
                ListItem::new(label)
            })
            .collect();
        render_list(
            frame,
            days,
            items,
            week.selected_day,
            theme.panel(week.compact_title.as_str()),
            theme,
        );
        if let Some(focus) = focus.filter(|_| !preview.is_empty()) {
            frame.render_widget(Paragraph::new(focus), preview);
        }
        return;
    }

    let header = Row::new(
        std::iter::once(Cell::from("Local")).chain(week.headers.iter().map(|column| {
            let style = if column.selected {
                theme.selected()
            } else if column.in_selection {
                theme.selected_cell()
            } else {
                theme.title()
            };
            Cell::from(column.label.as_str()).style(style)
        })),
    );
    let [grid, preview] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let visible = usize::from(grid.height.saturating_sub(6));
    let offset = (week.selected_hour + 1).saturating_sub(visible);
    let rows = week
        .fixed_rows
        .iter()
        .map(|row| week_row(row, theme.title(), None, theme))
        .chain(
            week.timed_rows
                .iter()
                .enumerate()
                .skip(offset)
                .take(visible)
                .map(|(index, row)| {
                    let active = (index == week.selected_hour).then_some(week.selected_day);
                    week_row(row, Style::default(), active, theme)
                }),
        );
    let table = Table::new(
        rows,
        std::iter::once(Constraint::Length(8)).chain([Constraint::Fill(1); 7]),
    )
    .column_spacing(1)
    .header(header)
    .block(
        theme
            .panel(week.title.as_str())
            .title_bottom(week.legend.as_str()),
    );
    frame.render_widget(table, grid);
    if let Some(label) = &week.preview {
        frame.render_widget(Paragraph::new(label.as_str()), preview);
    }
}

fn week_row(row: &WeekRow, label_style: Style, active: Option<usize>, theme: Theme) -> Row<'_> {
    let label = Cell::from(row.label.as_str()).style(label_style);
    Row::new(
        std::iter::once(label).chain(row.cells.iter().enumerate().map(|(index, label)| {
            let cell = Cell::from(label.as_str());
            if active == Some(index) {
                cell.style(theme.selected_cell())
            } else {
                cell
            }
        })),
    )
}
