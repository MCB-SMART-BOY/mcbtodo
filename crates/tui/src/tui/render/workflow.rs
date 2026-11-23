use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{ListItem, Paragraph, Wrap},
};

use crate::tui::{
    screen_model::{WorkflowConfirmation, WorkflowPanel, WorkflowScreenModel},
    theme::Theme,
};

pub(super) fn draw(
    frame: &mut Frame<'_>,
    area: Rect,
    model: &WorkflowScreenModel<'_>,
    theme: Theme,
) {
    match &model.panel {
        WorkflowPanel::Confirm(decision) => draw_confirmation(frame, area, decision, theme),
        WorkflowPanel::Empty(label) => {
            frame.render_widget(
                Paragraph::new(label.as_str())
                    .block(super::content_panel(theme.panel(" Runs "), area)),
                area,
            );
        }
        WorkflowPanel::Trace {
            title,
            text,
            viewport,
        } => viewport.draw(frame, area, title, text, theme),
        WorkflowPanel::Runs {
            title,
            rows,
            selected,
            details,
        } => {
            let [list_area, detail_area] = if area.width >= 96 && area.height >= 6 {
                Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
                    .areas(area)
            } else if area.height >= 9 {
                Layout::vertical([Constraint::Min(3), Constraint::Length(6)]).areas(area)
            } else {
                [area, Rect::ZERO]
            };
            let items = rows
                .iter()
                .map(|text| ListItem::new(text.as_str()))
                .collect();
            super::render_list(
                frame,
                list_area,
                items,
                *selected,
                theme.panel(*title),
                theme,
            );
            let lines: Vec<_> = details
                .iter()
                .map(|text| Line::raw(text.as_str()))
                .collect();
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(theme.panel(" Selected run ")),
                detail_area,
            );
        }
    }
}

fn draw_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    confirmation: &WorkflowConfirmation<'_>,
    theme: Theme,
) {
    let [body, footer] = if area.height >= 2 {
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area)
    } else {
        [area, Rect::ZERO]
    };
    confirmation
        .viewport
        .draw(frame, body, confirmation.title, &confirmation.text, theme);
    frame.render_widget(
        Paragraph::new(confirmation.hint(footer.width)).style(theme.accent()),
        footer,
    );
}
