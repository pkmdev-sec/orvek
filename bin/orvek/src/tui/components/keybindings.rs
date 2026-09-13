//! Styled global keyboard shortcut reference.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{format::wrap_display_lines, theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

const FOOTER: [(&str, &str); 2] = [("↑↓", "scroll"), ("esc", "close")];
const BINDINGS: [(&str, &str); 31] = [
    ("ctrl+s", "change reasoning effort"),
    ("ctrl+d", "select model · before first prompt"),
    ("ctrl+t", "fork session · when available"),
    ("ctrl+g", "edit prompt in $EDITOR"),
    ("ctrl+r", "recent prompts"),
    ("ctrl+z", "restore the last cleared draft"),
    ("ctrl/cmd+v", "paste clipboard image"),
    ("ctrl+o", "expand · collapse all tool calls"),
    (
        "ctrl+c",
        "clear input · when composer is focused and nonempty",
    ),
    ("ctrl+c ctrl+c", "split closes pane · else exit"),
    ("esc esc", "interrupt the active response"),
    ("enter", "submit prompt"),
    ("enter + enter", "submit prompt and steer"),
    ("shift+enter/ctrl+j", "insert newline"),
    ("ctrl+a/e", "move to line start · end"),
    ("ctrl+b/f", "move to previous · next character"),
    ("alt/option+b/f", "move to previous · next word"),
    ("alt/option+backspace", "delete previous word"),
    ("↑/↓ · ctrl+p/n", "move lines · prompt history at edge"),
    ("tab", "focus queue · when present"),
    ("/", "open actions · empty prompt only"),
    ("@", "insert workspace file"),
    ("@@", "mention another session"),
    ("$", "insert an available skill reference"),
    ("F2", "notification details"),
    ("!", "local shell command · prompt start"),
    ("mouse click/drag", "open links/tools · copy text"),
    (
        "picker click / double-click",
        "select / use · actions open on click",
    ),
    ("picker wheel", "move selection · scroll over a preview"),
    ("pgup/pgdn · wheel", "scroll transcript"),
    ("ctrl+home/end", "jump to start · follow latest"),
];

pub(super) enum KeybindingsEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum KeybindingsEffect {
    Dismiss,
}

#[derive(Default)]
pub(super) struct KeybindingsHelp {
    scroll: u16,
    max_scroll: u16,
    body: Rect,
}

impl Component for KeybindingsHelp {
    type Event = KeybindingsEvent;
    type Effect = KeybindingsEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            KeybindingsEvent::Terminal(Event::Key(key))
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                match key.code {
                    KeyCode::Esc => {
                        return ComponentUpdate {
                            effects: vec![KeybindingsEffect::Dismiss],
                            render: RenderRequest::Immediate,
                        };
                    }
                    KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                    KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_sub(self.body.height.max(1))
                    }
                    KeyCode::PageDown => {
                        self.scroll = self.scroll.saturating_add(self.body.height.max(1))
                    }
                    KeyCode::Home => self.scroll = 0,
                    KeyCode::End => self.scroll = self.max_scroll,
                    _ => return ComponentUpdate::none(),
                }
            }
            KeybindingsEvent::Terminal(Event::Mouse(mouse))
                if self.body.contains(Position::new(mouse.column, mouse.row)) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(1),
                    MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(1),
                    _ => return ComponentUpdate::none(),
                }
            }
            KeybindingsEvent::Terminal(_) => return ComponentUpdate::none(),
        }
        self.scroll = self.scroll.min(self.max_scroll);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let height = u16::try_from(BINDINGS.len())
            .unwrap_or(u16::MAX)
            .saturating_add(3);
        let layout =
            Floating::new("Keyboard shortcuts", 72, height, &FOOTER).render(frame, area, theme);
        self.body = layout.body;
        if layout.body.is_empty() {
            self.max_scroll = 0;
            return;
        }
        let lines = BINDINGS
            .iter()
            .flat_map(|&(key, description)| {
                binding_lines(key, description, layout.body.width, theme)
            })
            .collect::<Vec<_>>();
        self.max_scroll = lines
            .len()
            .saturating_sub(usize::from(layout.body.height))
            .min(usize::from(u16::MAX)) as u16;
        self.scroll = self.scroll.min(self.max_scroll);
        frame.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), layout.body);
    }
}

fn binding_lines(
    key: &'static str,
    description: &'static str,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let key_style = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let description_style = Style::default().fg(theme.muted());
    let occupied = 1 + key.width() + description.width();
    if occupied < usize::from(width) {
        return vec![Line::from(vec![
            Span::styled(format!(" {key}"), key_style),
            Span::raw(" ".repeat(usize::from(width) - occupied)),
            Span::styled(description, description_style),
        ])];
    }
    let mut lines = wrap_display_lines(&format!(" {key}"), usize::from(width))
        .into_iter()
        .map(|text| Line::styled(text, key_style))
        .collect::<Vec<_>>();
    let inset = if width > 2 { 2 } else { 0 };
    lines.extend(
        wrap_display_lines(description, usize::from(width.saturating_sub(inset)))
            .into_iter()
            .map(|text| {
                Line::styled(
                    format!("{}{text}", " ".repeat(usize::from(inset))),
                    description_style,
                )
            }),
    );
    lines
}

#[cfg(test)]
mod tests {
    use super::{BINDINGS, Component, KeybindingsEffect, KeybindingsEvent, KeybindingsHelp};
    use crate::tui::theme::Theme;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    #[test]
    fn popup_right_aligns_muted_descriptions() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();

        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content()
            .chunks(80)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let row = rendered
            .iter()
            .position(|line| line.contains("ctrl+s"))
            .expect("effort shortcut should render");
        for description in ["change reasoning effort", "paste clipboard image"] {
            let line = rendered
                .iter()
                .find(|line| line.contains(description))
                .expect("description should render");
            let start = line.find(description).unwrap();
            let end = unicode_width::UnicodeWidthStr::width(&line[..start])
                + unicode_width::UnicodeWidthStr::width(description);
            assert_eq!(end, 75);
        }
        assert_eq!(
            buffer[(5, u16::try_from(row).unwrap())].fg,
            Theme::default().accent()
        );
        assert_eq!(
            buffer[(74, u16::try_from(row).unwrap())].fg,
            Color::DarkGray
        );
    }

    #[test]
    fn popup_documents_context_sensitive_composer_shortcuts() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();

        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        for expected in [
            "ctrl/cmd+v",
            "ctrl+t",
            "fork session · when available",
            "ctrl+r",
            "ctrl+z",
            "ctrl+c ctrl+c",
            "clear input · when composer is focused and nonempty",
            "split closes pane · else exit",
            "enter + enter",
            "submit prompt and steer",
            "shift+enter/ctrl+j",
            "ctrl+a/e",
            "move to line start · end",
            "ctrl+b/f",
            "move to previous · next character",
            "alt/option+b/f",
            "move to previous · next word",
            "alt/option+backspace",
            "delete previous word",
            "ctrl+p/n",
            "prompt history at edge",
            "focus queue · when present",
            "open actions · empty prompt only",
            "insert workspace file",
            "local shell command · prompt start",
            "mouse click/drag",
            "pgup/pgdn · wheel",
            "scroll transcript",
            "ctrl+home/end",
            "jump to start · follow latest",
            "↑↓ scroll",
        ] {
            assert!(rendered.iter().any(|line| line.contains(expected)));
        }
    }

    #[test]
    fn compact_popup_scrolls_to_late_shortcuts() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        for _ in &BINDINGS {
            help.update(KeybindingsEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            ))));
            terminal
                .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
                .unwrap();
        }

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        for expected in [
            "local shell command · prompt start",
            "mouse click/drag",
            "scroll transcript",
            "jump to start · follow latest",
        ] {
            assert!(rendered.iter().any(|line| line.contains(expected)));
        }
    }

    #[test]
    fn narrow_terminals_do_not_overflow_the_popup() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(8, 4)).unwrap();

        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert_eq!(terminal.backend().buffer().area.width, 8);
    }

    #[test]
    fn escape_dismisses_the_popup() {
        let mut help = KeybindingsHelp::default();

        let update = help.update(KeybindingsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))));

        assert_eq!(update.effects, [KeybindingsEffect::Dismiss]);
    }
    #[test]
    fn long_keys_and_descriptions_wrap_without_losing_the_new_bindings() {
        use unicode_width::UnicodeWidthStr;
        for width in 1..40 {
            for (key, description) in BINDINGS {
                let lines = super::binding_lines(key, description, width, &Theme::default());
                assert!(
                    lines
                        .iter()
                        .all(|line| line.to_string().width() <= usize::from(width))
                );
            }
        }
        let narrow = super::binding_lines("key", "xyz", 1, &Theme::default())
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();
        assert!(narrow.ends_with("xyz"));
        for required in ["@@", "$", "F2", "picker wheel"] {
            assert!(BINDINGS.iter().any(|(key, _)| *key == required));
        }
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        help.update(KeybindingsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::End,
            KeyModifiers::NONE,
        ))));
        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("follow"));
        assert!(text.contains("latest"));
        let end = help.scroll;
        for _ in 0..20 {
            help.update(KeybindingsEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            ))));
        }
        assert_eq!(help.scroll, end);
    }
}
