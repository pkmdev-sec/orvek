//! Searchable workspace file picker opened from the composer.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
    typography::{CHOICE_MARKER, ChoiceStyle, SearchField},
};
use crate::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    widgets::{List, ListItem, ListState},
};
use std::{cmp::Reverse, fs, path::Path};

const KEY_BINDINGS: [(&str, &str); 3] = [("↑↓", "move"), ("enter/tab", "insert"), ("esc", "close")];
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
    navigation_area: Rect,
    matches: Vec<usize>,
}

impl FileFinder {
    pub(super) fn new(workspace: &Path) -> Self {
        let paths = discover_paths(workspace);
        let matches = (0..paths.len()).collect();
        Self {
            paths,
            query: String::new(),
            selected: 0,
            navigation_area: Rect::default(),
            matches,
        }
    }

    fn select_bounded(&mut self, delta: isize) -> ComponentUpdate<FileFinderEffect> {
        let next = self
            .selected
            .saturating_add_signed(delta)
            .min(self.matches.len().saturating_sub(1));
        if next == self.selected {
            return ComponentUpdate::none();
        }
        self.selected = next;
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<FileFinderEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::PageUp => self.select_bounded(
                -(isize::try_from(self.navigation_area.height)
                    .unwrap_or(1)
                    .max(1)),
            ),
            KeyCode::PageDown => self.select_bounded(
                isize::try_from(self.navigation_area.height)
                    .unwrap_or(1)
                    .max(1),
            ),
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
        SearchField::new(&self.query).render(frame, area, theme);
    }

    fn render_paths(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let items = self.matches.iter().enumerate().map(|(position, index)| {
            let typography = ChoiceStyle::new(position == self.selected, true);
            ListItem::new(self.paths[*index].as_str()).style(typography.primary(theme))
        });
        let list = List::new(items)
            .highlight_style(ChoiceStyle::new(true, true).highlight(theme))
            .highlight_symbol(CHOICE_MARKER);
        let selected = (!self.matches.is_empty()).then_some(self.selected);
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(list, area, &mut state);
    }
}

impl Component for FileFinder {
    type Event = FileFinderEvent;
    type Effect = FileFinderEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            FileFinderEvent::Terminal(Event::Key(key)) => self.update_key(key),
            FileFinderEvent::Terminal(Event::Mouse(mouse))
                if self
                    .navigation_area
                    .contains(Position::new(mouse.column, mouse.row)) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.select_bounded(-1),
                    MouseEventKind::ScrollDown => self.select_bounded(1),
                    _ => ComponentUpdate::none(),
                }
            }
            FileFinderEvent::Terminal(_) => ComponentUpdate::none(),
            FileFinderEvent::Query(query) => self.set_query(query),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.navigation_area = Rect::default();
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
        let paths_area = Rect {
            y: layout.body.y + 1,
            height: layout.body.height.saturating_sub(1),
            ..layout.body
        };
        self.navigation_area = paths_area;
        self.render_search(frame, search_area, theme);
        self.render_paths(frame, paths_area, theme);
    }
}

fn discover_paths(workspace: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    visit_directory(workspace, workspace, &mut paths);
    paths.sort_unstable();
    paths
}

fn visit_directory(workspace: &Path, directory: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|entry| entry.file_name());

    for entry in entries {
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
            visit_directory(workspace, &path, paths);
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
    #[test]
    fn rendered_picker_bounds_wheel_and_page_navigation() {
        let workspace = tempfile::tempdir().unwrap();
        for index in 0..30 {
            std::fs::write(workspace.path().join(format!("file-{index:02}")), "").unwrap();
        }
        let mut picker = FileFinder::new(workspace.path());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        let body = picker.navigation_area;
        assert!(!body.is_empty());
        let mouse = |kind, column, row| {
            FileFinderEvent::Terminal(Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }))
        };
        picker.update(mouse(crossterm::event::MouseEventKind::ScrollDown, 0, 0));
        assert_eq!(picker.selected, 0);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, 1);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollUp,
            body.x,
            body.y,
        ));
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollUp,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, 0);
        picker.update(FileFinderEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            picker.selected,
            picker
                .matches
                .len()
                .saturating_sub(1)
                .min(usize::from(body.height).max(1))
        );
        for _ in 0..40 {
            picker.update(FileFinderEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::PageDown,
                KeyModifiers::NONE,
            ))));
        }
        let last = picker.matches.len().saturating_sub(1);
        assert_eq!(picker.selected, last);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, last);
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        assert_eq!(picker.selected, last);
        let buffer = terminal.backend().buffer();
        assert!((body.y..body.bottom()).any(|row| {
            let text = (body.x..body.right())
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>();
            text.contains("› ") && text.contains("file-29")
        }));
        for _ in 0..40 {
            picker.update(FileFinderEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::PageUp,
                KeyModifiers::NONE,
            ))));
        }
        assert_eq!(picker.selected, 0);
        terminal
            .draw(|frame| {
                picker.render(
                    frame,
                    ratatui::layout::Rect::default(),
                    &crate::tui::theme::Theme::default(),
                )
            })
            .unwrap();
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, 0);
    }
}
