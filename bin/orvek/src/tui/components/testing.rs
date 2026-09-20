//! Cross-component visual state checks.

use super::{
    node::Component,
    recent_prompt_picker::RecentPromptPicker,
    startup::StartupScreen,
    toast::{Toast, ToastStack},
};
use crate::tui::{
    session::RecentPrompt,
    theme::{FeedbackTone, Theme, ThemeMode},
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use std::{path::PathBuf, time::Instant};

#[derive(Clone, Copy)]
enum VisualState {
    Empty,
    Loading,
    Error,
    Populated,
}

fn no_color_theme(mode: ThemeMode) -> Theme {
    let fields = [
        "background",
        "text",
        "border",
        "muted",
        "overlay_shadow",
        "accent",
        "brand_primary",
        "brand_secondary",
        "code_text",
        "code_background",
        "success",
        "warning",
        "error",
        "cancelled",
        "thinking_low",
        "thinking_medium",
        "thinking_high",
        "thinking_xhigh",
        "thinking_max",
    ];
    let config = fields
        .into_iter()
        .map(|field| format!("{field} = \"reset\""))
        .collect::<Vec<_>>()
        .join("\n");
    let mut theme: Theme = toml::from_str(&config).unwrap();
    theme.set_mode(mode);
    theme
}

fn render_state(state: VisualState, width: u16, height: u16, theme: &Theme) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| match state {
            VisualState::Empty => RecentPromptPicker::new(Vec::new(), "current".to_owned()).render(
                frame,
                frame.area(),
                theme,
            ),
            VisualState::Loading => StartupScreen::new(Instant::now(), "Loading session").render(
                frame,
                frame.area(),
                theme,
            ),
            VisualState::Error => {
                let mut toasts = ToastStack::default();
                toasts.push(Toast::plain(
                    "matrix failure",
                    FeedbackTone::Error,
                    Instant::now(),
                ));
                toasts.render(frame, frame.area(), theme);
            }
            VisualState::Populated => RecentPromptPicker::new(
                vec![RecentPrompt {
                    text: "hello matrix".to_owned(),
                    recorded_at_unix_ms: 1,
                    session_id: "current".to_owned(),
                    workspace: PathBuf::from("/work"),
                }],
                "current".to_owned(),
            )
            .render(frame, frame.area(), theme),
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn symbols(buffer: &Buffer) -> String {
    buffer.content().iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn visual_state_matrix_covers_themes_color_policy_and_terminal_sizes() {
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        let mut colored = Theme::default();
        colored.set_mode(mode);
        for (theme, no_color) in [(&colored, false), (&no_color_theme(mode), true)] {
            for (width, height) in [(80, 24), (24, 8)] {
                for state in [
                    VisualState::Empty,
                    VisualState::Loading,
                    VisualState::Error,
                    VisualState::Populated,
                ] {
                    let buffer = render_state(state, width, height, theme);
                    assert!(!symbols(&buffer).trim().is_empty());
                    if no_color {
                        assert!(
                            buffer
                                .content()
                                .iter()
                                .all(|cell| { cell.fg == Color::Reset && cell.bg == Color::Reset })
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn standard_state_surfaces_keep_their_meaning() {
    let theme = Theme::default();
    for (state, expected) in [
        (VisualState::Empty, "No prompts in this scope"),
        (VisualState::Loading, "Loading session"),
        (VisualState::Error, "matrix failure"),
        (VisualState::Populated, "hello matrix"),
    ] {
        let rendered = symbols(&render_state(state, 80, 24, &theme));
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in {rendered:?}"
        );
    }
}
