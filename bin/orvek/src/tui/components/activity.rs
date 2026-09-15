//! Shared task-state colors and animation styling.

use crate::tui::theme::Theme;
use ratatui::{
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
};

const CYCLE_FRAMES: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ActivityState {
    #[default]
    Idle,
    Thinking,
    Working,
    #[allow(dead_code)]
    Compacting,
    Complete,
    Error,
    Cancelled,
}

impl ActivityState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Ready",
            Self::Thinking => "Thinking",
            Self::Working => "Working",
            Self::Compacting => "Compacting",
            Self::Complete => "Complete",
            Self::Error => "Error",
            Self::Cancelled => "Cancelled",
        }
    }

    pub(crate) const fn active(self) -> bool {
        matches!(self, Self::Thinking | Self::Working | Self::Compacting)
    }

    pub(crate) const fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Idle => theme.muted(),
            Self::Thinking => theme.thinking_medium(),
            Self::Working => theme.accent(),
            Self::Compacting => theme.thinking_high(),
            Self::Complete => Color::Green,
            Self::Error => theme.thinking_xhigh(),
            Self::Cancelled => Color::Magenta,
        }
    }

    pub(crate) const fn compact(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (Self::Idle, false) => "○",
            (Self::Thinking, false) => "◌",
            (Self::Working, false) => "›",
            (Self::Compacting, false) => "↔",
            (Self::Complete, false) => "✓",
            (Self::Error, false) => "×",
            (Self::Cancelled, false) => "−",
            (Self::Idle, true) => "O",
            (Self::Thinking, true) => "*",
            (Self::Working, true) => ">",
            (Self::Compacting, true) => "=",
            (Self::Complete, true) => "+",
            (Self::Error, true) => "!",
            (Self::Cancelled, true) => "-",
        }
    }

    pub(crate) fn pixels(self, frame: usize, motion: bool) -> [[bool; 5]; 4] {
        let rows = match self {
            Self::Idle => [14, 17, 17, 14],
            Self::Thinking => [14, 16, 17, 14],
            Self::Working => [8, 4, 4, 8],
            Self::Compacting => [27, 17, 17, 27],
            Self::Complete => [1, 2, 20, 8],
            Self::Error => [17, 10, 10, 17],
            Self::Cancelled => [0, 31, 0, 0],
        };
        let mut pixels =
            std::array::from_fn(|y| std::array::from_fn(|x| rows[y] & (1 << (4 - x)) != 0));
        if !motion {
            return pixels;
        }
        match self {
            Self::Thinking => {
                let ring = [
                    (1, 0),
                    (2, 0),
                    (3, 0),
                    (4, 1),
                    (4, 2),
                    (3, 3),
                    (2, 3),
                    (1, 3),
                    (0, 2),
                    (0, 1),
                ];
                pixels = [[false; 5]; 4];
                let head = phase_position(frame, ring.len());
                for (index, (x, y)) in ring.into_iter().enumerate() {
                    pixels[y][x] = (index + 10 - head) % 10 >= 2;
                }
            }
            Self::Working => {
                pixels = [[false; 5]; 4];
                let position = phase_position(frame, 6);
                for (y, row) in pixels.iter_mut().enumerate() {
                    let x = position + usize::from(y == 1 || y == 2);
                    if x < 5 {
                        row[x] = true;
                    }
                }
            }
            Self::Compacting => {
                let edge = [0, 1, 2, 2, 1, 0][phase_position(frame, 6)];
                pixels =
                    std::array::from_fn(|_| std::array::from_fn(|x| x == edge || x == 4 - edge));
            }
            _ => {}
        }
        pixels
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ActivityVisual {
    state: ActivityState,
    frame: usize,
    animated: bool,
}

impl ActivityVisual {
    pub(crate) const fn new(state: ActivityState, frame: usize, animated: bool) -> Self {
        Self {
            state,
            frame,
            animated,
        }
    }

    pub(crate) const fn state(self) -> ActivityState {
        self.state
    }

    pub(crate) const fn color(self, theme: &Theme) -> Color {
        self.state.color(theme)
    }

    pub(crate) fn label_style(self, theme: &Theme) -> Style {
        let style = Style::default().fg(self.color(theme));
        if self.state == ActivityState::Idle {
            style
        } else {
            style.add_modifier(Modifier::BOLD)
        }
    }

    pub(crate) fn line_style(
        self,
        theme: &Theme,
        area: Rect,
        position: Position,
        idle_color: Color,
    ) -> Style {
        let color = if self.state == ActivityState::Idle {
            idle_color
        } else {
            self.color(theme)
        };
        let style = Style::default().fg(color);
        if !self.animated {
            return style;
        }
        let Some((index, length)) = perimeter_position(area, position) else {
            return style;
        };
        let head = phase_position(self.frame, length);
        let distance = index.abs_diff(head).min(length - index.abs_diff(head));
        match distance {
            0..=1 => style.add_modifier(Modifier::BOLD),
            2..=4 => style,
            _ => style.add_modifier(Modifier::DIM),
        }
    }
}

fn phase_position(frame: usize, length: usize) -> usize {
    (frame % CYCLE_FRAMES) * length / CYCLE_FRAMES
}

fn perimeter_position(area: Rect, position: Position) -> Option<(usize, usize)> {
    if area.width < 2 || area.height < 2 || !area.contains(position) {
        return None;
    }
    let x = usize::from(position.x - area.x);
    let y = usize::from(position.y - area.y);
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    let length = 2 * width + 2 * height - 4;
    let index = if y == 0 {
        x
    } else if x == width - 1 {
        width + y - 1
    } else if y == height - 1 {
        2 * width + height - 3 - x
    } else if x == 0 {
        2 * width + 2 * height - 4 - y
    } else {
        return None;
    };
    Some((index, length))
}

#[cfg(test)]
mod tests {
    use super::{ActivityState, ActivityVisual, perimeter_position};
    use crate::tui::theme::Theme;
    use ratatui::{
        layout::{Position, Rect},
        style::{Color, Modifier},
    };
    use std::collections::HashSet;

    #[test]
    fn every_state_has_a_distinct_reusable_color() {
        let theme = Theme::default();
        let colors = [
            ActivityState::Idle,
            ActivityState::Thinking,
            ActivityState::Working,
            ActivityState::Compacting,
            ActivityState::Complete,
            ActivityState::Error,
            ActivityState::Cancelled,
        ]
        .map(|state| state.color(&theme));
        assert_eq!(colors.into_iter().collect::<HashSet<Color>>().len(), 7);
    }

    #[test]
    fn perimeter_coordinates_form_one_closed_loop() {
        let area = Rect::new(3, 2, 8, 5);
        let mut indices = HashSet::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some((index, length)) = perimeter_position(area, Position::new(x, y)) {
                    assert_eq!(length, 22);
                    indices.insert(index);
                }
            }
        }
        assert_eq!(indices, (0..22).collect());
    }

    #[test]
    fn one_shared_frame_moves_the_glow_around_the_line_grid() {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 30, 5);
        let bright = |frame| {
            let visual = ActivityVisual::new(ActivityState::Thinking, frame, true);
            (0..area.width)
                .filter(|x| {
                    visual
                        .line_style(&theme, area, Position::new(*x, 0), theme.border())
                        .add_modifier
                        .contains(Modifier::BOLD)
                })
                .collect::<Vec<_>>()
        };
        assert_ne!(bright(0), bright(8));
    }
}
