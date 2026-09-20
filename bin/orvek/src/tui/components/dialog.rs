//! Shared chrome and layout for centered modal components.

use super::{
    choice::ScrollIndicator,
    node::{Component, ComponentUpdate, RenderRequest},
    typography,
};
use crate::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Shadow, Wrap},
};
use unicode_width::UnicodeWidthStr;

const KEY_BINDING_SEPARATOR: &str = " · ";

pub(super) type KeyBinding = (&'static str, &'static str);

pub(super) struct KeyHintBar<'a> {
    key_bindings: &'a [KeyBinding],
}

impl<'a> KeyHintBar<'a> {
    pub(super) const fn new(key_bindings: &'a [KeyBinding]) -> Self {
        Self { key_bindings }
    }

    pub(super) fn height(&self, width: u16, theme: &Theme) -> u16 {
        u16::try_from(self.lines(width, theme).len()).unwrap_or(u16::MAX)
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(
            Paragraph::new(self.lines(area.width, theme)).alignment(Alignment::Center),
            area,
        );
    }

    fn lines(&self, width: u16, theme: &Theme) -> Vec<Line<'a>> {
        key_binding_lines(self.key_bindings, width, theme)
    }
}

pub(super) struct Dialog<'a> {
    title: &'a str,
    width: u16,
    height: u16,
    key_bindings: &'a [KeyBinding],
    placement: Placement,
    border_color: Option<Color>,
    title_color: Option<Color>,
}

#[derive(Clone, Copy, Default)]
enum Placement {
    #[default]
    Center,
    Top,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct DialogLayout {
    pub(super) popup: Rect,
    pub(super) body: Rect,
    pub(super) footer: Rect,
}

impl<'a> Dialog<'a> {
    pub(super) const fn new(
        title: &'a str,
        width: u16,
        height: u16,
        key_bindings: &'a [KeyBinding],
    ) -> Self {
        Self {
            title,
            width,
            height,
            key_bindings,
            placement: Placement::Center,
            border_color: None,
            title_color: None,
        }
    }

    pub(super) const fn at_top(mut self) -> Self {
        self.placement = Placement::Top;
        self
    }

    pub(super) const fn colors(mut self, border: Color, title: Color) -> Self {
        self.border_color = Some(border);
        self.title_color = Some(title);
        self
    }

    pub(super) fn render(self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> DialogLayout {
        let popup = match self.placement {
            Placement::Center => centered(area, self.width, self.height),
            Placement::Top => top_centered(area, self.width, self.height),
        };
        let border_color = self.border_color.unwrap_or_else(|| theme.border());
        let title_color = self.title_color.unwrap_or_else(|| theme.accent());
        let key_hints = KeyHintBar::new(self.key_bindings);
        let spacious = popup.width >= 84;
        let bottom_hint = spacious
            .then(|| key_hints.lines(popup.width.saturating_sub(4), theme))
            .filter(|lines| lines.len() == 1)
            .and_then(|lines| lines.into_iter().next());
        let mut block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .padding(Padding::horizontal(u16::from(spacious)))
            .shadow(Shadow::light_shade().style(Style::default().fg(theme.overlay_shadow())));
        if !self.title.is_empty() {
            block = block
                .title(format!(" {} ", self.title))
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                );
        }
        if let Some(hint) = bottom_hint.clone() {
            block = block.title_bottom(hint.right_aligned());
        }
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);

        let (body, footer) = if bottom_hint.is_some() {
            (inner, Rect::default())
        } else {
            split_footer(inner, key_hints.height(inner.width, theme))
        };
        key_hints.render(frame, footer, theme);
        DialogLayout {
            popup,
            body,
            footer,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfirmationChoice {
    Confirm,
    Dismiss,
}

pub(super) struct ConfirmationDialog {
    title: &'static str,
    width: u16,
    height: u16,
    key_bindings: &'static [KeyBinding],
    body: fn(&Theme) -> Text<'static>,
    confirm_keys: &'static [KeyCode],
    dismiss_keys: &'static [KeyCode],
    body_area: Rect,
    scroll: usize,
    max_scroll: usize,
}

impl ConfirmationDialog {
    pub(super) const fn new(
        title: &'static str,
        width: u16,
        height: u16,
        key_bindings: &'static [KeyBinding],
        body: fn(&Theme) -> Text<'static>,
        confirm_keys: &'static [KeyCode],
        dismiss_keys: &'static [KeyCode],
    ) -> Self {
        Self {
            title,
            width,
            height,
            key_bindings,
            body,
            confirm_keys,
            dismiss_keys,
            body_area: Rect::ZERO,
            scroll: 0,
            max_scroll: 0,
        }
    }

    #[cfg(test)]
    pub(super) const fn scroll(&self) -> usize {
        self.scroll
    }

    fn scroll_by(&mut self, delta: isize) -> bool {
        let previous = self.scroll;
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll);
        previous != self.scroll
    }
}

impl Component for ConfirmationDialog {
    type Event = Event;
    type Effect = ConfirmationChoice;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        let scrolled = match event {
            Event::Mouse(mouse)
                if self
                    .body_area
                    .contains(Position::new(mouse.column, mouse.row)) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll_by(-3),
                    MouseEventKind::ScrollDown => self.scroll_by(3),
                    _ => false,
                }
            }
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Up => self.scroll_by(-1),
                    KeyCode::Down => self.scroll_by(1),
                    KeyCode::PageUp => self
                        .scroll_by(-isize::try_from(self.body_area.height).unwrap_or(isize::MAX)),
                    KeyCode::PageDown => {
                        self.scroll_by(isize::try_from(self.body_area.height).unwrap_or(isize::MAX))
                    }
                    KeyCode::Home => self.scroll_by(-(isize::MAX)),
                    KeyCode::End => self.scroll_by(isize::MAX),
                    _ => false,
                }
            }
            _ => false,
        };
        if scrolled {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }

        let Event::Key(key) = event else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        let choice = if self.confirm_keys.contains(&key.code) {
            Some(ConfirmationChoice::Confirm)
        } else if self.dismiss_keys.contains(&key.code) {
            Some(ConfirmationChoice::Dismiss)
        } else {
            None
        };
        choice.map_or_else(ComponentUpdate::none, |choice| ComponentUpdate {
            effects: vec![choice],
            render: RenderRequest::Immediate,
        })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let layout = Dialog::new(self.title, self.width, self.height, self.key_bindings)
            .render(frame, area, theme);
        self.body_area = layout.body;
        let text = (self.body)(theme);
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        let line_count = paragraph.line_count(self.body_area.width);
        self.max_scroll = line_count.saturating_sub(usize::from(self.body_area.height));
        self.scroll = self.scroll.min(self.max_scroll);
        frame.render_widget(
            paragraph.scroll((u16::try_from(self.scroll).unwrap_or(u16::MAX), 0)),
            self.body_area,
        );
        let indicator_area = Rect {
            x: self.body_area.right().saturating_sub(1),
            width: u16::from(!self.body_area.is_empty()),
            ..self.body_area
        };
        ScrollIndicator::new(self.scroll, usize::from(self.body_area.height), line_count).render(
            frame,
            indicator_area,
            theme,
        );
    }
}

fn key_binding_lines<'a>(
    key_bindings: &'a [KeyBinding],
    width: u16,
    theme: &Theme,
) -> Vec<Line<'a>> {
    if width == 0 {
        return Vec::new();
    }

    let width = usize::from(width);
    let separator_width = KEY_BINDING_SEPARATOR.width();
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut line_width = 0;
    for &(key, help) in key_bindings {
        let key_binding_width =
            key.width() + usize::from(!key.is_empty() && !help.is_empty()) + help.width();
        if !spans.is_empty() && line_width + separator_width + key_binding_width > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            line_width = 0;
        }
        if !spans.is_empty() {
            spans.push(Span::styled(
                KEY_BINDING_SEPARATOR,
                typography::secondary(theme),
            ));
            line_width += separator_width;
        }
        if !key.is_empty() {
            spans.push(Span::styled(key, typography::key(theme)));
        }
        if !help.is_empty() {
            spans.push(Span::styled(
                if key.is_empty() {
                    help.to_owned()
                } else {
                    format!(" {help}")
                },
                typography::secondary(theme),
            ));
        }
        line_width += key_binding_width;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn top_centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y,
        width,
        height: height.min(area.height),
    }
}

fn split_footer(inner: Rect, footer_height: u16) -> (Rect, Rect) {
    let footer_height = footer_height.min(inner.height);
    if footer_height == 0 {
        return (inner, Rect::default());
    }
    let footer = Rect {
        y: inner.bottom() - footer_height,
        height: footer_height,
        ..inner
    };
    let body = Rect {
        height: inner.height - footer_height,
        ..inner
    };
    (body, footer)
}

pub(super) struct KeyConfirmationLabels<'a> {
    key: &'a str,
    action: &'a str,
    cancel_key: &'a str,
}

impl<'a> KeyConfirmationLabels<'a> {
    pub(super) const fn new(key: &'a str, action: &'a str, cancel_key: &'a str) -> Self {
        Self {
            key,
            action,
            cancel_key,
        }
    }
}

/// A short-lived, repeated-key confirmation anchored above an input area.
pub(super) struct TimedKeyConfirmation<A> {
    action: A,
    expires_at: std::time::Instant,
}

impl<A: Copy + Eq> TimedKeyConfirmation<A> {
    pub(super) fn new(action: A, now: std::time::Instant, timeout: std::time::Duration) -> Self {
        Self {
            action,
            expires_at: now + timeout,
        }
    }

    pub(super) const fn action(&self) -> A {
        self.action
    }

    pub(super) const fn deadline(&self) -> std::time::Instant {
        self.expires_at
    }

    pub(super) fn confirms(&self, action: A, now: std::time::Instant) -> bool {
        self.action == action && now <= self.expires_at
    }

    pub(super) fn expired(&self, now: std::time::Instant) -> bool {
        now >= self.expires_at
    }

    pub(super) fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        anchor: Rect,
        theme: &Theme,
        labels: KeyConfirmationLabels<'_>,
    ) {
        const HEIGHT: u16 = 4;
        const WIDTH: u16 = 28;

        let available_height = anchor.y.saturating_sub(area.y);
        if available_height < HEIGHT {
            return;
        }
        let width = WIDTH.min(anchor.width).min(area.width);
        let gap = u16::from(available_height > HEIGHT);
        let popup = Rect {
            x: anchor.right().saturating_sub(width).max(area.x),
            y: anchor.y.saturating_sub(HEIGHT + gap),
            width,
            height: HEIGHT,
        };
        let title = Line::from(vec![
            Span::styled(
                format!(" {} ", labels.key),
                Style::reset().add_modifier(Modifier::BOLD),
            ),
            Span::styled("then ", Style::default().fg(theme.muted())),
        ]);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border()))
            .title(title);
        let body = block.inner(popup);

        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        frame.render_widget(
            Paragraph::new(vec![
                confirmation_line(labels.key, labels.action, theme),
                confirmation_line(labels.cancel_key, "cancel", theme),
            ]),
            body,
        );
    }
}

fn confirmation_line<'a>(key: &'a str, label: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(key, Style::reset().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {label}"), Style::default().fg(theme.muted())),
    ])
}

#[cfg(test)]
mod tests {
    use super::Dialog;
    use crate::tui::theme::{Theme, ThemeMode};
    use ratatui::{
        Terminal,
        backend::TestBackend,
        layout::Rect,
        style::{Color, Modifier, Style},
    };

    #[test]
    fn all_dialog_placements_use_light_cappuccino_pink_shadows() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            for at_top in [false, true] {
                for (width, height) in [(20, 8), (40, 12), (100, 30)] {
                    let mut theme = Theme::default();
                    theme.set_mode(mode);
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    let mut popup = Rect::default();
                    terminal
                        .draw(|frame| {
                            let area = frame.area();
                            frame
                                .buffer_mut()
                                .set_style(area, Style::default().bg(theme.background()));
                            let dialog = Dialog::new("Test", 16, 6, &[]);
                            let dialog = if at_top { dialog.at_top() } else { dialog };
                            popup = dialog.render(frame, frame.area(), &theme).popup;
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    for position in [(popup.right(), popup.y + 1), (popup.x + 1, popup.bottom())] {
                        let shadow = &buffer[position];
                        assert_eq!(shadow.symbol(), "░");
                        assert_eq!(shadow.fg, Color::Rgb(0xD6, 0xB1, 0xAB));
                        assert_eq!(shadow.bg, theme.background());
                    }
                    assert_eq!(buffer[(popup.x, popup.y)].symbol(), "╭");
                    assert_eq!(buffer[(popup.x, popup.y)].fg, theme.border());
                }
            }
        }
    }

    #[test]
    fn dialog_centers_rounded_chrome_and_styles_keys_separately_from_help() {
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();

        terminal
            .draw(|frame| {
                Dialog::new("Test", 16, 6, &[("left", "help")]).render(
                    frame,
                    frame.area(),
                    &Theme::default(),
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "╭");
        assert_eq!(buffer[(17, 6)].symbol(), "╯");
        let row = 5;
        let start = (0..20)
            .find(|&column| buffer[(column, row)].symbol() == "l")
            .unwrap();
        assert_eq!(buffer[(start, row)].fg, Theme::default().accent());
        assert!(buffer[(start, row)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(start + 5, row)].fg, Theme::default().muted());
    }

    #[test]
    fn wide_dialog_moves_single_line_hints_into_the_bottom_border() {
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        let mut layout = super::DialogLayout::default();

        terminal
            .draw(|frame| {
                layout = Dialog::new("Test", 90, 6, &[("enter", "open"), ("esc", "close")]).render(
                    frame,
                    frame.area(),
                    &Theme::default(),
                );
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(layout.footer.is_empty());
        assert_eq!(layout.body.height, 4);
        assert!(rendered.contains("enter open · esc close"));
    }

    #[test]
    fn dialog_wraps_key_bindings_without_clipping_menu_help() {
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        let mut body = Rect::default();

        terminal
            .draw(|frame| {
                body = Dialog::new(
                    "Test",
                    24,
                    8,
                    &[
                        ("first", "option"),
                        ("second", "option"),
                        ("third", "option"),
                    ],
                )
                .render(frame, frame.area(), &Theme::default())
                .body;
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(body.height, 3);
        assert!(rendered.contains("first option"));
        assert!(rendered.contains("second option"));
        assert!(rendered.contains("third option"));
    }
}
