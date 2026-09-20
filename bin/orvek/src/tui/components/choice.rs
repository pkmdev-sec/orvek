//! Shared selection, viewport, and scroll treatment for list-based pickers.

use super::typography::{CHOICE_MARKER, ChoiceStyle};
use crate::tui::theme::Theme;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    widgets::{List, ListItem, ListState, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

#[derive(Debug)]
pub(super) struct ListViewport {
    area: Rect,
    item_count: usize,
    item_height: u16,
    offset: usize,
}

impl ListViewport {
    pub(super) fn new(item_height: u16) -> Self {
        Self {
            area: Rect::default(),
            item_count: 0,
            item_height: item_height.max(1),
            offset: 0,
        }
    }

    pub(super) const fn area(&self) -> Rect {
        self.area
    }

    pub(super) fn contains(&self, position: Position) -> bool {
        self.area.contains(position)
    }

    pub(super) const fn offset(&self) -> usize {
        self.offset
    }

    pub(super) const fn item_count(&self) -> usize {
        self.item_count
    }

    pub(super) fn visible_items(&self) -> usize {
        usize::from(self.area.height / self.item_height)
    }

    pub(super) fn set_area(&mut self, area: Rect) {
        self.area = area;
        self.clamp_offset();
    }

    pub(super) fn set_item_count(&mut self, item_count: usize) {
        self.item_count = item_count;
        self.clamp_offset();
    }

    pub(super) fn reset(&mut self, item_count: usize) {
        self.item_count = item_count;
        self.offset = 0;
    }

    pub(super) fn ensure_visible(&mut self, index: usize) {
        if self.item_count == 0 {
            self.offset = 0;
            return;
        }
        if self.area.height < self.item_height {
            return;
        }
        let visible = self.visible_items();
        let index = index.min(self.item_count - 1);
        if index < self.offset {
            self.offset = index;
        } else if index >= self.offset.saturating_add(visible) {
            self.offset = index + 1 - visible;
        }
        self.clamp_offset();
    }

    fn clamp_offset(&mut self) {
        let visible = self.visible_items().max(1);
        self.offset = self.offset.min(self.item_count.saturating_sub(visible));
    }
}

#[derive(Debug)]
pub(super) struct ChoicePicker {
    selected: Option<usize>,
    viewport: ListViewport,
}

impl ChoicePicker {
    pub(super) fn new(item_count: usize, item_height: u16) -> Self {
        Self {
            selected: (item_count > 0).then_some(0),
            viewport: {
                let mut viewport = ListViewport::new(item_height);
                viewport.set_item_count(item_count);
                viewport
            },
        }
    }

    pub(super) const fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub(super) fn selected_or_zero(&self) -> usize {
        self.selected.unwrap_or(0)
    }

    pub(super) const fn is_selected(&self, index: usize) -> bool {
        matches!(self.selected, Some(selected) if selected == index)
    }

    pub(super) fn contains(&self, position: Position) -> bool {
        self.viewport.contains(position)
    }

    pub(super) fn reset(&mut self, item_count: usize) -> bool {
        let next = (item_count > 0).then_some(0);
        let changed = self.selected != next || self.viewport.offset() != 0;
        self.selected = next;
        self.viewport.reset(item_count);
        changed
    }

    pub(super) fn set_item_count(&mut self, item_count: usize) -> bool {
        let previous = self.selected;
        self.selected = match (self.selected, item_count) {
            (_, 0) => None,
            (Some(selected), count) => Some(selected.min(count - 1)),
            (None, _) => Some(0),
        };
        self.viewport.set_item_count(item_count);
        if let Some(selected) = self.selected {
            self.viewport.ensure_visible(selected);
        }
        previous != self.selected
    }

    pub(super) fn move_by(&mut self, delta: isize) -> bool {
        let Some(selected) = self.selected else {
            return false;
        };
        let next = selected
            .saturating_add_signed(delta)
            .min(self.viewport.item_count().saturating_sub(1));
        if next == selected {
            return false;
        }
        self.selected = Some(next);
        self.viewport.ensure_visible(next);
        true
    }

    pub(super) fn page_by(&mut self, pages: isize) -> bool {
        let page = self.viewport.visible_items().max(1);
        let delta = pages.saturating_mul(isize::try_from(page).unwrap_or(isize::MAX));
        self.move_by(delta)
    }

    pub(super) fn set_area(&mut self, area: Rect) {
        self.viewport.set_area(area);
        if let Some(selected) = self.selected {
            self.viewport.ensure_visible(selected);
        }
    }

    pub(super) const fn area(&self) -> Rect {
        self.viewport.area()
    }

    pub(super) const fn offset(&self) -> usize {
        self.viewport.offset()
    }

    pub(super) fn visible_items(&self) -> usize {
        self.viewport.visible_items()
    }

    pub(super) fn render<'a>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        items: Vec<ListItem<'a>>,
        selected_enabled: bool,
        theme: &Theme,
    ) {
        self.set_item_count(items.len());
        self.set_area(area);
        if area.is_empty() {
            return;
        }

        let indicator = ScrollIndicator::for_viewport(&self.viewport);
        let (list_area, indicator_area) = if indicator.is_needed() && area.width > 1 {
            (
                Rect {
                    width: area.width - 1,
                    ..area
                },
                Rect {
                    x: area.right() - 1,
                    width: 1,
                    ..area
                },
            )
        } else {
            (area, Rect::default())
        };
        let list = List::new(items)
            .style(Style::default().fg(theme.text()))
            .highlight_style(ChoiceStyle::new(true, selected_enabled).highlight(theme))
            .highlight_symbol(CHOICE_MARKER);
        let mut state = ListState::default()
            .with_selected(self.selected)
            .with_offset(self.viewport.offset());
        frame.render_stateful_widget(list, list_area, &mut state);
        indicator.render(frame, indicator_area, theme);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScrollIndicator {
    offset: usize,
    visible: usize,
    total: usize,
}

impl ScrollIndicator {
    pub(super) const fn new(offset: usize, visible: usize, total: usize) -> Self {
        Self {
            offset,
            visible,
            total,
        }
    }

    pub(super) fn for_viewport(viewport: &ListViewport) -> Self {
        Self::new(
            viewport.offset(),
            viewport.visible_items(),
            viewport.item_count(),
        )
    }

    pub(super) const fn is_needed(self) -> bool {
        self.visible > 0 && self.total > self.visible
    }

    pub(super) fn render(self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if !self.is_needed() || area.is_empty() {
            return;
        }
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(Style::default().fg(theme.scroll_track()))
            .thumb_symbol("┃")
            .thumb_style(Style::default().fg(theme.scroll_thumb()));
        let mut state =
            ScrollbarState::new(self.total.saturating_sub(self.visible).saturating_add(1))
                .position(self.offset)
                .viewport_content_length(self.visible);
        frame.render_stateful_widget(scrollbar, area, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::{ChoicePicker, ListViewport, ScrollIndicator};
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, widgets::ListItem};

    #[test]
    fn selection_and_viewport_stay_in_range() {
        let mut picker = ChoicePicker::new(10, 1);
        picker.set_area(Rect::new(0, 0, 10, 3));

        assert!(picker.move_by(7));
        assert_eq!(picker.selected(), Some(7));
        assert_eq!(picker.viewport.offset(), 5);
        assert!(picker.move_by(-20));
        assert_eq!(picker.selected(), Some(0));
        assert_eq!(picker.viewport.offset(), 0);
        assert!(picker.set_item_count(0));
        assert_eq!(picker.selected(), None);
        assert!(!picker.move_by(1));
    }

    #[test]
    fn paging_respects_multi_line_items() {
        let mut picker = ChoicePicker::new(20, 2);
        picker.set_area(Rect::new(0, 0, 20, 7));

        assert!(picker.page_by(1));
        assert_eq!(picker.selected(), Some(3));
    }

    #[test]
    fn indicator_moves_from_top_to_bottom() {
        let mut terminal = Terminal::new(TestBackend::new(8, 5)).unwrap();
        let mut viewport = ListViewport::new(1);
        viewport.set_item_count(20);
        viewport.set_area(Rect::new(0, 0, 8, 5));
        viewport.ensure_visible(19);
        let indicator = ScrollIndicator::for_viewport(&viewport);

        terminal
            .draw(|frame| indicator.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(7, 4)].symbol(), "┃");
        assert_eq!(buffer[(7, 4)].fg, Theme::default().scroll_thumb());
        assert_eq!(buffer[(7, 0)].fg, Theme::default().scroll_track());
    }

    #[test]
    fn picker_reserves_indicator_only_when_content_overflows() {
        let mut picker = ChoicePicker::new(5, 1);
        let mut terminal = Terminal::new(TestBackend::new(8, 3)).unwrap();
        let items = (0..5)
            .map(|index| ListItem::new(format!("item {index}")))
            .collect();

        terminal
            .draw(|frame| {
                picker.render(frame, frame.area(), items, true, &Theme::default());
            })
            .unwrap();

        assert_eq!(terminal.backend().buffer()[(7, 0)].symbol(), "┃");
    }
}
