//! Disposable queue view. Only host receipts change membership and order.
use super::{
    node::{Component, ComponentUpdate, RenderRequest},
    waved_text::WavedText,
};
use crate::tui::{format::sanitize_terminal_text_inline, prompt::Submission, theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use orvek_harness::Digest;
use ratatui::{
    Frame,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders},
};
use std::{borrow::Cow, time::Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
const UPDATING_TEXT: &str = "updating";
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct QueueId(pub(crate) uuid::Uuid);
impl QueueId {
    #[cfg(test)]
    pub(crate) const fn new(value: u64) -> Self {
        Self(uuid::Uuid::from_u128(value as u128))
    }
}
#[derive(Clone, Debug)]
pub(crate) struct QueuedInput {
    pub(crate) id: QueueId,
    pub(crate) input: Digest,
    pub(crate) prompt: Submission,
}
#[derive(Debug, Eq, PartialEq)]
pub(super) enum QueueEffect {
    Blur,
    Edit {
        id: QueueId,
        expected_input: Digest,
        prompt: Submission,
    },
    Steer {
        id: QueueId,
        expected_input: Digest,
    },
    Remove {
        id: QueueId,
    },
    Move {
        id: QueueId,
        expected_input: Digest,
        before: Option<QueueId>,
    },
}
pub(super) enum QueueEvent {
    Terminal(Event),
    AnimationFrame(Instant),
}
struct QueueItem {
    id: QueueId,
    input: Digest,
    prompt: Submission,
    state: QueueItemState,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum QueueItemState {
    Queued,
    Editing,
    Updating,
}
pub(super) struct MessageQueue {
    items: Vec<QueueItem>,
    selected: usize,
    first_visible: usize,
    area: Rect,
    focused: bool,
    updating_label: WavedText,
}
impl Default for MessageQueue {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            first_visible: 0,
            area: Rect::default(),
            focused: false,
            updating_label: WavedText::new(UPDATING_TEXT, Color::Rgb(220, 220, 220)),
        }
    }
}
impl MessageQueue {
    pub(super) fn replace(&mut self, inputs: Vec<QueuedInput>) {
        let selected = self.items.get(self.selected).map(|item| item.id);
        self.items = inputs
            .into_iter()
            .map(|input| QueueItem {
                id: input.id,
                input: input.input,
                prompt: input.prompt,
                state: QueueItemState::Queued,
            })
            .collect();
        self.selected = selected
            .and_then(|id| self.items.iter().position(|item| item.id == id))
            .unwrap_or(self.items.len().saturating_sub(1));
        self.focused &= !self.items.is_empty();
        self.first_visible = self.first_visible.min(self.max_offset());
        if self.focused {
            self.reveal_selected();
        }
        self.sync_wave();
    }
    #[cfg(test)]
    pub(super) fn push(&mut self, prompt: impl Into<Submission>) {
        self.items.push(QueueItem {
            id: QueueId::new(self.items.len() as u64),
            input: Digest::of(b"fixture"),
            prompt: prompt.into(),
            state: QueueItemState::Queued,
        });
        self.selected = self.items.len() - 1;
    }
    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.items.len()
    }
    pub(super) fn focused(&self) -> bool {
        self.focused
    }
    pub(super) fn has_pending_action(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.state == QueueItemState::Updating)
    }
    pub(super) fn cancel_edit(&mut self, id: QueueId) -> bool {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.state = QueueItemState::Queued;
            true
        } else {
            false
        }
    }
    pub(super) fn set_focused(&mut self, focused: bool) {
        self.focused = focused && !self.items.is_empty();
        if self.focused {
            self.reveal_selected();
        }
    }
    pub(super) fn focus_row(&mut self, row: u16, area: Rect) -> bool {
        if self.items.is_empty() || row <= area.y || row >= area.bottom().saturating_sub(1) {
            return false;
        }
        self.selected =
            (self.first_visible + usize::from((row - area.y - 1) / 2)).min(self.items.len() - 1);
        self.focused = !self.items.is_empty();
        true
    }
    pub(super) fn desired_height(&self) -> u16 {
        if self.items.is_empty() {
            0
        } else {
            u16::try_from(self.items.len().saturating_mul(2) + 1).unwrap_or(u16::MAX)
        }
    }
    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        self.updating_label.animation_deadline()
    }
    fn visible_items(&self) -> usize {
        usize::from(self.area.height.saturating_sub(1) / 2)
    }

    fn max_offset(&self) -> usize {
        self.items.len().saturating_sub(self.visible_items().max(1))
    }

    fn reveal_selected(&mut self) {
        let visible = self.visible_items().max(1);
        if self.selected < self.first_visible {
            self.first_visible = self.selected;
        } else if self.selected >= self.first_visible + visible {
            self.first_visible = self.selected + 1 - visible;
        }
        self.first_visible = self.first_visible.min(self.max_offset());
    }

    fn sync_wave(&mut self) {
        self.updating_label
            .set_active(self.has_pending_action(), Instant::now());
    }
    fn update_terminal(&mut self, event: Event) -> ComponentUpdate<QueueEffect> {
        if let Event::Mouse(mouse) = &event {
            if !self.area.contains(Position::new(mouse.column, mouse.row))
                || self.visible_items() == 0
                || mouse.modifiers.contains(KeyModifiers::SHIFT)
            {
                return ComponentUpdate::none();
            }
            let next = match mouse.kind {
                MouseEventKind::ScrollUp => self.first_visible.saturating_sub(3),
                MouseEventKind::ScrollDown => {
                    self.first_visible.saturating_add(3).min(self.max_offset())
                }
                _ => return ComponentUpdate::none(),
            };
            if next == self.first_visible {
                return ComponentUpdate::none();
            }
            self.first_visible = next;
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        let Event::Key(key) = event else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) || self.items.is_empty()
        {
            return ComponentUpdate::none();
        }
        if key.code == KeyCode::Esc {
            self.focused = false;
            return ComponentUpdate {
                effects: vec![QueueEffect::Blur],
                render: RenderRequest::Immediate,
            };
        }
        if matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) && !key.modifiers.contains(KeyModifiers::SHIFT)
        {
            let step = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                self.visible_items().max(1)
            } else {
                1
            };
            self.selected = if matches!(key.code, KeyCode::Down | KeyCode::PageDown) {
                self.selected.saturating_add(step).min(self.items.len() - 1)
            } else {
                self.selected.saturating_sub(step)
            };
            self.reveal_selected();
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        let selected = self.selected;
        let item = &self.items[selected];
        if item.state != QueueItemState::Queued {
            return ComponentUpdate::none();
        }
        let id = item.id;
        let expected_input = item.input;
        let effect = match key.code {
            KeyCode::Char('e') => QueueEffect::Edit {
                id,
                expected_input,
                prompt: item.prompt.clone(),
            },
            KeyCode::Enter => QueueEffect::Steer { id, expected_input },
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => QueueEffect::Remove { id },
            KeyCode::Up if selected > 0 => QueueEffect::Move {
                id,
                expected_input,
                before: Some(self.items[selected - 1].id),
            },
            KeyCode::Down if selected + 1 < self.items.len() => QueueEffect::Move {
                id,
                expected_input,
                before: self.items.get(selected + 2).map(|item| item.id),
            },
            _ => return ComponentUpdate::none(),
        };
        self.items[selected].state = if matches!(effect, QueueEffect::Edit { .. }) {
            QueueItemState::Editing
        } else {
            QueueItemState::Updating
        };
        self.sync_wave();
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }
    fn focused_title(&self, width: u16) -> Option<String> {
        let item = self.items.get(self.selected)?;
        let variants: &[&str] = match item.state {
            QueueItemState::Queued => &[
                " ↑↓ select · ⇧↑↓ reorder · e edit · enter steer · d delete · esc back ",
                " ↑↓ select · e edit · enter steer · d delete ",
                " ↑↓ select · enter steer ",
                " ↑↓ select ",
            ],
            QueueItemState::Editing => &[" enter save · esc cancel ", " esc cancel "],
            QueueItemState::Updating => &[" waiting for host · esc back ", " waiting for host "],
        };
        variants
            .iter()
            .find(|value| UnicodeWidthStr::width(**value) <= usize::from(width))
            .map(|value| (*value).to_owned())
    }
}
impl Component for MessageQueue {
    type Event = QueueEvent;
    type Effect = QueueEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            QueueEvent::Terminal(event) => self.update_terminal(event),
            QueueEvent::AnimationFrame(now) => {
                let changed = self.updating_label.advance(now);
                ComponentUpdate::render(if changed {
                    RenderRequest::Streaming
                } else {
                    RenderRequest::None
                })
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let resized = self.area != area;
        self.area = area;
        self.first_visible = self.first_visible.min(self.max_offset());
        if resized && self.focused {
            self.reveal_selected();
        }
        if area.width < 4 || area.height < 3 || self.items.is_empty() {
            return;
        }

        let border = theme.border();
        let title = if self.updating_label.is_active() {
            let mut spans = Vec::with_capacity(UPDATING_TEXT.len() + 2);
            spans.push(Span::styled(" queue · ", Style::default().fg(border)));
            spans.extend(self.updating_label.spans());
            spans.push(Span::styled(" ", Style::default().fg(border)));
            Line::from(spans)
        } else {
            Line::styled(" queue · enter steer latest ", Style::default().fg(border))
        };
        let mut block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title(title);
        if self.focused
            && let Some(title) = self.focused_title(area.width.saturating_sub(2))
        {
            block = block
                .title_bottom(Line::styled(title, Style::default().fg(border)))
                .title_alignment(Alignment::Center);
        }
        frame.render_widget(block, area);

        let content_width = usize::from(area.width.saturating_sub(4));
        for (visible_index, (index, item)) in self
            .items
            .iter()
            .enumerate()
            .skip(self.first_visible)
            .take(self.visible_items())
            .enumerate()
        {
            let offset = u16::try_from(visible_index * 2).unwrap_or(u16::MAX);
            let row_y = area.y + 1 + offset;
            if visible_index > 0 {
                let y = row_y - 1;
                frame
                    .buffer_mut()
                    .set_string(area.x, y, "├", Style::default().fg(border));
                for x in area.x + 1..area.right().saturating_sub(1) {
                    frame
                        .buffer_mut()
                        .set_string(x, y, "─", Style::default().fg(border));
                }
                frame.buffer_mut().set_string(
                    area.right().saturating_sub(1),
                    y,
                    "┤",
                    Style::default().fg(border),
                );
            }

            if row_y >= area.bottom().saturating_sub(1) {
                break;
            }
            let mut style = if self.focused && index == self.selected {
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default().fg(theme.text())
            };
            if item.state != QueueItemState::Queued {
                style = style.fg(theme.muted()).add_modifier(Modifier::ITALIC);
            }
            frame.buffer_mut().set_stringn(
                area.x + 2,
                row_y,
                truncate(item.prompt.display_text(), content_width),
                content_width,
                style,
            );
        }
    }
}

fn truncate(text: &str, width: usize) -> Cow<'_, str> {
    let text = sanitize_terminal_text_inline(text);
    if UnicodeWidthStr::width(text.as_ref()) <= width {
        return text;
    }
    if width == 0 {
        return Cow::Borrowed("");
    }

    let mut result = String::new();
    let available = width.saturating_sub(1);
    for grapheme in text.graphemes(true) {
        if UnicodeWidthStr::width(result.as_str()) + UnicodeWidthStr::width(grapheme) > available {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    Cow::Owned(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    fn key(code: KeyCode, modifiers: KeyModifiers) -> QueueEvent {
        QueueEvent::Terminal(Event::Key(KeyEvent::new(code, modifiers)))
    }
    fn rendered_queue(queue: &mut MessageQueue, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(48, height)).unwrap();
        terminal
            .draw(|frame| queue.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn wheel_reaches_hidden_queue_items_and_clicks_keep_their_identity() {
        let mut queue = MessageQueue::default();
        for index in 0..10 {
            queue.push(format!("queued item {index}"));
        }
        assert!(rendered_queue(&mut queue, 7).contains("queued item 0"));
        for _ in 0..20 {
            let update = queue.update(QueueEvent::Terminal(Event::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::ScrollDown,
                    column: 5,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                },
            )));
            assert!(update.effects.is_empty());
        }
        let output = rendered_queue(&mut queue, 7);
        assert!(output.contains("queued item 7"));
        assert!(output.contains("queued item 9"));
        assert!(!output.contains("queued item 0"));
        assert!(!queue.focused());
        assert_eq!(queue.len(), 10);
        queue.focus_row(1, Rect::new(0, 0, 48, 7));
        let update = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(update.effects.as_slice(), [QueueEffect::Steer { id, .. }] if *id == QueueId::new(7))
        );
    }

    #[test]
    fn queue_wheel_is_bounded_and_ignores_other_surfaces() {
        let mut queue = MessageQueue::default();
        for index in 0..10 {
            queue.push(format!("queued item {index}"));
        }
        let before = rendered_queue(&mut queue, 7);
        for (kind, column, row) in [
            (MouseEventKind::ScrollDown, 48, 3),
            (MouseEventKind::ScrollDown, 5, 7),
            (MouseEventKind::ScrollUp, 5, 3),
        ] {
            let update = queue.update(QueueEvent::Terminal(Event::Mouse(
                crossterm::event::MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
            )));
            assert!(update.effects.is_empty());
            assert_eq!(update.render, RenderRequest::None);
        }
        assert_eq!(before, rendered_queue(&mut queue, 7));
        assert_eq!(queue.len(), 10);
    }

    #[test]
    fn queue_page_navigation_and_resize_keep_the_selection_visible() {
        let mut queue = MessageQueue::default();
        for index in 0..10 {
            queue.push(format!("queued item {index}"));
        }
        rendered_queue(&mut queue, 7);
        queue.set_focused(true);
        assert!(rendered_queue(&mut queue, 7).contains("queued item 9"));
        let update = queue.update(key(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(update.effects.is_empty());
        assert!(rendered_queue(&mut queue, 7).contains("queued item 6"));
        assert!(rendered_queue(&mut queue, 3).contains("queued item 6"));
        for _ in 0..20 {
            queue.update(key(KeyCode::PageUp, KeyModifiers::NONE));
        }
        assert!(rendered_queue(&mut queue, 3).contains("queued item 0"));
        let update = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(update.effects.as_slice(), [QueueEffect::Steer { id, .. }] if *id == QueueId::new(0))
        );
        assert_eq!(queue.len(), 10);
    }

    #[test]
    fn host_receipt_is_required_to_remove_or_reorder_entries() {
        let mut queue = MessageQueue::default();
        queue.push("first".to_owned());
        queue.push("second".to_owned());
        let update = queue.update(key(KeyCode::Up, KeyModifiers::SHIFT));
        assert!(
            matches!(update.effects.as_slice(),[QueueEffect::Move{id, before:Some(before),..}] if *id==QueueId::new(1)&&*before==QueueId::new(0))
        );
        assert_eq!(queue.items[0].prompt.display_text(), "first");
        assert_eq!(queue.len(), 2);
        queue.replace(vec![QueuedInput {
            id: QueueId::new(1),
            input: Digest::of(b"fixture"),
            prompt: "second".to_owned().into(),
        }]);
        let update = queue.update(key(KeyCode::Delete, KeyModifiers::NONE));
        assert!(matches!(
            update.effects.as_slice(),
            [QueueEffect::Remove { .. }]
        ));
        assert_eq!(queue.len(), 1);
        queue.replace(Vec::new());
        assert!(queue.is_empty());
    }
    #[test]
    fn edit_keeps_input_identity_and_images() {
        let prompt = Submission::multimodal(
            "[Image #1]".into(),
            [(0..10, "data:image/png;base64,AA==".into())],
        );
        let mut queue = MessageQueue::default();
        let digest = Digest::of(b"source");
        queue.replace(vec![QueuedInput {
            id: QueueId::new(2),
            input: digest,
            prompt: prompt.clone(),
        }]);
        let update = queue.update(key(KeyCode::Char('e'), KeyModifiers::NONE));
        assert!(
            matches!(&update.effects[..],[QueueEffect::Edit{expected_input,prompt:actual,..}] if *expected_input==digest&&actual==&prompt)
        );
        assert!(queue.cancel_edit(QueueId::new(2)));
        assert_eq!(queue.len(), 1);
    }
    #[test]
    fn terminal_preview_is_bounded_and_sanitized() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert!(!truncate("before\x1b[31m after", 100).contains('\x1b'));
    }
}
