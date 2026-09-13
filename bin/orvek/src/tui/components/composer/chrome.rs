use crate::tui::format::wrap_display_lines;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
pub(super) enum LabelKind {
    Context,
    InputMode,
    Review,
    Activity,
    Subagents,
    Timer,
    Model,
    Effort,
    Fast,
    Pro,
}

pub(super) struct ChromeLabel {
    pub(super) kind: LabelKind,
    pub(super) text: String,
    pub(super) area: Rect,
    pub(super) source_offset: usize,
}

pub(super) struct ChromeLayout {
    pub(super) width: u16,
    pub(super) rows: u16,
    pub(super) labels: Vec<ChromeLabel>,
}

impl ChromeLayout {
    pub(super) fn new(
        width: u16,
        primary: Vec<(LabelKind, String)>,
        metadata: Vec<(LabelKind, String)>,
    ) -> Self {
        let inner = usize::from(width.saturating_sub(4));
        let mut layout = Self {
            width,
            rows: 1,
            labels: Vec::new(),
        };
        if inner == 0 {
            return layout;
        }
        let primary_width = group_width(&primary);
        let metadata_width = group_width(&metadata);
        let shared = primary_width + metadata_width + 4 <= inner;
        layout.place(primary, inner, 0, 0);
        if shared {
            layout.place(metadata, inner, inner - metadata_width, 0);
        } else {
            layout.place(metadata, inner, 0, layout.rows);
        }
        layout
    }

    fn place(&mut self, parts: Vec<(LabelKind, String)>, width: usize, mut x: usize, mut y: u16) {
        let mut separated = false;
        for (kind, text) in parts {
            let gap = if separated { spacing(kind) } else { 0 };
            if x + gap + text.width() > width && x > 0 {
                x = 0;
                y += 1;
            } else {
                x += gap;
            }
            let mut source_offset = 0;
            for (index, line) in wrap_display_lines(&text, width).into_iter().enumerate() {
                if index > 0 {
                    x = 0;
                    y += 1;
                }
                let cells = line.width();
                self.labels.push(ChromeLabel {
                    kind,
                    source_offset,
                    area: Rect::new(x as u16 + 2, y, cells as u16, 1),
                    text: line.clone(),
                });
                source_offset += line.chars().count();
                x += cells;
            }
            separated = true;
            self.rows = self.rows.max(y + 1);
        }
    }

    pub(super) fn editor_area(&self, area: Rect) -> Rect {
        if area.width < 5 || area.height < 3 {
            return Rect {
                height: area.height.min(1),
                ..area
            };
        }
        if area.width < 32 {
            return Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        }
        let rows = self.rows.min(area.height.saturating_sub(2));
        let padding = u16::from(area.height >= rows + 4);
        Rect::new(
            area.x + 2,
            area.y + rows + padding,
            area.width - 4,
            area.height.saturating_sub(rows + padding * 2 + 1),
        )
    }
}

fn group_width(parts: &[(LabelKind, String)]) -> usize {
    parts.iter().map(|(_, text)| text.width()).sum::<usize>()
        + parts
            .iter()
            .skip(1)
            .map(|(kind, _)| spacing(*kind))
            .sum::<usize>()
}

fn spacing(kind: LabelKind) -> usize {
    if matches!(
        kind,
        LabelKind::Fast | LabelKind::Pro | LabelKind::Subagents
    ) {
        1
    } else {
        2
    }
}
