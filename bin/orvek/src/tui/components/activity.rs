//! Shared task-state colors and animation styling.

use super::waved_text::wave_style;
use crate::tui::theme::Theme;
use ratatui::{
    layout::{Position, Rect},
    style::{Color, Style},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ActivityState {
    #[default]
    Idle,
    Thinking,
    Working,
    Complete,
    Error,
    Cancelled,
}

impl ActivityState {
    pub(crate) const fn active(self) -> bool {
        matches!(self, Self::Thinking | Self::Working)
    }

    pub(crate) const fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Idle => theme.muted(),
            Self::Thinking => theme.thinking_medium(),
            Self::Working => theme.accent(),
            Self::Complete => theme.success(),
            Self::Error => theme.error(),
            Self::Cancelled => theme.cancelled(),
        }
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

    #[cfg(test)]
    pub(crate) const fn state(self) -> ActivityState {
        self.state
    }

    pub(crate) const fn color(self, theme: &Theme) -> Color {
        self.state.color(theme)
    }

    pub(crate) fn line_symbol(self, area: Rect, position: Position) -> &'static str {
        if perimeter_position(area, position).is_some() {
            "·"
        } else {
            " "
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
        let Some((index, _)) = perimeter_position(area, position) else {
            return style;
        };
        wave_style(theme.brand_secondary(), index, self.frame)
    }
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
    use super::{ActivityState, ActivityVisual};
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
            ActivityState::Complete,
            ActivityState::Error,
            ActivityState::Cancelled,
        ]
        .map(|state| state.color(&theme));
        assert_eq!(colors.into_iter().collect::<HashSet<Color>>().len(), 6);
    }

    #[test]
    fn pixel_outline_is_continuous_and_uses_only_small_blocks() {
        let area = Rect::new(0, 0, 24, 5);
        let visual = ActivityVisual::new(ActivityState::Working, 0, true);
        let top = (0..area.width)
            .map(|x| visual.line_symbol(area, Position::new(x, 0)))
            .collect::<String>();

        assert_eq!(top, "·".repeat(usize::from(area.width)));
        assert!(!top.contains([' ', '■', '─']));
    }

    #[test]
    fn active_outline_uses_the_sky_blue_theme_wave_without_bold_cells() {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 24, 5);
        let colors = |frame| {
            let visual = ActivityVisual::new(ActivityState::Working, frame, true);
            (0..area.width)
                .map(|x| {
                    visual
                        .line_style(&theme, area, Position::new(x, 0), theme.border())
                        .fg
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };

        let first = colors(0);
        let second = colors(1);
        assert_ne!(first, second);
        assert_eq!(first[0], theme.brand_secondary());
        assert!(first.iter().collect::<HashSet<_>>().len() >= 6);
        let visual = ActivityVisual::new(ActivityState::Working, 8, true);
        assert!((0..area.width).all(|x| {
            !visual
                .line_style(&theme, area, Position::new(x, 0), theme.border())
                .add_modifier
                .intersects(Modifier::BOLD | Modifier::DIM)
        }));
    }

    #[test]
    fn settled_outline_is_continuous_and_has_no_highlight() {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 24, 5);
        let visual = ActivityVisual::new(ActivityState::Idle, 0, false);
        assert!((0..area.width).all(|x| {
            let position = Position::new(x, 0);
            visual.line_symbol(area, position) == "·"
                && !visual
                    .line_style(&theme, area, position, theme.border())
                    .add_modifier
                    .intersects(Modifier::BOLD | Modifier::DIM)
        }));
    }
}
