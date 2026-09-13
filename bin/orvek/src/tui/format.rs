//! Shared formatting for terminal-facing values.

use std::{borrow::Cow, env, path::Path, time::Duration};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn normalize_line_endings(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }

    let mut normalized = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(index) = remaining.find('\r') {
        normalized.push_str(&remaining[..index]);
        normalized.push('\n');
        remaining = &remaining[index + 1..];
        if let Some(after_newline) = remaining.strip_prefix('\n') {
            remaining = after_newline;
        }
    }
    normalized.push_str(remaining);
    Cow::Owned(normalized)
}

pub(crate) fn sanitize_terminal_text(text: &str) -> Cow<'_, str> {
    sanitize_terminal_text_with_break(text, '\n')
}

pub(crate) fn sanitize_terminal_text_inline(text: &str) -> Cow<'_, str> {
    sanitize_terminal_text_with_break(text, ' ')
}

fn sanitize_terminal_text_with_break(text: &str, line_break: char) -> Cow<'_, str> {
    let requires_sanitization = text.chars().any(|character| {
        character == '\r'
            || character == '\t'
            || character.is_control() && character != '\n'
            || character == '\n' && line_break != '\n'
    });
    if !requires_sanitization {
        return Cow::Borrowed(text);
    }

    let mut sanitized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                sanitized.push(line_break);
            }
            '\n' => sanitized.push(line_break),
            '\t' => sanitized.push_str("    "),
            character if character.is_control() => sanitized.push('�'),
            character => sanitized.push(character),
        }
    }
    Cow::Owned(sanitized)
}

pub(crate) fn terminal_text_width(text: &str) -> usize {
    text.graphemes(true)
        .map(|grapheme| match grapheme {
            "\t" => 4,
            grapheme if grapheme.contains(char::is_control) => 1,
            grapheme => grapheme.width(),
        })
        .sum()
}

pub(crate) fn truncate_display(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let text = sanitize_terminal_text_inline(text);
    if text.width() <= width {
        return text.into_owned();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        if used + grapheme.width() > width - 1 {
            break;
        }
        used += grapheme.width();
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

/// Wrap display text without changing the stored content or splitting a grapheme.
pub(crate) fn wrap_display_lines(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let text = sanitize_terminal_text(text);
    let mut lines = Vec::new();
    for source in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        let mut last_break = None;
        for original in source.graphemes(true) {
            let grapheme = if original.width() > width {
                "�"
            } else {
                original
            };
            let cells = grapheme.width();
            while used + cells > width {
                if let Some(index) = last_break.take() {
                    lines.push(line[..index].to_owned());
                    line = line[index..].to_owned();
                    used = line.width();
                } else {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
            }
            line.push_str(grapheme);
            used += cells;
            if grapheme.chars().all(char::is_whitespace)
                && !matches!(grapheme, "\u{a0}" | "\u{202f}")
                && !line.trim().is_empty()
            {
                last_break = Some(line.len());
            }
        }
        lines.push(line);
    }
    lines
}

pub(crate) fn format_duration(nanoseconds: u64) -> String {
    if nanoseconds >= 1_000_000_000 {
        let tenths = duration_display_tick(nanoseconds).saturating_sub(1_000);
        return format!("{}.{:01}s", tenths / 10, tenths % 10);
    }
    format!("{}ms", duration_display_tick(nanoseconds))
}

pub(crate) fn format_turn_duration(nanoseconds: u64) -> String {
    let total_seconds = nanoseconds / 1_000_000_000;
    let days = total_seconds / 86_400;
    let hours = total_seconds % 86_400 / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

pub(crate) fn duration_display_tick(nanoseconds: u64) -> u64 {
    if nanoseconds < 1_000_000_000 {
        return nanoseconds / 1_000_000;
    }
    1_000_u64.saturating_add(nanoseconds.saturating_add(50_000_000) / 100_000_000)
}

pub(crate) fn humanize_tool(name: &str) -> String {
    name.trim_start_matches("mcp__")
        .replace("__", " · ")
        .replace('_', " ")
}

pub(crate) fn shorten_home(path: &Path) -> String {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from);
    let Some(home) = home else {
        return path.display().to_string();
    };
    if path == home {
        return "~".to_owned();
    }
    let Ok(relative) = path.strip_prefix(&home) else {
        return path.display().to_string();
    };
    format!("~/{}", relative.display())
}

pub(crate) fn format_age(started_at_unix_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let started = Duration::from_millis(started_at_unix_ms);
    let elapsed = now.saturating_sub(started);
    let seconds = elapsed.as_secs();
    match seconds {
        0..=59 => "now".to_owned(),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        duration_display_tick, format_duration, format_turn_duration, normalize_line_endings,
        sanitize_terminal_text, sanitize_terminal_text_inline, terminal_text_width,
        truncate_display, wrap_display_lines,
    };
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn display_wrapping_preserves_indentation_and_graphemes() {
        let text = "  café\n\n    界面 👩‍💻  ";
        for width in 2..20 {
            let rows = wrap_display_lines(text, width);
            assert!(rows.iter().all(|line| line.width() <= width));
            assert_eq!(rows.concat(), text.replace('\n', ""));
            assert!(rows.iter().any(String::is_empty));
        }
        assert_eq!(
            wrap_display_lines("one two three", 7),
            ["one ", "two ", "three"]
        );
        assert_eq!(wrap_display_lines("", 7), [""]);
        assert_eq!(
            wrap_display_lines("X O\u{a0}reopen", 9),
            ["X ", "O\u{a0}reopen"]
        );
        assert!(wrap_display_lines("content", 0).is_empty());
    }

    #[test]
    fn display_projections_bound_controls_and_long_identifiers() {
        for width in 0..30 {
            let text = "cafe\u{301}\t界面👩‍💻\u{1b}\r\nlong_identifier_without_spaces";
            let short = truncate_display(text, width);
            assert!(short.width() <= width);
            assert!(!short.contains(char::is_control));
            for row in wrap_display_lines(text, width) {
                assert!(row.width() <= width);
                assert!(!row.contains(char::is_control));
            }
        }
        assert_eq!(truncate_display("界面", 3), "界…");
        assert_eq!(truncate_display("cafe\u{301}", 4), "cafe\u{301}");
        assert_eq!(wrap_display_lines("界", 1), ["�"]);
    }

    #[test]
    fn line_endings_are_normalized_without_changing_lf_text() {
        assert_eq!(normalize_line_endings("one\ntwo"), "one\ntwo");
        assert_eq!(
            normalize_line_endings("one\r\ntwo\rthree"),
            "one\ntwo\nthree"
        );
    }

    #[test]
    fn terminal_text_has_safe_multiline_and_inline_projections() {
        let text = "one\r\ntwo\tthree\u{1b}";
        assert_eq!(sanitize_terminal_text(text), "one\ntwo    three�");
        assert_eq!(sanitize_terminal_text_inline(text), "one two    three�");
        assert_eq!(terminal_text_width("two\tthree\u{1b}"), 13);
    }

    #[test]
    fn durations_round_to_the_same_tick_used_for_live_redraws() {
        for (nanoseconds, expected) in [
            (999_999_999, "999ms"),
            (1_049_999_999, "1.0s"),
            (1_050_000_000, "1.1s"),
            (11_249_999_999, "11.2s"),
            (11_250_000_000, "11.3s"),
        ] {
            assert_eq!(format_duration(nanoseconds), expected);
        }
        assert_eq!(duration_display_tick(1_050_000_000), 1_011);
    }

    #[test]
    fn turn_durations_only_use_whole_seconds_and_larger_units() {
        for (nanoseconds, expected) in [
            (999_999_999, "0s"),
            (5_000_000_000, "5s"),
            (65_000_000_000, "1m 5s"),
            (3_665_000_000_000, "1h 1m 5s"),
            (176_465_000_000_000, "2d 1h 1m 5s"),
        ] {
            assert_eq!(format_turn_duration(nanoseconds), expected);
        }
    }
}
