//! Animated linear selector for the model fixed to a new session.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::theme::Theme;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use nanocodex::Model;
use ratatui::{
    Frame,
    layout::{Alignment, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::{Duration, Instant};

const MODELS: [Model; 3] = [Model::Luna, Model::Terra, Model::Sol];
const ANIMATION_DURATION: Duration = Duration::from_millis(180);
const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const KEY_BINDINGS: [(&str, &str); 3] = [("←/→", "model"), ("enter", "apply"), ("esc", "cancel")];

pub(super) enum ModelSelectorEvent {
    Terminal { event: Event, now: Instant },
    AnimationFrame(Instant),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ModelSelectorEffect {
    Apply(Model),
    Dismiss,
}

pub(super) struct ModelSelector {
    selected: usize,
    current: Model,
    motion_enabled: bool,
    targets: [Rect; 3],
    displayed_position: f64,
    animation: Option<Animation>,
}

struct Animation {
    from: f64,
    to: f64,
    started_at: Instant,
    next_frame: Instant,
}

impl ModelSelector {
    pub(super) fn new(initial: Model) -> Self {
        let selected = model_index(initial);
        Self {
            selected,
            current: initial,
            motion_enabled: true,
            targets: [Rect::default(); 3],
            displayed_position: selected as f64,
            animation: None,
        }
    }

    pub(super) fn set_motion_enabled(&mut self, enabled: bool) {
        self.motion_enabled = enabled;
        if !enabled {
            self.displayed_position = self.selected as f64;
            self.animation = None;
        }
    }

    fn update_mouse(
        &mut self,
        mouse: MouseEvent,
        now: Instant,
    ) -> ComponentUpdate<ModelSelectorEffect> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return ComponentUpdate::none();
        }
        let Some(index) = self
            .targets
            .iter()
            .position(|target| target.contains(Position::new(mouse.column, mouse.row)))
        else {
            return ComponentUpdate::none();
        };
        self.select_relative(index as isize - self.selected as isize, now)
    }

    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        self.animation
            .as_ref()
            .map(|animation| animation.next_frame)
    }

    fn update_key(&mut self, key: KeyEvent, now: Instant) -> ComponentUpdate<ModelSelectorEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::Left | KeyCode::Up => self.select_relative(-1, now),
            KeyCode::Right | KeyCode::Down => self.select_relative(1, now),
            KeyCode::Enter => ComponentUpdate {
                effects: vec![ModelSelectorEffect::Apply(MODELS[self.selected])],
                render: RenderRequest::Immediate,
            },
            KeyCode::Esc | KeyCode::Backspace => ComponentUpdate {
                effects: vec![ModelSelectorEffect::Dismiss],
                render: RenderRequest::Immediate,
            },
            _ => ComponentUpdate::none(),
        }
    }

    fn select_relative(
        &mut self,
        direction: isize,
        now: Instant,
    ) -> ComponentUpdate<ModelSelectorEffect> {
        self.advance_animation(now);
        let next = self
            .selected
            .saturating_add_signed(direction)
            .min(MODELS.len() - 1);
        if next == self.selected {
            return ComponentUpdate::none();
        }
        self.selected = next;
        if !self.motion_enabled {
            self.displayed_position = next as f64;
            self.animation = None;
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        self.animation = Some(Animation {
            from: self.displayed_position,
            to: next as f64,
            started_at: now,
            next_frame: now + ANIMATION_FRAME_INTERVAL,
        });
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn advance_animation(&mut self, now: Instant) -> bool {
        let Some(animation) = &mut self.animation else {
            return false;
        };
        let elapsed = now.saturating_duration_since(animation.started_at);
        let progress = (elapsed.as_secs_f64() / ANIMATION_DURATION.as_secs_f64()).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.displayed_position = animation.from + (animation.to - animation.from) * eased;
        if progress >= 1.0 {
            self.displayed_position = self.selected as f64;
            self.animation = None;
        } else {
            animation.next_frame = now + ANIMATION_FRAME_INTERVAL;
        }
        true
    }

    fn render_slider(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 14 || area.height < 2 {
            return;
        }
        let left = area.x.saturating_add(2);
        let right = area.right().saturating_sub(3).max(left);
        let width = right.saturating_sub(left);
        let indicator_column = left.saturating_add(
            (f64::from(width) * self.displayed_position / (MODELS.len() - 1) as f64).round() as u16,
        );
        let selected_color = theme.model(MODELS[self.selected]);
        let buffer = frame.buffer_mut();
        for column in left..=right {
            buffer.set_string(column, area.y, "─", Style::default().fg(theme.border()));
        }
        for index in 0..MODELS.len() {
            let column = model_column(left, width, index);
            buffer.set_string(column, area.y, "●", Style::default().fg(theme.muted()));
        }
        buffer.set_string(
            indicator_column,
            area.y,
            "◆",
            Style::default()
                .fg(selected_color)
                .add_modifier(Modifier::BOLD),
        );

        let labels = [
            (model_column(left, width, 0), Model::Luna, "Luna"),
            (model_column(left, width, 1), Model::Terra, "Terra"),
            (model_column(left, width, 2), Model::Sol, "Sol"),
        ];
        for (index, (column, model, label)) in labels.into_iter().enumerate() {
            let label_width = u16::try_from(label.len()).unwrap_or(u16::MAX);
            let start = column.saturating_sub(label_width / 2).max(area.x);
            self.targets[index] = Rect::new(
                start.saturating_sub(1).max(area.x),
                area.y,
                label_width + 2,
                2,
            )
            .intersection(area);
            buffer.set_string(
                start,
                area.y.saturating_add(1),
                label,
                Style::default().fg(theme.model(model)),
            );
        }
    }
}

fn model_column(left: u16, width: u16, index: usize) -> u16 {
    left.saturating_add(
        (f64::from(width) * index as f64 / (MODELS.len() - 1) as f64).round() as u16,
    )
}

impl Component for ModelSelector {
    type Event = ModelSelectorEvent;
    type Effect = ModelSelectorEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            ModelSelectorEvent::Terminal {
                event: Event::Key(key),
                now,
            } => self.update_key(key, now),
            ModelSelectorEvent::Terminal {
                event: Event::Mouse(mouse),
                now,
            } => self.update_mouse(mouse, now),
            ModelSelectorEvent::Terminal { .. } => ComponentUpdate::none(),
            ModelSelectorEvent::AnimationFrame(now) => {
                if self.advance_animation(now) {
                    ComponentUpdate::render(RenderRequest::Streaming)
                } else {
                    ComponentUpdate::none()
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.targets = [Rect::default(); 3];
        let layout = Floating::new("Select model", 52, 9, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let model = MODELS[self.selected];
        let title = Line::from(vec![
            Span::styled("Selected: ", Style::default().fg(theme.border())),
            Span::styled(
                model_name(model),
                Style::default()
                    .fg(theme.model(model))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(title).alignment(Alignment::Center),
            Rect {
                height: 1,
                ..layout.body
            },
        );
        let slider_offset = if layout.body.height >= 5 { 3 } else { 1 };
        if layout.body.height >= 5 {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Current: ", Style::default().fg(theme.muted())),
                    Span::styled(
                        model_name(self.current),
                        Style::default().fg(theme.model(self.current)),
                    ),
                ]))
                .alignment(Alignment::Center),
                Rect::new(layout.body.x, layout.body.y + 1, layout.body.width, 1),
            );
        }
        self.render_slider(
            frame,
            Rect {
                y: layout.body.y.saturating_add(slider_offset),
                height: 2,
                ..layout.body
            }
            .intersection(layout.body),
            theme,
        );
    }
}

fn model_index(model: Model) -> usize {
    MODELS
        .iter()
        .position(|candidate| *candidate == model)
        .unwrap_or(2)
}

fn model_name(model: Model) -> &'static str {
    match model {
        Model::Luna => "Luna",
        Model::Terra => "Terra",
        Model::Sol => "Sol",
        _ => "Sol",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render(selector: &mut ModelSelector) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(60, 9)).unwrap();
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
    }

    fn rendered_label_color(selector: &mut ModelSelector, label: &str) -> Color {
        let terminal = render(selector);
        let buffer = terminal.backend().buffer();
        let label = label.chars().collect::<Vec<_>>();
        let label_width = u16::try_from(label.len()).unwrap();
        for y in 0..buffer.area.height {
            for x in 0..=buffer.area.width.saturating_sub(label_width) {
                if label.iter().enumerate().all(|(offset, character)| {
                    buffer[(x + u16::try_from(offset).unwrap(), y)].symbol()
                        == character.to_string()
                }) {
                    return buffer[(x, y)].fg;
                }
            }
        }
        panic!("label not rendered: {label:?}");
    }

    fn rendered_stop_colors(selector: &mut ModelSelector) -> Vec<Color> {
        render(selector)
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() == "●")
            .map(|cell| cell.fg)
            .collect()
    }

    #[test]
    fn terra_label_is_centered_under_the_middle_stop() {
        let terminal = render(&mut ModelSelector::new(Model::Terra));
        let buffer = terminal.backend().buffer();
        let stop = buffer
            .content
            .iter()
            .position(|cell| cell.symbol() == "◆")
            .unwrap();
        let width = usize::from(buffer.area.width);
        let label_row = stop / width + 1;
        let terra = buffer.content[label_row * width..(label_row + 1) * width]
            .windows(5)
            .position(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>() == "Terra")
            .unwrap();

        assert_eq!(terra + 2, stop % width);
    }

    #[test]
    fn selection_moves_linearly_and_does_not_wrap() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Sol);

        selector.update_key(key(KeyCode::Right), now);
        assert_eq!(selector.selected, 2);
        selector.update_key(key(KeyCode::Left), now);
        assert_eq!(selector.selected, 1);
        selector.update_key(key(KeyCode::Left), now);
        selector.update_key(key(KeyCode::Left), now);
        assert_eq!(selector.selected, 0);
    }

    #[test]
    fn every_stop_keeps_its_model_color() {
        let mut selector = ModelSelector::new(Model::Sol);

        assert_eq!(rendered_label_color(&mut selector, "Luna"), Color::White);
        assert_eq!(rendered_label_color(&mut selector, "Terra"), Color::Green);
        assert_eq!(rendered_label_color(&mut selector, "Sol"), Color::Yellow);
    }

    #[test]
    fn rail_is_neutral_while_selection_keeps_the_model_color() {
        let mut selector = ModelSelector::new(Model::Sol);
        let terminal = render(&mut selector);
        let rail = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() == "─")
            .collect::<Vec<_>>();

        assert!(!rail.is_empty());
        assert!(rail.iter().all(|cell| cell.fg == Theme::default().border()));
    }

    #[test]
    fn unselected_stops_remain_muted() {
        assert_eq!(
            rendered_stop_colors(&mut ModelSelector::new(Model::Luna)),
            [Color::DarkGray, Color::DarkGray]
        );
        assert_eq!(
            rendered_stop_colors(&mut ModelSelector::new(Model::Terra)),
            [Color::DarkGray, Color::DarkGray]
        );
        assert_eq!(
            rendered_stop_colors(&mut ModelSelector::new(Model::Sol)),
            [Color::DarkGray, Color::DarkGray]
        );
    }

    #[test]
    fn title_does_not_describe_the_model_order() {
        let terminal = render(&mut ModelSelector::new(Model::Terra));
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(!rendered.contains("smarter"));
    }

    #[test]
    fn narrow_selector_does_not_overwrite_wrapped_menu_help() {
        let mut terminal = Terminal::new(TestBackend::new(30, 7)).unwrap();
        terminal
            .draw(|frame| {
                ModelSelector::new(Model::Sol).render(frame, frame.area(), &Theme::default());
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("←/→ model"));
        assert!(rendered.contains("enter apply"));
        assert!(rendered.contains("esc cancel"));
        assert!(rendered.contains('◆'));
        assert!(rendered.contains("Sol"));
        assert_eq!(terminal.backend().buffer()[(0, 6)].symbol(), "╰");
        assert_eq!(terminal.backend().buffer()[(29, 6)].symbol(), "╯");
    }

    #[test]
    fn applying_returns_the_selected_model() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Sol);
        selector.update_key(key(KeyCode::Left), now);

        let update = selector.update_key(key(KeyCode::Enter), now);

        assert_eq!(update.effects, [ModelSelectorEffect::Apply(Model::Terra)]);
    }

    #[test]
    fn animation_reaches_the_selected_stop() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Luna);
        selector.update_key(key(KeyCode::Right), now);
        assert!(selector.animation_deadline().is_some());

        selector.update(ModelSelectorEvent::AnimationFrame(now + ANIMATION_DURATION));

        assert_eq!(selector.displayed_position, 1.0);
        assert!(selector.animation_deadline().is_none());
    }
    #[test]
    fn click_changes_pending_choice_and_motion_can_snap_without_a_deadline() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Luna);
        selector.set_motion_enabled(false);
        let mut terminal = render(&mut selector);
        let target = selector.targets[2];
        let update = selector.update(ModelSelectorEvent::Terminal {
            now,
            event: Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: target.x,
                row: target.y,
                modifiers: KeyModifiers::NONE,
            }),
        });
        assert!(update.effects.is_empty());
        assert_eq!(selector.animation_deadline(), None);
        assert_eq!(selector.displayed_position, 2.0);
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Selected: Sol"));
        assert!(text.contains("Current: Luna"));
        assert_eq!(
            selector.update_key(key(KeyCode::Enter), now).effects,
            [ModelSelectorEffect::Apply(Model::Sol)]
        );
    }

    #[test]
    fn tiny_rectangles_do_not_write_outside_the_selector() {
        for width in 0..18 {
            for height in 0..10 {
                let mut selector = ModelSelector::new(Model::Terra);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
                    .unwrap();
            }
        }
    }
}
