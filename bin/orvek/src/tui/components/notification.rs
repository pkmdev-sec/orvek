//! A transient notice and a separate, read-only snapshot of its full message.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    format::{sanitize_terminal_text, truncate_display, wrap_display_lines},
    theme::Theme,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use semver::Version;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const DISPLAY_DURATION: Duration = Duration::from_secs(10);
const MAX_WIDTH: u16 = 64;
const MAX_BODY_HEIGHT: u16 = 4;
const DETAIL_KEYS: [(&str, &str); 3] = [("↑↓/pgup/pgdn", "scroll"), ("c", "copy"), ("esc", "back")];

pub(super) struct Notification {
    pub(super) message: Line<'static>,
    pub(super) color: Color,
    pub(super) deadline: Instant,
    visibility: Visibility,
    natural_width: u16,
    wrapped: Option<WrappedMessage>,
}

enum Visibility {
    Hidden { remaining: Duration },
    Visible,
}

struct WrappedMessage {
    width: u16,
    lines: Vec<Line<'static>>,
}

impl Notification {
    pub(super) fn plain(message: String, color: Color) -> Self {
        Self::new(
            Line {
                // Line::styled removes newlines; a span retains the complete message.
                spans: vec![Span::raw(sanitize_terminal_text(&message).into_owned())],
                style: Style::default().fg(color).add_modifier(Modifier::BOLD),
                ..Line::default()
            },
            color,
        )
    }

    pub(super) fn update_available(version: Version) -> Self {
        let green = Style::default().fg(Color::Green);
        Self::new(
            Line::from(vec![
                Span::styled("Update available · ", green),
                Span::styled(format!("v{version}"), green.add_modifier(Modifier::BOLD)),
                Span::styled(" · run ", green),
                Span::styled("`orvek update`", Style::default().fg(Color::Reset)),
            ]),
            Color::Green,
        )
    }

    fn new(message: Line<'static>, color: Color) -> Self {
        let longest_line = message_text(&message)
            .split('\n')
            .map(UnicodeWidthStr::width)
            .max()
            .unwrap_or(0);
        Self {
            message,
            color,
            deadline: Instant::now() + DISPLAY_DURATION,
            visibility: Visibility::Hidden {
                remaining: DISPLAY_DURATION,
            },
            natural_width: longest_line
                .saturating_add(4)
                .clamp(12, usize::from(MAX_WIDTH)) as u16,
            wrapped: None,
        }
    }

    pub(super) fn visibility(&mut self, visible: bool, now: Instant) {
        match (&self.visibility, visible) {
            (Visibility::Hidden { remaining }, true) => {
                self.deadline = now + *remaining;
                self.visibility = Visibility::Visible;
            }
            (Visibility::Visible, false) => {
                self.visibility = Visibility::Hidden {
                    remaining: self.deadline.saturating_duration_since(now),
                };
            }
            _ => {}
        }
    }

    pub(super) const fn expires_at(&self) -> Option<Instant> {
        match self.visibility {
            Visibility::Visible => Some(self.deadline),
            Visibility::Hidden { .. } => None,
        }
    }

    pub(super) fn expired(&self, now: Instant) -> bool {
        match self.visibility {
            Visibility::Visible => now >= self.deadline,
            Visibility::Hidden { remaining } => remaining.is_zero(),
        }
    }

    pub(super) fn render(
        &mut self,
        frame: &mut Frame<'_>,
        transcript_area: Rect,
        theme: &Theme,
    ) -> Option<Rect> {
        let area = transcript_area.intersection(frame.area());
        if area.width < 3 || area.height < 3 {
            return None;
        }
        let width = self.natural_width.min(area.width);
        let inset = u16::from(width >= 5);
        let body_width = width.saturating_sub(2 + inset * 2);
        let wrapped = WrappedMessage::get(&mut self.wrapped, &self.message, body_width);
        let body_limit = MAX_BODY_HEIGHT.min(area.height - 2);
        let overflow = wrapped.lines.len() > usize::from(body_limit);
        let capacity = body_limit.saturating_sub(u16::from(overflow));
        if capacity == 0 {
            return None;
        }
        let content_height = wrapped.lines.len().min(usize::from(capacity)) as u16;
        let height = content_height + u16::from(overflow) + 2;
        let popup = Rect::new(area.x + (area.width - width) / 2, area.y, width, height);
        let body = Floating::new("", width, height, &[])
            .at_top()
            .colors(self.color, self.color)
            .render(frame, popup, theme)
            .body;
        let alignment = if wrapped.lines.len() == 1 {
            Alignment::Center
        } else {
            Alignment::Left
        };
        let text = Rect::new(body.x + inset, body.y, body_width, content_height);
        frame.render_widget(
            Paragraph::new(
                wrapped
                    .lines
                    .iter()
                    .take(usize::from(content_height))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .alignment(alignment),
            text,
        );
        if overflow {
            frame.render_widget(
                Paragraph::new(truncate_display("… F2 details", usize::from(body.width)))
                    .style(Style::default().fg(theme.muted()))
                    .alignment(Alignment::Right),
                Rect::new(body.x, body.bottom() - 1, body.width, 1),
            );
        }
        Some(popup)
    }
}

impl WrappedMessage {
    fn get<'a>(cache: &'a mut Option<Self>, message: &Line<'_>, width: u16) -> &'a Self {
        if cache.as_ref().is_none_or(|wrapped| wrapped.width != width) {
            *cache = Some(Self {
                width,
                lines: styled_wrap(message, width),
            });
        }
        cache.as_ref().expect("message wrapping is prepared")
    }
}

pub(super) enum NotificationDetailsEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum NotificationDetailsEffect {
    Dismiss,
    Copy(String),
}

pub(super) struct NotificationDetails {
    message: Line<'static>,
    color: Color,
    safe_text: String,
    wrapped: Option<WrappedMessage>,
    scroll: usize,
    max_scroll: usize,
    body: Rect,
    page_height: usize,
}

impl NotificationDetails {
    pub(super) fn new(notification: &Notification) -> Self {
        Self {
            message: notification.message.clone(),
            color: notification.color,
            safe_text: sanitize_terminal_text(&message_text(&notification.message)).into_owned(),
            wrapped: None,
            scroll: 0,
            max_scroll: 0,
            body: Rect::default(),
            page_height: 0,
        }
    }
}

impl Component for NotificationDetails {
    type Event = NotificationDetailsEvent;
    type Effect = NotificationDetailsEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            NotificationDetailsEvent::Terminal(Event::Key(key))
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                match key.code {
                    KeyCode::Esc => {
                        return ComponentUpdate {
                            effects: vec![NotificationDetailsEffect::Dismiss],
                            render: RenderRequest::Immediate,
                        };
                    }
                    KeyCode::Char('c' | 'C')
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        return ComponentUpdate {
                            effects: vec![NotificationDetailsEffect::Copy(self.safe_text.clone())],
                            render: RenderRequest::Immediate,
                        };
                    }
                    KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                    KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_sub(self.page_height.max(1))
                    }
                    KeyCode::PageDown => {
                        self.scroll = self.scroll.saturating_add(self.page_height.max(1))
                    }
                    KeyCode::Home => self.scroll = 0,
                    KeyCode::End => self.scroll = self.max_scroll,
                    _ => return ComponentUpdate::none(),
                }
            }
            NotificationDetailsEvent::Terminal(Event::Mouse(mouse))
                if self.body.contains(Position::new(mouse.column, mouse.row)) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(1),
                    MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(1),
                    _ => return ComponentUpdate::none(),
                }
            }
            _ => return ComponentUpdate::none(),
        }
        self.scroll = self.scroll.min(self.max_scroll);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let width = area.width.min(72);
        let inset = if width > 6 { 2 } else { 0 };
        let text_width = width.saturating_sub(2 + inset * 2);
        let wrapped = WrappedMessage::get(&mut self.wrapped, &self.message, text_width);
        let height = wrapped
            .lines
            .len()
            .saturating_add(5)
            .max(7)
            .min(usize::from(u16::MAX)) as u16;
        self.body = Floating::new("Notification", 72, height, &DETAIL_KEYS)
            .colors(self.color, self.color)
            .render(frame, area, theme)
            .body;
        self.page_height = usize::from(self.body.height.saturating_sub(2));
        self.max_scroll = wrapped.lines.len().saturating_sub(self.page_height);
        self.scroll = self.scroll.min(self.max_scroll);
        let text = Rect::new(
            self.body.x + inset,
            self.body.y,
            text_width,
            self.page_height as u16,
        )
        .intersection(self.body);
        frame.render_widget(
            Paragraph::new(
                wrapped
                    .lines
                    .iter()
                    .skip(self.scroll)
                    .take(self.page_height)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            text,
        );
        if !self.body.is_empty() {
            let position = if self.page_height == 0 || wrapped.lines.is_empty() {
                format!("{} lines · C copy", wrapped.lines.len())
            } else {
                format!(
                    "{}–{} / {}",
                    self.scroll + 1,
                    (self.scroll + self.page_height).min(wrapped.lines.len()),
                    wrapped.lines.len()
                )
            };
            frame.render_widget(
                Paragraph::new(truncate_display(&position, usize::from(self.body.width)))
                    .style(Style::default().fg(theme.muted()))
                    .alignment(Alignment::Right),
                Rect::new(self.body.x, self.body.bottom() - 1, self.body.width, 1),
            );
        }
    }
}

fn message_text(message: &Line<'_>) -> String {
    // Line and Span's Display implementations omit embedded newlines.
    message
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

struct StyledGlyph {
    text: String,
    style: Style,
}

fn styled_wrap(message: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let raw = message_text(message);
    let mut ranges = Vec::with_capacity(message.spans.len());
    let mut end = 0;
    for span in &message.spans {
        end += span.content.len();
        ranges.push((end, message.style.patch(span.style)));
    }
    let mut span_index = 0;
    let mut safe_text = String::new();
    let mut safe_ranges = Vec::new();
    for (index, grapheme) in raw.grapheme_indices(true) {
        while ranges.get(span_index).is_some_and(|(end, _)| index >= *end) {
            span_index += 1;
        }
        let style = ranges
            .get(span_index)
            .map_or(message.style, |(_, style)| *style);
        safe_text.push_str(&sanitize_terminal_text(grapheme));
        safe_ranges.push((safe_text.len(), style));
    }
    let mut range_index = 0;
    let glyphs = safe_text
        .grapheme_indices(true)
        .map(|(index, text)| {
            while safe_ranges
                .get(range_index)
                .is_some_and(|(end, _)| index >= *end)
            {
                range_index += 1;
            }
            let style = safe_ranges
                .get(range_index)
                .map_or(message.style, |(_, style)| *style);
            StyledGlyph {
                text: text.to_owned(),
                style,
            }
        })
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for paragraph in glyphs.split(|glyph| glyph.text == "\n") {
        let text = paragraph
            .iter()
            .map(|glyph| glyph.text.as_str())
            .collect::<String>();
        let mut glyph_index = 0;
        for row in wrap_display_lines(&text, usize::from(width)) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for grapheme in row.graphemes(true) {
                let style = paragraph[glyph_index].style;
                glyph_index += 1;
                if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
                    last.content.to_mut().push_str(grapheme);
                } else {
                    spans.push(Span::styled(grapheme.to_owned(), style));
                }
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseButton, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> NotificationDetailsEvent {
        NotificationDetailsEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn render_details(details: &mut NotificationDetails, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| details.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn lifetime_counts_only_visible_time_without_a_polling_deadline() {
        let now = Instant::now();
        let mut notice = Notification::plain("Saved.".into(), Color::Green);
        assert_eq!(notice.expires_at(), None);
        assert!(!notice.expired(now + Duration::from_secs(1000)));
        notice.visibility(true, now);
        assert_eq!(notice.expires_at(), Some(now + DISPLAY_DURATION));
        notice.visibility(true, now + Duration::from_secs(3));
        assert_eq!(notice.deadline, now + DISPLAY_DURATION);
        notice.visibility(false, now + Duration::from_secs(4));
        assert_eq!(notice.expires_at(), None);
        notice.visibility(false, now + Duration::from_secs(100));
        assert!(!notice.expired(now + Duration::from_secs(1000)));
        notice.visibility(true, now + Duration::from_secs(100));
        assert_eq!(notice.expires_at(), Some(now + Duration::from_secs(106)));
        assert!(!notice.expired(now + Duration::from_secs(105)));
        assert!(notice.expired(now + Duration::from_secs(106)));
        notice.visibility(false, now + Duration::from_secs(107));
        assert!(notice.expired(now + Duration::from_secs(108)));
        assert_eq!(notice.expires_at(), None);
    }

    #[test]
    fn wrapping_preserves_the_version_emphasis_and_reset_command_color() {
        let notice = Notification::update_available(Version::new(1, 2, 3));
        for width in 1..48 {
            let wrapped = styled_wrap(&notice.message, width);
            let command = wrapped
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.style.fg == Some(Color::Reset))
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert_eq!(command, "`orvek update`");
            let bold = wrapped
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert_eq!(bold, "v1.2.3");
            assert!(
                wrapped
                    .iter()
                    .all(|line| line.width() <= usize::from(width))
            );
        }
    }

    #[test]
    fn word_wrap_and_unicode_sanitization_preserve_every_displayed_character() {
        let samples = [
            "one two three",
            "  indented\n\ntrailing  ",
            "漢字 e\u{301} 👨‍👩‍👧‍👦 end",
            "before\r\nafter\tend\u{1b}\u{301}!",
            "\n",
            "",
        ];
        assert_eq!(
            styled_wrap(&Line::from(Span::raw(samples[0])), 8)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["one two ", "three"]
        );
        for sample in samples {
            for width in 1..36 {
                let rows = styled_wrap(&Line::from(Span::raw(sample)), width)
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>();
                assert_eq!(
                    rows,
                    wrap_display_lines(sample, usize::from(width)),
                    "width={width}, sample={sample:?}"
                );
                assert!(rows.iter().all(|line| line.width() <= usize::from(width)));
            }
        }
    }

    #[test]
    fn grapheme_clusters_crossing_style_boundaries_are_never_split() {
        let line = Line::from(vec![
            Span::styled("e", Style::default().fg(Color::Green)),
            Span::styled("\u{301}界", Style::default().fg(Color::Red)),
        ]);
        let wrapped = styled_wrap(&line, 2);
        assert_eq!(wrapped[0].to_string(), "e\u{301}");
        assert_eq!(wrapped[0].spans[0].style.fg, Some(Color::Green));
        assert_eq!(wrapped[1].to_string(), "界");
        assert_eq!(wrapped[1].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn banner_is_bounded_by_the_transcript_and_offers_details_for_overflow() {
        let mut notice = Notification::plain(
            "A long notice with meaningful text. ".repeat(12),
            Color::Red,
        );
        let area = Rect::new(9, 3, 70, 12);
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        let mut popup = None;
        terminal
            .draw(|frame| {
                for cell in &mut frame.buffer_mut().content {
                    cell.set_symbol("·");
                }
                popup = notice.render(frame, area, &Theme::default());
            })
            .unwrap();
        let popup = popup.unwrap();
        assert_eq!(popup.y, area.y);
        assert_eq!(popup.width, 64);
        assert!(popup.height <= 6);
        assert_eq!(popup.intersection(area), popup);
        let buffer = terminal.backend().buffer();
        for y in 0..24 {
            for x in 0..90 {
                if !popup.contains(Position::new(x, y)) {
                    assert_eq!(buffer[(x, y)].symbol(), "·");
                }
            }
        }
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("F2 details"));
        assert_eq!(notice.expires_at(), None);
    }

    #[test]
    fn short_notice_is_centered_and_preserves_its_native_color() {
        let mut notice = Notification::plain("Saved.".into(), Color::Green);
        let mut terminal = Terminal::new(TestBackend::new(30, 8)).unwrap();
        let mut popup = None;
        terminal
            .draw(|frame| popup = notice.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let popup = popup.unwrap();
        let buffer = terminal.backend().buffer();
        let start = popup.x + (popup.width - 6) / 2;
        assert_eq!(buffer[(start, popup.y + 1)].symbol(), "S");
        assert_eq!(buffer[(start, popup.y + 1)].fg, Color::Green);
        assert!(
            buffer[(start, popup.y + 1)]
                .modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn tiny_rectangles_never_paint_outside_the_transcript() {
        let mut notice = Notification::plain(
            "漢字 sample notice with a long explanation".into(),
            Color::Yellow,
        );
        for width in 0..20 {
            for height in 0..10 {
                let area = Rect::new(5, 3, width, height);
                let mut terminal = Terminal::new(TestBackend::new(30, 16)).unwrap();
                terminal
                    .draw(|frame| {
                        for cell in &mut frame.buffer_mut().content {
                            cell.set_symbol("·");
                        }
                        if let Some(popup) = notice.render(frame, area, &Theme::default()) {
                            assert_eq!(popup.intersection(area), popup);
                            assert!(popup.height <= 6);
                        }
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                for y in 0..16 {
                    for x in 0..30 {
                        if !area.contains(Position::new(x, y)) {
                            assert_eq!(buffer[(x, y)].symbol(), "·");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn details_retain_the_opened_snapshot_and_copy_the_complete_safe_message() {
        let raw = format!("  first\n\n{}\nfinal line\u{1b}", "line\n".repeat(40));
        let mut pending = Notification::plain(raw.clone(), Color::Red);
        let mut details = NotificationDetails::new(&pending);
        pending = Notification::plain("New pending notice.".into(), Color::Green);
        assert_eq!(pending.message.to_string(), "New pending notice.");
        render_details(&mut details, 32, 12);
        assert!(details.max_scroll > 0);
        details.update(key(KeyCode::End));
        let text = render_details(&mut details, 32, 12);
        assert!(text.contains("final line�"));
        assert_eq!(
            details.update(key(KeyCode::Char('c'))).effects,
            [NotificationDetailsEffect::Copy(
                sanitize_terminal_text(&raw).into_owned()
            )]
        );
        assert_eq!(details.color, Color::Red);
        assert!(!details.safe_text.contains('\u{1b}'));
        details.update(key(KeyCode::Home));
        assert!(render_details(&mut details, 72, 16).contains("  first"));
    }

    #[test]
    fn detail_keys_and_wheel_scroll_without_submitting_or_running_actions() {
        let notice = Notification::plain("explanation\n".repeat(40), Color::Yellow);
        let mut details = NotificationDetails::new(&notice);
        render_details(&mut details, 40, 12);
        details.update(key(KeyCode::PageDown));
        assert_eq!(details.scroll, details.page_height);
        details.update(key(KeyCode::PageUp));
        assert_eq!(details.scroll, 0);
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: details.body.x,
            row: details.body.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(
            details
                .update(NotificationDetailsEvent::Terminal(Event::Mouse(mouse)))
                .effects
                .is_empty()
        );
        assert_eq!(details.scroll, 1);
        for code in [KeyCode::Enter, KeyCode::Tab, KeyCode::Char('o')] {
            assert!(details.update(key(code)).effects.is_empty());
        }
        assert!(
            details
                .update(NotificationDetailsEvent::Terminal(Event::Paste(
                    "ignored".into()
                )))
                .effects
                .is_empty()
        );
        assert!(
            details
                .update(NotificationDetailsEvent::Terminal(Event::Mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        ..mouse
                    }
                )))
                .effects
                .is_empty()
        );
        let mut release = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(
            details
                .update(NotificationDetailsEvent::Terminal(Event::Key(release)))
                .effects
                .is_empty()
        );
        assert!(
            details
                .update(NotificationDetailsEvent::Terminal(Event::Key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
                )))
                .effects
                .is_empty()
        );
        assert_eq!(
            details.update(key(KeyCode::Esc)).effects,
            [NotificationDetailsEffect::Dismiss]
        );
    }

    #[test]
    fn detail_resize_clamps_scroll_and_all_tiny_rectangles_remain_safe() {
        let notice = Notification::plain("漢字 explanation ".repeat(40), Color::Red);
        let mut details = NotificationDetails::new(&notice);
        render_details(&mut details, 30, 10);
        details.update(key(KeyCode::End));
        for width in 0..25 {
            for height in 0..14 {
                render_details(&mut details, width, height);
                assert!(details.scroll <= details.max_scroll);
            }
        }
        render_details(&mut details, 100, 40);
        assert!(details.scroll <= details.max_scroll);
    }
}
