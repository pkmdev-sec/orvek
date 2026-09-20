//! Read-only context telemetry overlay.

use super::{
    dialog::Dialog,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    context::{CompactionDiagnostics, CompactionTrigger, ContextDiagnostics},
    theme::Theme,
};
use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, BarChart, Block, BorderType, Borders, Cell, Chart, Dataset, Gauge, GraphType,
        Paragraph, Row, Sparkline, Table, Wrap,
    },
};

const FOOTER: [(&str, &str); 2] = [("r", "refresh"), ("esc", "close")];

pub(super) enum ContextDiagnosticsEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ContextDiagnosticsEffect {
    Dismiss,
    Refresh,
}

pub(super) struct ContextDiagnosticsPanel {
    diagnostics: ContextDiagnostics,
    body: Rect,
    scroll: u16,
    max_scroll: u16,
}

impl ContextDiagnosticsPanel {
    pub(super) const fn new(diagnostics: ContextDiagnostics) -> Self {
        Self {
            diagnostics,
            body: Rect::new(0, 0, 0, 0),
            scroll: 0,
            max_scroll: 0,
        }
    }

    pub(super) fn replace(&mut self, diagnostics: ContextDiagnostics) {
        self.diagnostics = diagnostics;
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let label = Style::default().fg(theme.muted());
        let value = Style::default().fg(theme.text());
        let heading = Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD);
        let mut lines = vec![Line::styled(" Context budget", heading)];
        let usage = self.diagnostics.usage;
        let current = usage.map(|usage| usage.total);
        lines.extend([
            fact(
                " Window / request limit",
                format!(
                    "{} / {}",
                    optional_count(self.diagnostics.model_window_tokens),
                    optional_count(self.diagnostics.request_token_limit)
                ),
                label,
                value,
            ),
            fact(
                " Current / headroom",
                current.map_or_else(
                    || "unavailable".to_owned(),
                    |tokens| {
                        format!(
                            "{} / {}",
                            format_count(tokens),
                            optional_count(
                                self.diagnostics
                                    .model_window_tokens
                                    .map(|window| window.saturating_sub(tokens))
                            )
                        )
                    },
                ),
                label,
                value,
            ),
            fact(
                " Until request limit",
                optional_count(
                    current
                        .zip(self.diagnostics.request_token_limit)
                        .map(|(tokens, limit)| limit.saturating_sub(tokens)),
                ),
                label,
                value,
            ),
            fact(
                " Recorded billable tokens",
                optional_count(self.diagnostics.billed_tokens),
                label,
                value,
            ),
            fact(
                " Billing uncertainty",
                if self.diagnostics.billing_uncertain {
                    "unmeasured provider attempt".into()
                } else {
                    "none recorded".into()
                },
                label,
                value,
            ),
            Line::styled(" Latest context measurement", heading),
            fact(
                " Input (cached/uncached)",
                usage.map_or_else(
                    || "unavailable".to_owned(),
                    |usage| {
                        format!(
                            "{} ({}/{})",
                            format_count(usage.input),
                            format_count(usage.cached_input),
                            format_count(usage.uncached_input)
                        )
                    },
                ),
                label,
                value,
            ),
            fact(
                " Cache-hit ratio",
                usage.map_or_else(
                    || "unavailable".to_owned(),
                    |usage| {
                        if usage.input == 0 {
                            "unavailable".to_owned()
                        } else {
                            format!(
                                "{:.1}%",
                                usage.cached_input as f64 * 100.0 / usage.input as f64
                            )
                        }
                    },
                ),
                label,
                value,
            ),
            fact(
                " Output / reasoning / total",
                usage.map_or_else(
                    || "unavailable".to_owned(),
                    |usage| {
                        format!(
                            "{} / {} / {}",
                            format_count(usage.output),
                            optional_count(usage.reasoning),
                            format_count(usage.total)
                        )
                    },
                ),
                label,
                value,
            ),
            fact(
                " Cached-token effect",
                "cached input still counts toward the window".to_owned(),
                label,
                value,
            ),
            Line::styled(" Generation", heading),
            fact(
                " Prompt cache",
                optional_bool(self.diagnostics.prompt_cache),
                label,
                value,
            ),
            Line::styled(" Representation", heading),
            fact(
                " Source / selection / pages",
                self.diagnostics.representation.map_or_else(
                    || "unavailable".to_owned(),
                    |item| {
                        format!(
                            "{} bytes / {} native / {} bitmap / {} pages",
                            format_count(item.source_bytes),
                            item.native_segments,
                            item.bitmap_segments,
                            item.bitmap_pages
                        )
                    },
                ),
                label,
                value,
            ),
            fact(
                " Pair / next-call savings",
                self.diagnostics.representation.map_or_else(
                    || "unavailable".to_owned(),
                    |item| {
                        format!(
                            "{} / {}",
                            optional_usd(item.observed_pair_savings),
                            optional_usd(item.estimated_next_call_savings)
                        )
                    },
                ),
                label,
                value,
            ),
            Line::styled(" Local estimates", heading),
            fact(
                " Context categories",
                "unavailable (prefix/user/assistant/reasoning/tools/compaction/media)".to_owned(),
                label,
                value,
            ),
            fact(
                " Pending shell",
                "unavailable (count/bytes/tokens)".to_owned(),
                label,
                value,
            ),
            fact(
                " Server/local delta",
                "unavailable".to_owned(),
                label,
                value,
            ),
            Line::styled(" Compaction", heading),
            fact(
                " Started / completed",
                format!(
                    "{} / {}",
                    self.diagnostics.compactions_started, self.diagnostics.compactions_completed
                ),
                label,
                value,
            ),
        ]);
        let compaction = self.diagnostics.last_compaction;
        lines.extend([
            fact(
                " Trigger / time",
                compaction.map_or_else(|| "unavailable".to_owned(), format_compaction_time),
                label,
                value,
            ),
            fact(
                " Before / next input",
                compaction.map_or_else(
                    || "unavailable".to_owned(),
                    |item| {
                        format!(
                            "{} / {}",
                            optional_count(item.before_tokens),
                            optional_count(item.after_tokens)
                        )
                    },
                ),
                label,
                value,
            ),
        ]);
        lines
    }

    fn render_dashboard(&self, frame: &mut Frame<'_>, body: Rect, theme: &Theme) {
        let [gauges, flow, details, footnote] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Min(7),
            Constraint::Length(1),
        ])
        .spacing(1)
        .areas(body);
        let [context_area, cache_area] =
            Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
                .spacing(1)
                .areas(gauges);

        let usage = self.diagnostics.usage;
        let current = usage.map(|usage| usage.total);
        let context_percent = current
            .zip(self.diagnostics.model_window_tokens)
            .filter(|(_, window)| *window > 0)
            .map_or(0, |(tokens, window)| {
                u16::try_from(tokens.saturating_mul(100).saturating_div(window).min(100))
                    .unwrap_or(100)
            });
        let context_label = current
            .zip(self.diagnostics.model_window_tokens)
            .map_or_else(
                || "measurement unavailable".to_owned(),
                |(tokens, window)| {
                    format!(
                        "{} / {} · {context_percent}%",
                        format_count(tokens),
                        format_count(window)
                    )
                },
            );
        frame.render_widget(
            Gauge::default()
                .block(dashboard_block("Context budget", theme))
                .percent(context_percent)
                .label(context_label)
                .use_unicode(true)
                .gauge_style(Style::default().fg(context_pressure_color(theme, context_percent))),
            context_area,
        );

        let cache_percent = usage.filter(|usage| usage.input > 0).map_or(0, |usage| {
            u16::try_from(
                usage
                    .cached_input
                    .saturating_mul(100)
                    .saturating_div(usage.input),
            )
            .unwrap_or(100)
        });
        let cache_label = usage.filter(|usage| usage.input > 0).map_or_else(
            || "unavailable".to_owned(),
            |usage| {
                format!(
                    "{:.1}% · {} cached",
                    usage.cached_input as f64 * 100.0 / usage.input as f64,
                    format_count(usage.cached_input)
                )
            },
        );
        frame.render_widget(
            Gauge::default()
                .block(dashboard_block("Cache hit", theme))
                .percent(cache_percent.min(100))
                .label(cache_label)
                .use_unicode(true)
                .gauge_style(Style::default().fg(theme.brand_secondary())),
            cache_area,
        );

        let flow_rows = usage.map_or_else(
            || {
                vec![Row::new(vec![
                    Cell::from("Latest measurement"),
                    Cell::from("unavailable"),
                    Cell::from("waiting for provider usage"),
                ])]
            },
            |usage| {
                vec![
                    Row::new(vec![
                        Cell::from("Input (cached/uncached)"),
                        Cell::from(format!(
                            "{} ({}/{})",
                            format_count(usage.input),
                            format_count(usage.cached_input),
                            format_count(usage.uncached_input)
                        )),
                        Cell::from("window pressure"),
                    ]),
                    Row::new(vec![
                        Cell::from("Output / reasoning / total"),
                        Cell::from(format!(
                            "{} / {} / {}",
                            format_count(usage.output),
                            optional_count(usage.reasoning),
                            format_count(usage.total)
                        )),
                        Cell::from("latest call"),
                    ]),
                    Row::new(vec![
                        Cell::from("Recorded billable tokens"),
                        Cell::from(optional_count(self.diagnostics.billed_tokens)),
                        Cell::from(if self.diagnostics.billing_uncertain {
                            "provider gap"
                        } else {
                            "receipts complete"
                        }),
                    ]),
                ]
            },
        );
        if body.width >= 112 {
            let token_bars = usage.map_or(
                [("cached", 0), ("uncached", 0), ("output", 0), ("reason", 0)],
                |usage| {
                    [
                        ("cached", usage.cached_input),
                        ("uncached", usage.uncached_input),
                        ("output", usage.output),
                        ("reason", usage.reasoning.unwrap_or_default()),
                    ]
                },
            );
            frame.render_widget(
                BarChart::default()
                    .block(dashboard_block("Token flow", theme))
                    .data(&token_bars)
                    .bar_width(10)
                    .bar_gap(2)
                    .bar_style(Style::default().fg(theme.accent()))
                    .value_style(Style::default().fg(theme.text()))
                    .label_style(Style::default().fg(theme.muted())),
                flow,
            );
        } else {
            frame.render_widget(
                Table::new(
                    flow_rows,
                    [
                        Constraint::Length(27),
                        Constraint::Length(27),
                        Constraint::Min(10),
                    ],
                )
                .header(
                    Row::new(vec!["Metric", "Value", "Signal"]).style(
                        Style::default()
                            .fg(theme.accent())
                            .add_modifier(Modifier::BOLD),
                    ),
                )
                .column_spacing(1)
                .style(Style::default().fg(theme.text()))
                .block(dashboard_block("Token flow", theme)),
                flow,
            );
        }

        let [trend_area, details_area] =
            Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)])
                .spacing(1)
                .areas(details);
        let mut trend = self.diagnostics.usage_history().collect::<Vec<_>>();
        if trend.is_empty()
            && let Some(usage) = usage
        {
            trend.push(usage.input);
        }
        let trend_max = self
            .diagnostics
            .model_window_tokens
            .filter(|window| *window > 0)
            .unwrap_or_else(|| trend.iter().copied().max().unwrap_or(1))
            .max(1);
        if body.width >= 112 {
            let points = trend
                .iter()
                .enumerate()
                .map(|(index, value)| (index as f64, *value as f64))
                .collect::<Vec<_>>();
            let x_max = points.len().saturating_sub(1).max(1) as f64;
            let dataset = Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(theme.accent()))
                .data(&points);
            frame.render_widget(
                Chart::new(vec![dataset])
                    .block(dashboard_block("Usage trend", theme))
                    .x_axis(Axis::default().bounds([0.0, x_max]))
                    .y_axis(Axis::default().bounds([0.0, trend_max as f64])),
                trend_area,
            );
        } else {
            frame.render_widget(
                Sparkline::default()
                    .block(dashboard_block("Usage trend", theme))
                    .data(&trend)
                    .max(trend_max)
                    .style(Style::default().fg(theme.accent())),
                trend_area,
            );
        }

        let representation = self.diagnostics.representation;
        let compaction = self.diagnostics.last_compaction;
        let detail_lines = vec![
            fact(
                " Source / selection / pages",
                representation.map_or_else(
                    || "unavailable".to_owned(),
                    |item| {
                        format!(
                            "{} / {}n {}b / {}p",
                            format_count(item.source_bytes),
                            item.native_segments,
                            item.bitmap_segments,
                            item.bitmap_pages
                        )
                    },
                ),
                Style::default().fg(theme.muted()),
                Style::default().fg(theme.text()),
            ),
            fact(
                " Pair / next-call savings",
                representation.map_or_else(
                    || "unavailable".to_owned(),
                    |item| {
                        format!(
                            "{} / {}",
                            optional_usd(item.observed_pair_savings),
                            optional_usd(item.estimated_next_call_savings)
                        )
                    },
                ),
                Style::default().fg(theme.muted()),
                Style::default().fg(theme.text()),
            ),
            fact(
                " Prompt cache",
                optional_bool(self.diagnostics.prompt_cache),
                Style::default().fg(theme.muted()),
                Style::default().fg(theme.text()),
            ),
            fact(
                " Compactions",
                format!(
                    "{} started / {} complete",
                    self.diagnostics.compactions_started, self.diagnostics.compactions_completed
                ),
                Style::default().fg(theme.muted()),
                Style::default().fg(theme.text()),
            ),
            fact(
                " Before / next input",
                compaction.map_or_else(
                    || "unavailable".to_owned(),
                    |item| {
                        format!(
                            "{} / {}",
                            optional_count(item.before_tokens),
                            optional_count(item.after_tokens)
                        )
                    },
                ),
                Style::default().fg(theme.muted()),
                Style::default().fg(theme.text()),
            ),
        ];
        frame.render_widget(
            Paragraph::new(detail_lines).block(dashboard_block("Representation", theme)),
            details_area,
        );
        frame.render_widget(
            Paragraph::new(Line::styled(
                " cached input still counts toward the window",
                Style::default()
                    .fg(theme.muted())
                    .add_modifier(Modifier::ITALIC),
            )),
            footnote,
        );
    }
}

impl Component for ContextDiagnosticsPanel {
    type Event = ContextDiagnosticsEvent;
    type Effect = ContextDiagnosticsEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            ContextDiagnosticsEvent::Terminal(Event::Key(key))
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                match key.code {
                    KeyCode::Esc => {
                        return ComponentUpdate {
                            effects: vec![ContextDiagnosticsEffect::Dismiss],
                            render: RenderRequest::Immediate,
                        };
                    }
                    KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => {
                        return ComponentUpdate {
                            effects: vec![ContextDiagnosticsEffect::Refresh],
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
            ContextDiagnosticsEvent::Terminal(Event::Mouse(mouse))
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
        let layout =
            Dialog::new("Context diagnostics", 128, 34, &FOOTER).render(frame, area, theme);
        self.body = layout.body;
        if self.body.width >= 68 && self.body.height >= 20 {
            self.max_scroll = 0;
            self.scroll = 0;
            self.render_dashboard(frame, self.body, theme);
            return;
        }
        let paragraph = Paragraph::new(self.lines(theme)).wrap(Wrap { trim: false });
        self.max_scroll = paragraph
            .line_count(self.body.width)
            .saturating_sub(usize::from(self.body.height))
            .min(usize::from(u16::MAX)) as u16;
        self.scroll = self.scroll.min(self.max_scroll);
        frame.render_widget(paragraph.scroll((self.scroll, 0)), self.body);
    }
}

fn dashboard_block<'a>(title: &'a str, theme: &Theme) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border()))
        .title(Line::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ))
}

fn context_pressure_color(theme: &Theme, percent: u16) -> ratatui::style::Color {
    match percent {
        0..=49 => theme.success(),
        50..=74 => theme.brand_secondary(),
        75..=89 => theme.warning(),
        _ => theme.error(),
    }
}

fn fact(
    label_text: &'static str,
    value_text: String,
    label_style: Style,
    value_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label_text:<26}"), label_style),
        Span::styled(value_text, value_style),
    ])
}

fn optional_count(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), format_count)
}

fn optional_usd(value: Option<orvek_harness::inference::UsdCost>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

fn optional_bool(value: Option<bool>) -> String {
    value.map_or_else(
        || "unavailable".to_owned(),
        |present| if present { "present" } else { "absent" }.to_owned(),
    )
}

fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

fn format_compaction_time(compaction: CompactionDiagnostics) -> String {
    let trigger = match compaction.trigger {
        CompactionTrigger::Automatic => "automatic",
    };
    let timestamp = i64::try_from(compaction.started_at_unix_ms)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map_or_else(
            || "unknown time".to_owned(),
            |time| time.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        );
    let duration = compaction.completed_at_unix_ms.map_or_else(
        || "ongoing".to_owned(),
        |completed| format_duration_millis(completed.saturating_sub(compaction.started_at_unix_ms)),
    );
    format!("{trigger} / {timestamp} · {duration}")
}

fn format_duration_millis(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        return format!("{milliseconds}ms");
    }
    format!("{}.{}s", milliseconds / 1_000, milliseconds % 1_000 / 100)
}

#[cfg(test)]
mod tests {
    use super::{Component, ContextDiagnosticsPanel, format_compaction_time};
    use crate::tui::{
        context::{CompactionDiagnostics, CompactionTrigger, ContextDiagnostics, TokenUsage},
        theme::Theme,
    };
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn panel_renders_counts_unavailable_metrics_and_cache_help() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.usage = Some(TokenUsage {
            input: 100_000,
            cached_input: 75_000,
            uncached_input: 25_000,
            output: 2_000,
            reasoning: Some(500),
            total: 102_000,
        });
        diagnostics.prompt_cache = Some(true);
        let mut panel = ContextDiagnosticsPanel::new(diagnostics);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| panel.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Context diagnostics",
            "Context budget",
            "Token flow",
            "Usage trend",
            "100,000 (75,000/25,000)",
            "Representation",
            "Pair / next-call savings",
            "cached input still counts toward the window",
            "r refresh · esc close",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?}\n{rendered}"
            );
        }
    }

    #[test]
    fn compaction_time_is_readable_and_includes_duration() {
        let rendered = format_compaction_time(CompactionDiagnostics {
            trigger: CompactionTrigger::Automatic,
            started_at_unix_ms: 0,
            completed_at_unix_ms: Some(39_095),
            before_tokens: None,
            after_tokens: None,
        });

        assert_eq!(rendered, "automatic / 1970-01-01 00:00:00Z · 39.0s");
    }
    #[test]
    fn compact_diagnostics_wheel_reaches_final_metrics() {
        use super::ContextDiagnosticsEvent;
        use crossterm::event::{Event, MouseEvent, MouseEventKind};
        let mut panel = ContextDiagnosticsPanel::new(ContextDiagnostics::default());
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        for _ in 0..80 {
            terminal
                .draw(|frame| panel.render(frame, frame.area(), &Theme::default()))
                .unwrap();
            panel.update(ContextDiagnosticsEvent::Terminal(Event::Mouse(
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: 20,
                    row: 5,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            )));
        }
        terminal
            .draw(|frame| panel.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Before / next input"), "{rendered}");
    }
    #[test]
    fn wheel_outside_popup_does_not_scroll_and_resize_clamps_to_content() {
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
        };
        let mut panel = ContextDiagnosticsPanel::new(ContextDiagnostics::default());
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| panel.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let before = terminal.backend().buffer().clone();
        panel.update(super::ContextDiagnosticsEvent::Terminal(Event::Mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
        )));
        terminal
            .draw(|frame| panel.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer(), &before);
        panel.update(super::ContextDiagnosticsEvent::Terminal(Event::Key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        )));
        let mut wide = Terminal::new(TestBackend::new(100, 40)).unwrap();
        wide.draw(|frame| panel.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let rendered = wide
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Context budget"));
        assert!(rendered.contains("Before / next input"));
    }
}
