//! Searchable workspace file picker opened from the composer.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{format::sanitize_terminal_text_inline, theme::Theme};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::{
    cmp::Reverse,
    fs,
    path::Path,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const KEY_BINDINGS: [(&str, &str); 3] = [("↑↓", "move"), ("enter/tab", "insert"), ("esc", "close")];
const SEARCH_LABEL: &str = "Search: ";
const FOCUS_MARKER: &str = "› ";
const SKIPPED_DIRECTORIES: [&str; 4] = [".git", ".jj", "node_modules", "target"];

pub(super) enum FileFinderEvent {
    Terminal(Event),
    Query(String),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum FileFinderEffect {
    Dismiss,
    Insert(String),
}

pub(super) struct FileFinder {
    paths: Vec<String>,
    query: String,
    selected: usize,
    matches: Vec<usize>,
    loading: bool,
    list_area: Rect,
    offset: usize,
    last_click: Option<(usize, Instant)>,
}

impl FileFinder {
    #[cfg(test)]
    pub(super) fn new(workspace: &Path) -> Self {
        Self::from_paths(discover_paths(workspace))
    }

    pub(super) fn loading() -> Self {
        Self {
            loading: true,
            ..Self::from_paths(Vec::new())
        }
    }

    pub(super) fn from_paths(paths: Vec<String>) -> Self {
        let matches = (0..paths.len()).collect();
        Self {
            paths,
            query: String::new(),
            selected: 0,
            matches,
            loading: false,
            list_area: Rect::default(),
            offset: 0,
            last_click: None,
        }
    }

    pub(super) fn set_paths(&mut self, paths: Vec<String>) {
        self.paths = paths;
        self.loading = false;
        self.refresh_matches();
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<FileFinderEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        self.last_click = None;
        match key.code {
            KeyCode::Esc => Self::dismiss(),
            KeyCode::Enter | KeyCode::Tab => self.handle_enter(),
            KeyCode::Up => {
                self.select_previous();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down => {
                self.select_next();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn set_query(&mut self, query: String) -> ComponentUpdate<FileFinderEffect> {
        self.query = query;
        self.refresh_matches();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn dismiss() -> ComponentUpdate<FileFinderEffect> {
        ComponentUpdate {
            effects: vec![FileFinderEffect::Dismiss],
            render: RenderRequest::Immediate,
        }
    }

    fn handle_enter(&mut self) -> ComponentUpdate<FileFinderEffect> {
        let Some(index) = self.matches.get(self.selected) else {
            return ComponentUpdate::none();
        };
        ComponentUpdate {
            effects: vec![FileFinderEffect::Insert(self.paths[*index].clone())],
            render: RenderRequest::Immediate,
        }
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_ascii_lowercase();
        let mut matches = self
            .paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| fuzzy_score(path, &query).map(|score| (index, score)))
            .collect::<Vec<_>>();
        matches.sort_by_key(|(index, score)| (Reverse(*score), self.paths[*index].as_str()));
        self.matches = matches.into_iter().map(|(index, _)| index).collect();
        self.selected = 0;
        self.offset = 0;
        self.last_click = None;
        self.list_area = Rect::default();
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1).min(self.matches.len() - 1);
        }
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let marker = "  ";
        let prefix_width = marker.width() + SEARCH_LABEL.width();
        let query_width = usize::from(area.width).saturating_sub(prefix_width);
        let visible_query = visible_query_tail(&self.query, query_width);
        let label_style = Style::default().fg(theme.muted());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, label_style),
                Span::styled(SEARCH_LABEL, label_style),
                Span::styled(visible_query, Style::default().fg(theme.text())),
            ])),
            area,
        );
    }

    fn update_mouse(
        &mut self,
        mouse: MouseEvent,
        now: Instant,
    ) -> ComponentUpdate<FileFinderEffect> {
        if !self
            .list_area
            .contains(Position::new(mouse.column, mouse.row))
        {
            self.last_click = None;
            return ComponentUpdate::none();
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.last_click = None;
                self.select_previous();
            }
            MouseEventKind::ScrollDown => {
                self.last_click = None;
                self.select_next();
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let index = self.offset + usize::from(mouse.row - self.list_area.y);
                if index >= self.matches.len() {
                    return ComponentUpdate::none();
                }
                let confirm = self.last_click.is_some_and(|(previous, at)| {
                    previous == index
                        && now.saturating_duration_since(at) <= Duration::from_millis(500)
                });
                self.selected = index;
                self.last_click = Some((index, now));
                if confirm {
                    self.last_click = None;
                    return self.handle_enter();
                }
            }
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render_paths(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = area;
        if area.is_empty() {
            return;
        }
        if self.matches.is_empty() {
            let message = if self.loading {
                "  Finding workspace paths…"
            } else if self.paths.is_empty() {
                "  No workspace paths found"
            } else {
                "  No matching paths"
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let capacity = usize::from(area.height);
        self.offset = self
            .offset
            .min(self.matches.len().saturating_sub(capacity))
            .min(self.selected);
        if self.selected >= self.offset + capacity {
            self.offset = self.selected + 1 - capacity;
        }
        let width = usize::from(area.width).saturating_sub(4);
        for (row, index) in self
            .matches
            .iter()
            .skip(self.offset)
            .take(capacity)
            .enumerate()
        {
            let path = compact_path(&self.paths[*index], width);
            let split = path
                .trim_end_matches('/')
                .rfind('/')
                .map_or(0, |index| index + 1);
            let selected = row + self.offset == self.selected;
            let style = Style::default().fg(if selected {
                theme.accent()
            } else {
                theme.text()
            });
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(if selected { FOCUS_MARKER } else { "  " }, style),
                    Span::styled(path[..split].to_owned(), Style::default().fg(theme.muted())),
                    Span::styled(path[split..].to_owned(), style.add_modifier(Modifier::BOLD)),
                ])),
                Rect::new(area.x, area.y + row as u16, area.width, 1),
            );
        }
    }

    fn render_details(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let Some(index) = self.matches.get(self.selected) else {
            return;
        };
        if area.is_empty() {
            return;
        }
        let width = usize::from(area.width).saturating_sub(4);
        if width == 0 {
            return;
        }
        let text = compact_path(
            &format!("Insert: @{}", self.paths[*index]),
            width * usize::from(area.height),
        );
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut used = 0;
        for grapheme in text.graphemes(true) {
            if used + grapheme.width() > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push_str(grapheme);
            used += grapheme.width();
        }
        lines.push(line);
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .take(usize::from(area.height))
                    .map(Line::from)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(theme.muted())),
            Rect::new(
                area.x + 2.min(area.width),
                area.y,
                width as u16,
                area.height,
            ),
        );
    }
}

impl Component for FileFinder {
    type Event = FileFinderEvent;
    type Effect = FileFinderEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            FileFinderEvent::Terminal(Event::Key(key)) => self.update_key(key),
            FileFinderEvent::Terminal(Event::Mouse(mouse)) => {
                self.update_mouse(mouse, Instant::now())
            }
            FileFinderEvent::Terminal(_) => ComponentUpdate::none(),
            FileFinderEvent::Query(query) => self.set_query(query),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect::default();
        if area.is_empty() {
            return;
        }

        let layout = Floating::new("Files and directories", 72, 14, &KEY_BINDINGS)
            .render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let search_area = Rect {
            height: 1,
            ..layout.body
        };
        let detail_height = 2.min(layout.body.height.saturating_sub(2));
        let paths_area = Rect::new(
            layout.body.x,
            layout.body.y + 1,
            layout.body.width,
            layout.body.height.saturating_sub(1 + detail_height),
        );
        let details_area = Rect::new(
            layout.body.x,
            paths_area.bottom(),
            layout.body.width,
            detail_height,
        );
        self.render_search(frame, search_area, theme);
        self.render_paths(frame, paths_area, theme);
        self.render_details(frame, details_area, theme);
    }
}

fn compact_path(path: &str, width: usize) -> String {
    let path = sanitize_terminal_text_inline(path);
    if path.width() <= width {
        return path.into_owned();
    }
    if width == 0 {
        return String::new();
    }
    let left_width = width / 2;
    let right_width = width - 1 - left_width;
    let mut left = String::new();
    for grapheme in path.graphemes(true) {
        if left.width() + grapheme.width() > left_width {
            break;
        }
        left.push_str(grapheme);
    }
    format!("{left}…{}", visible_query_tail(&path, right_width))
}

#[cfg(test)]
pub(crate) fn discover_paths(workspace: &Path) -> Vec<String> {
    discover_paths_cancellable(workspace, &CancellationToken::new())
}

pub(crate) fn discover_paths_cancellable(
    workspace: &Path,
    cancellation: &CancellationToken,
) -> Vec<String> {
    let mut paths = Vec::new();
    visit_directory(workspace, workspace, &mut paths, cancellation);
    if cancellation.is_cancelled() {
        return Vec::new();
    }
    paths.sort_unstable();
    paths
}

fn visit_directory(
    workspace: &Path,
    directory: &Path,
    paths: &mut Vec<String>,
    cancellation: &CancellationToken,
) {
    if cancellation.is_cancelled() {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if cancellation.is_cancelled() {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if is_skipped_directory(&path) {
                continue;
            }

            if let Some(relative) = relative_path(workspace, &path) {
                paths.push(format!("{relative}/"));
            }
            visit_directory(workspace, &path, paths, cancellation);
        } else if file_type.is_file()
            && let Some(relative) = relative_path(workspace, &path)
        {
            paths.push(relative);
        }
    }
}

fn relative_path(workspace: &Path, path: &Path) -> Option<String> {
    let relative = path
        .strip_prefix(workspace)
        .ok()?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if relative.chars().any(char::is_control) {
        return None;
    }
    Some(relative)
}

fn is_skipped_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name))
}

pub(super) fn fuzzy_score(path: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }

    let path = path.to_ascii_lowercase();
    let mut query = query.chars();
    let mut expected = query.next()?;
    let mut score = 0_usize;
    let mut previous_match = None;
    let mut previous_character = None;
    for (index, character) in path.chars().enumerate() {
        if character != expected {
            previous_character = Some(character);
            continue;
        }
        score += 10;
        if previous_match.is_some_and(|previous| previous + 1 == index) {
            score += 15;
        }
        if previous_character.is_none_or(|previous| previous == '/') {
            score += 8;
        }
        previous_match = Some(index);
        let Some(next) = query.next() else {
            return Some(score.saturating_sub(index));
        };
        expected = next;
        previous_character = Some(character);
    }
    None
}

pub(super) fn visible_query_tail(query: &str, width: usize) -> &str {
    let mut used = 0;
    for (index, grapheme) in query.grapheme_indices(true).rev() {
        used += grapheme.width();
        if used > width {
            return &query[index + grapheme.len()..];
        }
    }
    query
}

#[cfg(test)]
mod tests {
    use super::{
        Component, FileFinder, FileFinderEffect, FileFinderEvent, discover_paths, fuzzy_score,
    };
    use crate::tui::theme::Theme;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use std::fs;

    fn key(code: KeyCode) -> FileFinderEvent {
        FileFinderEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn workspace() -> tempfile::TempDir {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src/components")).unwrap();
        fs::create_dir_all(workspace.path().join("target/debug")).unwrap();
        fs::write(workspace.path().join("README.md"), "read me").unwrap();
        fs::write(workspace.path().join("src/lib.rs"), "pub mod components;").unwrap();
        fs::write(workspace.path().join("src/components/file_finder.rs"), "").unwrap();
        fs::write(workspace.path().join("target/debug/artifact"), "").unwrap();
        workspace
    }

    #[test]
    fn discovers_relative_workspace_paths_and_skips_build_directories() {
        let workspace = workspace();

        assert_eq!(
            discover_paths(workspace.path()),
            [
                "README.md",
                "src/",
                "src/components/",
                "src/components/file_finder.rs",
                "src/lib.rs"
            ]
        );
    }

    #[test]
    fn fuzzy_search_matches_non_contiguous_characters_and_ranks_tight_matches_first() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(FileFinderEvent::Query("ff".to_owned()));

        assert_eq!(finder.matches.len(), 1);
        assert_eq!(
            finder.paths[finder.matches[0]],
            "src/components/file_finder.rs"
        );
        assert!(fuzzy_score("src/file_finder.rs", "ff").is_some());
        assert!(fuzzy_score("README.md", "ff").is_none());
    }

    #[test]
    fn enter_inserts_a_unique_search_result() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(FileFinderEvent::Query("read".to_owned()));

        assert_eq!(
            finder.update(key(KeyCode::Enter)).effects,
            [FileFinderEffect::Insert("README.md".to_owned())]
        );
    }

    #[test]
    fn enter_inserts_a_directory_with_a_trailing_slash() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(FileFinderEvent::Query("components/".to_owned()));

        assert_eq!(
            finder.update(key(KeyCode::Enter)).effects,
            [FileFinderEffect::Insert("src/components/".to_owned())]
        );
    }

    #[test]
    fn tab_inserts_the_selected_search_result() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(FileFinderEvent::Query("read".to_owned()));

        assert_eq!(
            finder.update(key(KeyCode::Tab)).effects,
            [FileFinderEffect::Insert("README.md".to_owned())]
        );
    }

    #[test]
    fn arrows_navigate_results_before_selection() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(key(KeyCode::Down));

        assert_eq!(
            finder.update(key(KeyCode::Enter)).effects,
            [FileFinderEffect::Insert("src/".to_owned())]
        );
    }

    #[test]
    fn query_updates_filter_results_and_escape_dismisses() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(FileFinderEvent::Query("read".to_owned()));

        assert_eq!(finder.matches.len(), 1);
        assert_eq!(finder.paths[finder.matches[0]], "README.md");
        assert_eq!(
            finder.update(key(KeyCode::Esc)).effects,
            [FileFinderEffect::Dismiss]
        );
    }

    #[test]
    fn popup_uses_file_finder_chrome_and_selection_styling() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();

        terminal
            .draw(|frame| finder.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(4, 3)].symbol(), "╭");
        assert_eq!(buffer[(75, 16)].symbol(), "╯");
        assert_eq!(buffer[(5, 5)].symbol(), "›");
        assert_eq!(buffer[(5, 5)].fg, Theme::default().accent());
        assert!(buffer.content().chunks(80).any(|cells| {
            cells
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("Files and directories")
        }));
        assert!(buffer.content().chunks(80).any(|cells| {
            cells
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("enter/tab insert")
        }));
    }

    #[test]
    fn footer_says_when_enter_or_tab_will_insert() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(FileFinderEvent::Query("read".to_owned()));
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();

        terminal
            .draw(|frame| finder.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .chunks(80)
                .any(|cells| {
                    cells
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>()
                        .contains("enter/tab insert")
                })
        );
    }

    #[test]
    fn narrow_terminals_do_not_overflow_the_popup() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        let mut terminal = Terminal::new(TestBackend::new(3, 2)).unwrap();

        terminal
            .draw(|frame| finder.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert_eq!(terminal.backend().buffer().area.width, 3);
    }
    fn render(finder: &mut FileFinder, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| finder.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
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
    fn loading_snapshot_preserves_the_query_and_cannot_insert_stale_paths() {
        let mut finder = FileFinder::loading();
        finder.update(FileFinderEvent::Query("source".into()));
        assert!(finder.update(key(KeyCode::Enter)).effects.is_empty());
        assert!(render(&mut finder, 72, 14).contains("Finding workspace paths"));
        finder.set_paths(vec![
            "docs/".into(),
            "source/".into(),
            "source/main.rs".into(),
        ]);
        assert_eq!(finder.query, "source");
        assert_eq!(
            finder.update(key(KeyCode::Enter)).effects,
            [FileFinderEffect::Insert("source/".into())]
        );
        finder.update(FileFinderEvent::Query("missing".into()));
        assert!(render(&mut finder, 72, 14).contains("No matching paths"));
        finder.set_paths(vec![]);
        assert!(render(&mut finder, 72, 14).contains("No workspace paths found"));
    }

    #[test]
    fn mouse_uses_the_original_path_and_fixed_details_survive_tiny_layouts() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let now = std::time::Instant::now();
        let path = "日本語/very-long-directory-name/source/file.rs";
        let mut finder = FileFinder::from_paths(vec![path.into()]);
        let text = render(&mut finder, 32, 14);
        assert!(text.contains("Insert: @"));
        let area = finder.list_area;
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(finder.update_mouse(mouse, now).effects.is_empty());
        assert_eq!(
            finder
                .update_mouse(mouse, now + std::time::Duration::from_millis(100))
                .effects,
            [FileFinderEffect::Insert(path.into())]
        );
        for width in 0..20 {
            for height in 0..15 {
                render(&mut finder, width, height);
            }
        }
    }
}
