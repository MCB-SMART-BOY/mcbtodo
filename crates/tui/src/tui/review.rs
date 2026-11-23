use std::{cell::Cell, ops::Range};

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    buffer::CellWidth,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::theme::Theme;

#[derive(Default)]
pub(super) struct ReviewViewport(Cell<ScrollBounds>);

#[derive(Default)]
pub(super) struct PannableReview {
    wrapped: ReviewViewport,
    unwrapped: Option<(u16, u16)>,
}

impl PannableReview {
    pub(super) fn toggle_wrap(&mut self) {
        if let Some((row, _)) = self.unwrapped.take() {
            let mut bounds = self.wrapped.0.get();
            if usize::from(row) != bounds.first_source.line {
                bounds.resume_source_row = Some(usize::from(row));
                self.wrapped.0.set(bounds);
            }
        } else {
            self.unwrapped = Some(self.start_pan());
        }
    }

    pub(super) fn pan_left(&mut self, columns: u16) {
        if let Some((_, column)) = self.unwrapped.as_mut() {
            *column = column.saturating_sub(columns);
        }
    }

    pub(super) fn pan_right(&mut self, columns: u16, text: &str) {
        let start = self.start_pan();
        let (_, column) = self.unwrapped.get_or_insert(start);
        if let Some(width) = text.lines().map(|line| Line::raw(line).width()).max() {
            let limit = u16::try_from(width.saturating_sub(1)).unwrap_or(u16::MAX);
            *column = column.saturating_add(columns).min(limit);
        }
    }

    fn start_pan(&self) -> (u16, u16) {
        let row = self.wrapped.0.get().first_source.line;
        (u16::try_from(row).unwrap_or(u16::MAX), 0)
    }

    pub(super) fn scroll(&mut self, key: KeyCode, text: &str) {
        let Some((row, column)) = self.unwrapped.as_mut() else {
            self.wrapped.scroll(key);
            return;
        };
        let last_row = || u16::try_from(text.lines().count().saturating_sub(1)).unwrap_or(u16::MAX);
        match key {
            KeyCode::Char('k') | KeyCode::Up => *row = row.saturating_sub(1),
            KeyCode::Char('j') | KeyCode::Down => *row = row.saturating_add(1).min(last_row()),
            KeyCode::PageUp => *row = row.saturating_sub(10),
            KeyCode::PageDown => *row = row.saturating_add(10).min(last_row()),
            KeyCode::Home => (*row, *column) = (0, 0),
            KeyCode::End => *row = last_row(),
            _ => {}
        }
    }

    pub(super) fn draw(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        text: &str,
        theme: Theme,
    ) {
        if let Some(scroll) = self.unwrapped {
            let mut paragraph = Paragraph::new(text).scroll(scroll);
            if area.width >= 5 && area.height >= 3 {
                paragraph = paragraph.block(theme.panel(title));
            }
            frame.render_widget(paragraph, area);
        } else {
            self.wrapped.draw(frame, area, title, text, theme);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ScrollBounds {
    offset: usize,
    height: usize,
    width: u16,
    last_offset: usize,
    first_source: SourcePosition,
    reflow_anchor: Option<SourcePosition>,
    resume_source_row: Option<usize>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct SourcePosition {
    line: usize,
    grapheme: usize,
}

impl ReviewViewport {
    fn visible_rows(&self, rows: &[ReviewRow], width: u16, height: u16) -> Range<usize> {
        let total = rows.len();
        let height = usize::from(height);
        let last_offset = total.saturating_sub(height);
        let previous = self.0.get();
        let resume = previous
            .resume_source_row
            .map(|line| SourcePosition { line, grapheme: 0 });
        let anchor = resume.or(previous.reflow_anchor);
        let resized = width != previous.width || height != previous.height;
        let offset = anchor
            .filter(|_| resume.is_some() || resized)
            .map_or(previous.offset, |position| {
                let index = rows.partition_point(|row| row.start < position);
                if rows.get(index).is_some_and(|row| row.start == position) {
                    index
                } else {
                    index.saturating_sub(1)
                }
            })
            .min(last_offset);
        let first_source = rows.get(offset).map(|row| row.start);
        self.0.set(ScrollBounds {
            offset,
            height,
            width,
            last_offset,
            first_source: first_source.unwrap_or_default(),
            reflow_anchor: anchor.or(first_source),
            resume_source_row: None,
        });
        offset..(offset + height).min(total)
    }

    pub(super) fn scroll(&self, key: KeyCode) {
        let mut bounds = self.0.get();
        let ScrollBounds {
            offset,
            height,
            last_offset,
            ..
        } = bounds;
        bounds.offset = match key {
            KeyCode::Char('j') | KeyCode::Down => offset.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => offset.saturating_sub(1),
            KeyCode::PageDown => offset.saturating_add(height),
            KeyCode::PageUp => offset.saturating_sub(height),
            KeyCode::Home => 0,
            KeyCode::End => last_offset,
            _ => return,
        }
        .min(last_offset);
        // Explicit navigation replaces the retained position from a prior resize.
        bounds.reflow_anchor = None;
        self.0.set(bounds);
    }

    pub(super) fn draw(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        text: &str,
        theme: Theme,
    ) {
        let content = if area.width >= 5 && area.height >= 3 {
            let panel = theme.panel(title);
            let content = panel.inner(area);
            frame.render_widget(panel, area);
            content
        } else {
            area
        };
        if content.is_empty() {
            return;
        }
        let rows = review_rows(text, usize::from(content.width));
        let viewport = self.visible_rows(&rows, content.width, content.height);
        let lines = rows[viewport]
            .iter()
            .map(|row| Line::raw(row.text.as_str()))
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), content);
    }
}

struct ReviewRow {
    start: SourcePosition,
    text: String,
}

fn review_rows(text: &str, width: usize) -> Vec<ReviewRow> {
    // Wrap at grapheme boundaries so exact values retain spaces and long tokens fit.
    let mut rows = Vec::new();
    for (source_line, line) in text.split('\n').enumerate() {
        let mut start = SourcePosition {
            line: source_line,
            grapheme: 0,
        };
        let mut row = String::new();
        let mut used = 0;
        let mut push = |symbol: &str, columns: usize, grapheme: usize| {
            if used + columns > width {
                rows.push(ReviewRow {
                    start,
                    text: std::mem::take(&mut row),
                });
                start.grapheme = grapheme;
                used = 0;
            }
            row.push_str(symbol);
            used += columns;
        };
        let span = Span::raw(line);
        for (index, grapheme) in span.styled_graphemes(Style::default()).enumerate() {
            let columns = usize::from(grapheme.symbol.cell_width());
            if columns == 0 || columns > width {
                for ch in grapheme.symbol.chars().flat_map(char::escape_unicode) {
                    push(ch.encode_utf8(&mut [0; 4]), 1, index);
                }
            } else {
                push(grapheme.symbol, columns, index);
            }
        }
        rows.push(ReviewRow { start, text: row });
    }
    rows
}

pub(super) fn append_review_line(visible: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '\n' => visible.push_str(r"\n"),
            '\r' => visible.push_str(r"\r"),
            '\t' => visible.push_str(r"\t"),
            '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => visible.extend(ch.escape_unicode()),
            ch if ch.is_control() => visible.extend(ch.escape_unicode()),
            ch => visible.push(ch),
        }
    }
}
