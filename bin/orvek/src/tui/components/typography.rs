//! Shared typography and input treatment for TUI display components.

use crate::tui::theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const SEARCH_PREFIX: &str = "  Search: ";
const SEARCH_CURSOR: &str = "▏";
const SEARCH_PLACEHOLDER: &str = " type to filter";
pub(super) const CHOICE_MARKER: &str = "› ";

pub(super) fn body(theme: &Theme) -> Style {
    Style::default().fg(theme.text())
}

pub(super) fn secondary(theme: &Theme) -> Style {
    Style::default().fg(theme.muted())
}

pub(super) fn accent(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD)
}

pub(super) fn key(theme: &Theme) -> Style {
    accent(theme)
}

#[derive(Clone, Copy)]
pub(super) struct ChoiceStyle {
    selected: bool,
    enabled: bool,
}

impl ChoiceStyle {
    pub(super) const fn new(selected: bool, enabled: bool) -> Self {
        Self { selected, enabled }
    }

    pub(super) fn primary(self, theme: &Theme) -> Style {
        if !self.enabled {
            secondary(theme)
        } else if self.selected {
            accent(theme)
        } else {
            body(theme)
        }
    }

    pub(super) fn detail(self, theme: &Theme) -> Style {
        if self.enabled && self.selected {
            body(theme)
        } else {
            secondary(theme)
        }
    }

    pub(super) fn highlight(self, theme: &Theme) -> Style {
        let style = Style::default().bg(theme.code_background());
        if self.enabled {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }
}

pub(super) struct SearchField<'a> {
    query: &'a str,
    prefix: &'a str,
}

impl<'a> SearchField<'a> {
    pub(super) const fn new(query: &'a str) -> Self {
        Self {
            query,
            prefix: SEARCH_PREFIX,
        }
    }

    pub(super) const fn with_prefix(query: &'a str, prefix: &'a str) -> Self {
        Self { query, prefix }
    }

    pub(super) fn render(self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if !area.is_empty() {
            frame.render_widget(Paragraph::new(self.line(area.width, theme)), area);
        }
    }

    pub(super) fn line(self, width: u16, theme: &Theme) -> Line<'static> {
        self.line_with_suffix(width, Vec::new(), theme)
    }

    pub(super) fn line_with_suffix(
        self,
        width: u16,
        suffix: Vec<Span<'static>>,
        theme: &Theme,
    ) -> Line<'static> {
        let suffix_width = suffix
            .iter()
            .map(|span| span.content.width())
            .sum::<usize>();
        let available = usize::from(width)
            .saturating_sub(self.prefix.width())
            .saturating_sub(SEARCH_CURSOR.width())
            .saturating_sub(suffix_width);
        let query = visible_tail(self.query, available);
        let mut spans = vec![
            Span::styled(self.prefix.to_owned(), secondary(theme)),
            Span::styled(query.to_owned(), body(theme)),
            Span::styled(SEARCH_CURSOR, accent(theme)),
        ];
        if self.query.is_empty() && suffix.is_empty() {
            spans.push(Span::styled(SEARCH_PLACEHOLDER, secondary(theme)));
        }
        spans.extend(suffix);
        Line::from(spans)
    }
}

fn visible_tail(query: &str, width: usize) -> &str {
    let mut used = 0;
    for (index, grapheme) in query.grapheme_indices(true).rev() {
        used += grapheme.width();
        if used > width {
            return &query[index + grapheme.len()..];
        }
    }
    query
}

#[cfg(test)]
mod tests {
    use super::{ChoiceStyle, SearchField, visible_tail};
    use crate::tui::theme::Theme;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Color, Modifier},
    };

    #[test]
    fn search_field_keeps_the_unicode_tail_and_shows_focus() {
        assert_eq!(visible_tail("one界two", 5), "界two");

        let mut terminal = Terminal::new(TestBackend::new(16, 1)).unwrap();
        terminal
            .draw(|frame| {
                SearchField::new("one界two").render(frame, frame.area(), &Theme::default())
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(10, 0)].symbol(), "界");
        assert_eq!(buffer[(12, 0)].symbol(), "t");
        assert_eq!(buffer[(15, 0)].symbol(), "▏");
        let cursor = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "▏")
            .unwrap();
        assert_eq!(cursor.fg, Color::Blue);
        assert!(cursor.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn empty_search_field_explains_what_typing_does() {
        let mut terminal = Terminal::new(TestBackend::new(30, 1)).unwrap();
        terminal
            .draw(|frame| SearchField::new("").render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(text.contains("Search: ▏ type to filter"));
    }

    #[test]
    fn selected_choice_has_hierarchy_without_flattening_detail_text() {
        let theme = Theme::default();
        let selected = ChoiceStyle::new(true, true);
        let idle = ChoiceStyle::new(false, true);

        assert_eq!(selected.primary(&theme).fg, Some(theme.accent()));
        assert!(
            selected
                .primary(&theme)
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(selected.detail(&theme).fg, Some(theme.text()));
        assert_eq!(selected.highlight(&theme).bg, Some(theme.code_background()));
        assert_eq!(idle.primary(&theme).fg, Some(theme.text()));
        assert_eq!(idle.detail(&theme).fg, Some(theme.muted()));
    }
}
