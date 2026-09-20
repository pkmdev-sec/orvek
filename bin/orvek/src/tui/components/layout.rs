//! Shared geometry for the root TUI surface.

use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RootLayout {
    pub(super) content: Rect,
    pub(super) transcript: Rect,
    pub(super) queue: Rect,
    pub(super) composer: Rect,
    pub(super) composer_content: Rect,
}

impl RootLayout {
    pub(super) fn calculate(
        area: Rect,
        desired_composer_height: u16,
        desired_queue_height: u16,
    ) -> Self {
        let standard_layout = area.height >= 16 && area.width >= 32;
        let body = area;
        let composer_height = desired_composer_height.min(body.height);
        let composer = Rect {
            y: body.bottom().saturating_sub(composer_height),
            height: composer_height,
            ..body
        };
        let available_queue_height = body.height.saturating_sub(composer_height);
        let queue_height =
            desired_queue_height
                .min((body.height / 3).max(3))
                .min(if standard_layout {
                    available_queue_height.saturating_sub(10)
                } else {
                    available_queue_height
                });
        let queue_width = body.width.saturating_mul(95) / 100;
        let queue = Rect {
            x: body.x + body.width.saturating_sub(queue_width) / 2,
            y: composer.y.saturating_sub(queue_height),
            width: queue_width,
            height: queue_height,
        };
        let transcript = Rect {
            height: body
                .height
                .saturating_sub(composer_height)
                .saturating_sub(queue_height),
            ..body
        };
        let composer_content = if composer.width >= 2 && composer.height >= 3 {
            Rect::new(
                composer.x + 1,
                composer.y + 1,
                composer.width - 2,
                composer.height - 2,
            )
        } else {
            Rect {
                height: composer.height.min(1),
                ..composer
            }
        };

        Self {
            content: area,
            transcript,
            queue,
            composer,
            composer_content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RootLayout;
    use ratatui::layout::Rect;

    #[test]
    fn standard_layout_preserves_full_width_transcript_and_composer() {
        let area = Rect::new(0, 0, 80, 24);
        let layout = RootLayout::calculate(area, 5, 4);

        assert_eq!(layout.transcript.x, area.x);
        assert_eq!(layout.transcript.y, area.y);
        assert_eq!(layout.transcript.width, 80);
        assert_eq!(layout.composer.width, 80);
        assert_eq!(layout.composer.bottom(), area.bottom());
        assert_eq!(layout.transcript.bottom(), layout.queue.y);
        assert_eq!(layout.queue.bottom(), layout.composer.y);
    }

    #[test]
    fn narrow_short_layout_stays_inside_the_terminal() {
        let area = Rect::new(3, 2, 8, 4);
        let layout = RootLayout::calculate(area, 10, 10);

        for region in [
            layout.transcript,
            layout.queue,
            layout.composer,
            layout.composer_content,
        ] {
            assert!(region.x >= area.x);
            assert!(region.y >= area.y);
            assert!(region.right() <= area.right());
            assert!(region.bottom() <= area.bottom());
        }
        assert_eq!(layout.queue.height, 0);
        assert_eq!(layout.transcript.height, 0);
    }
}
