use chrono::NaiveDate;
use mcbtodo_vault::Vault;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Cell, ListItem, Paragraph, Row, Table},
};

use super::list;
use crate::tui::{
    State,
    calendar::{self, Lane, Week},
};

pub(super) fn day(frame: &mut Frame<'_>, area: Rect, state: &State, vault: &Vault) {
    let entries = calendar::day(vault, state.day);
    let mut items = Vec::new();
    let mut previous_lane = None;
    let mut selected = 0;
    for (index, entry) in entries.iter().enumerate() {
        let lane = calendar::lane(entry);
        if previous_lane != Some(lane) {
            let title = match lane {
                Lane::AllDay => "All-day",
                Lane::Timed => "Timed",
                Lane::Deadline => "Deadlines",
                Lane::Ongoing => "Ongoing",
            };
            items.push(ListItem::new(title).style(Style::new().fg(Color::Cyan).bold()));
            previous_lane = Some(lane);
        }
        if index == state.row {
            selected = items.len();
        }
        items.push(ListItem::new(calendar::day_label(
            entry,
            state.day,
            vault.timezone(),
        )));
    }
    list(frame, area, items, selected, "No entries for this day");
}

pub(super) fn week(frame: &mut Frame<'_>, area: Rect, state: &State, vault: &Vault) {
    let week = Week::new(vault, state.day);
    let columns: [_; 7] = std::array::from_fn(|index| week.column(index));
    let (start, end) = state.selected_range();
    let date_label = |date: NaiveDate| {
        let marker = if date == state.day {
            '>'
        } else if (start..=end).contains(&date) {
            '+'
        } else {
            ' '
        };
        format!("{marker}{}", date.format("%a %d"))
    };
    if area.width < 72 || area.height < 8 {
        let items = columns
            .iter()
            .map(|column| {
                let counts = [
                    (Lane::AllDay, "all"),
                    (Lane::Timed, "timed"),
                    (Lane::Deadline, "due"),
                    (Lane::Ongoing, "active"),
                ]
                .into_iter()
                .filter_map(|(lane, label)| {
                    let count = column.in_lane(lane).count();
                    (count > 0).then(|| format!("{count} {label}"))
                })
                .collect::<Vec<_>>()
                .join(" · ");
                ListItem::new(format!("{}  {counts}", date_label(column.date)))
            })
            .collect();
        list(
            frame,
            area,
            items,
            (state.day - week.monday).num_days() as usize,
            "No entries for this week",
        );
        return;
    }

    let header = Row::new(
        std::iter::once(Cell::from("Local")).chain(columns.iter().map(|column| {
            let style = if column.date == state.day {
                Style::new().fg(Color::Black).bg(Color::Cyan).bold()
            } else if (start..=end).contains(&column.date) {
                Style::new()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::new().bold()
            };
            Cell::from(date_label(column.date)).style(style)
        })),
    );
    let title_cell = |title| Cell::from(title).style(Style::new().bold());
    let mut rows: Vec<_> = [
        (Lane::AllDay, "All-day"),
        (Lane::Deadline, "Due"),
        (Lane::Ongoing, "Ongoing"),
    ]
    .into_iter()
    .map(|(lane, label)| {
        Row::new(
            std::iter::once(title_cell(label)).chain(columns.iter().map(|column| {
                let mut entries = column.in_lane(lane);
                let label = entries.next().map(|entry| {
                    let mark = match lane {
                        Lane::AllDay if entry.busy => '*',
                        Lane::AllDay => '·',
                        Lane::Deadline => '!',
                        _ => '·',
                    };
                    summarize(format!("{mark}{}", entry.title), entries.count())
                });
                Cell::from(label.unwrap_or_default())
            })),
        )
    })
    .collect();

    let [grid, preview] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let visible = usize::from(grid.height.saturating_sub(6));
    let selected = state.week_hour.min(week.hours.len().saturating_sub(1));
    let offset = (selected + 1).saturating_sub(visible);
    let zone = vault.timezone();
    for (index, &hour) in week.hours.iter().enumerate().skip(offset).take(visible) {
        let pointer = if index == selected { '>' } else { ' ' };
        rows.push(Row::new(
            std::iter::once(Cell::from(format!("{pointer} {hour:02}:00"))).chain(
                columns.iter().map(|column| {
                    let mut entries = column.at_hour(hour, zone);
                    let label = entries.next().map(|entry| {
                        let time = calendar::grid_time(entry, column.date, zone);
                        let mark = if entry.busy { '*' } else { '·' };
                        summarize(format!("{mark}{time} {}", entry.title), entries.count())
                    });
                    let style = if index == selected && column.date == state.day {
                        Style::new()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    Cell::from(label.unwrap_or_default()).style(style)
                }),
            ),
        ));
    }
    let timed = if week.hours.is_empty() {
        "No timed entries".into()
    } else {
        format!("Timed hours {}/{}", selected + 1, week.hours.len())
    };
    let table = Table::new(
        rows,
        std::iter::once(Constraint::Length(8)).chain([Constraint::Fill(1); 7]),
    )
    .column_spacing(1)
    .header(header)
    .block(
        Block::bordered()
            .title(format!(
                " {} → {} ",
                week.monday.format("%Y-%m-%d"),
                week.days[6].format("%Y-%m-%d")
            ))
            .title_bottom(format!(" {timed}  * Busy  · Free  ! Due ")),
    );
    frame.render_widget(table, grid);
    if let Some(entry) = week.focused_timed(state.day, selected, zone) {
        frame.render_widget(
            Paragraph::new(calendar::day_label(entry, state.day, zone)),
            preview,
        );
    }
}

fn summarize(first: String, additional: usize) -> String {
    if additional == 0 {
        first
    } else {
        format!("+{additional} {first}")
    }
}
