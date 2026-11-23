use mcbtodo_platform::TuiTheme;
use ratatui::{
    style::{Color, Modifier, Style},
    text::Line,
    widgets::Block,
};

/// Shared visual tokens for every TUI renderer.
///
/// Palettes use named terminal colors or the terminal's default colors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Theme {
    pub(super) background: Color,
    pub(super) surface: Color,
    pub(super) surface_active: Color,
    pub(super) border: Color,
    pub(super) border_focus: Color,
    pub(super) text_primary: Color,
    pub(super) text_secondary: Color,
    pub(super) text_muted: Color,
    pub(super) accent: Color,
    pub(super) success: Color,
    pub(super) warning: Color,
    pub(super) error: Color,
    pub(super) selection: Color,
    pub(super) disabled: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::Reset,
            surface: Color::Reset,
            surface_active: Color::DarkGray,
            border: Color::DarkGray,
            border_focus: Color::Cyan,
            text_primary: Color::Reset,
            text_secondary: Color::White,
            text_muted: Color::Gray,
            accent: Color::Cyan,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            selection: Color::Cyan,
            disabled: Color::DarkGray,
        }
    }
}

impl Theme {
    pub(super) fn for_session(preference: TuiTheme, no_color: bool) -> Self {
        Self::with_palette(if no_color {
            TuiTheme::Monochrome
        } else {
            preference
        })
    }

    pub(super) fn with_palette(preference: TuiTheme) -> Self {
        match preference {
            TuiTheme::Default => Self::default(),
            TuiTheme::HighContrast => Self {
                border: Color::White,
                border_focus: Color::LightCyan,
                text_secondary: Color::Reset,
                text_muted: Color::Reset,
                disabled: Color::Gray,
                ..Self::default()
            },
            TuiTheme::Monochrome => Self {
                background: Color::Reset,
                surface: Color::Reset,
                surface_active: Color::Reset,
                border: Color::Reset,
                border_focus: Color::Reset,
                text_primary: Color::Reset,
                text_secondary: Color::Reset,
                text_muted: Color::Reset,
                accent: Color::Reset,
                success: Color::Reset,
                warning: Color::Reset,
                error: Color::Reset,
                selection: Color::Reset,
                disabled: Color::Reset,
            },
        }
    }

    pub(super) fn panel<'a>(self, title: impl Into<Line<'a>>) -> Block<'a> {
        Block::bordered()
            .border_style(Style::new().fg(self.border))
            .title(title)
            .title_style(self.title())
    }

    pub(super) fn title(self) -> Style {
        Style::new().fg(self.text_primary).bold()
    }

    pub(super) fn accent(self) -> Style {
        Style::new().fg(self.accent).bold()
    }

    pub(super) fn secondary(self) -> Style {
        Style::new().fg(self.text_secondary)
    }

    pub(super) fn muted(self) -> Style {
        let style = Style::new().fg(self.text_muted);
        if self.text_muted == Color::Reset {
            style
        } else {
            style.add_modifier(Modifier::DIM)
        }
    }

    pub(super) fn selected(self) -> Style {
        if self.selection == Color::Reset {
            return self.selected_cell().bold();
        }
        Style::new()
            .fg(Color::Black)
            .bg(self.selection)
            .add_modifier(Modifier::BOLD)
    }

    pub(super) fn selected_cell(self) -> Style {
        Style::new()
            .fg(self.selection)
            .add_modifier(Modifier::REVERSED)
    }

    pub(super) fn focus(self) -> Style {
        Style::new().fg(self.border_focus).bold()
    }

    pub(super) fn warning(self) -> Style {
        Style::new().fg(self.warning).bold()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_distinct_semantic_tokens() {
        let theme = Theme::default();

        assert_ne!(theme.accent, theme.warning);
        assert_ne!(theme.warning, theme.error);
        assert_ne!(theme.border, theme.border_focus);
        assert_eq!(theme.selected().fg, Some(Color::Black));
        assert_eq!(theme.selected().bg, Some(theme.selection));
    }

    #[test]
    fn session_palette_respects_no_color() {
        for palette in [
            TuiTheme::Default,
            TuiTheme::HighContrast,
            TuiTheme::Monochrome,
        ] {
            assert_eq!(
                Theme::for_session(palette, false),
                Theme::with_palette(palette)
            );
            assert_eq!(
                Theme::for_session(palette, true),
                Theme::with_palette(TuiTheme::Monochrome)
            );
        }
    }
}
