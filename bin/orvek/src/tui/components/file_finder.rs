//! Searchable workspace file picker opened from the composer.

use super::{
    choice::ChoicePicker,
    dialog::Dialog,
    node::{Component, ComponentUpdate, RenderRequest},
    typography::{ChoiceStyle, SearchField},
};
use crate::tui::{file_index::FileIndex, theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    widgets::{ListItem, Paragraph},
};
use std::{
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    sync::Arc,
};

const KEY_BINDINGS: [(&str, &str); 3] = [("↑↓", "move"), ("enter/tab", "insert"), ("esc", "close")];
const MAX_MATCHES: usize = 256;

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
    index: Option<Arc<FileIndex>>,
    error: Option<String>,
    query: String,
    choice: ChoicePicker,
    matches: Vec<usize>,
    matches_truncated: bool,
}

impl FileFinder {
    pub(super) fn loading() -> Self {
        Self {
            index: None,
            error: None,
            query: String::new(),
            choice: ChoicePicker::new(0, 1),
            matches: Vec::new(),
            matches_truncated: false,
        }
    }

    pub(super) fn with_index(index: Arc<FileIndex>) -> Self {
        let mut finder = Self::loading();
        finder.set_index(index);
        finder
    }

    #[cfg(test)]
    fn new(workspace: &std::path::Path) -> Self {
        Self::with_index(Arc::new(crate::tui::file_index::discover_file_index(
            workspace,
        )))
    }

    pub(super) fn set_index(&mut self, index: Arc<FileIndex>) {
        self.index = Some(index);
        self.error = None;
        self.refresh_matches();
    }

    pub(super) fn set_error(&mut self, error: String) {
        self.index = None;
        self.error = Some(error);
        self.refresh_matches();
    }

    pub(super) fn set_loading(&mut self) {
        self.index = None;
        self.error = None;
        self.refresh_matches();
    }

    pub(super) const fn can_retry(&self) -> bool {
        self.error.is_some()
    }

    fn select_bounded(&mut self, delta: isize) -> ComponentUpdate<FileFinderEffect> {
        if self.choice.move_by(delta) {
            ComponentUpdate::render(RenderRequest::Immediate)
        } else {
            ComponentUpdate::none()
        }
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<FileFinderEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::PageUp => self.select_bounded(
                -(isize::try_from(self.choice.area().height)
                    .unwrap_or(1)
                    .max(1)),
            ),
            KeyCode::PageDown => self.select_bounded(
                isize::try_from(self.choice.area().height)
                    .unwrap_or(1)
                    .max(1),
            ),
            KeyCode::Esc => Self::dismiss(),
            KeyCode::Enter | KeyCode::Tab => self.handle_enter(),
            KeyCode::Up => self.select_bounded(-1),
            KeyCode::Down => self.select_bounded(1),
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
        let Some(index) = self.matches.get(self.choice.selected_or_zero()) else {
            return ComponentUpdate::none();
        };
        let Some(path) = self
            .index
            .as_ref()
            .and_then(|file_index| file_index.entries().get(*index))
            .map(|entry| entry.path().to_owned())
        else {
            return ComponentUpdate::none();
        };
        ComponentUpdate {
            effects: vec![FileFinderEffect::Insert(path)],
            render: RenderRequest::Immediate,
        }
    }

    fn refresh_matches(&mut self) {
        let Some(index) = &self.index else {
            self.matches.clear();
            self.matches_truncated = false;
            self.choice.reset(0);
            return;
        };
        if self.query.is_empty() {
            self.matches = (0..index.entries().len().min(MAX_MATCHES)).collect();
            self.matches_truncated = index.entries().len() > MAX_MATCHES;
            self.choice.reset(self.matches.len());
            return;
        }

        let query = self.query.to_ascii_lowercase();
        let mut best = BinaryHeap::with_capacity(MAX_MATCHES + 1);
        let mut match_count = 0_usize;
        for (entry_index, entry) in index.entries().iter().enumerate() {
            let Some(score) = fuzzy_score_normalized(entry.search_text(), &query) else {
                continue;
            };
            match_count += 1;
            let ranked = (Reverse(score), entry_index);
            if best.len() < MAX_MATCHES {
                best.push(ranked);
            } else if best.peek().is_some_and(|worst| ranked < *worst) {
                best.pop();
                best.push(ranked);
            }
        }

        let mut matches = best
            .into_iter()
            .map(|(Reverse(score), entry_index)| (entry_index, score))
            .collect::<Vec<_>>();
        matches.sort_unstable_by(|left, right| compare_matches(index, left, right));
        self.matches = matches
            .into_iter()
            .map(|(entry_index, _)| entry_index)
            .collect();
        self.matches_truncated = match_count > MAX_MATCHES;
        self.choice.reset(self.matches.len());
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        SearchField::new(&self.query).render(frame, area, theme);
    }

    fn render_paths(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(format!(
                    "  Could not index workspace paths.\n\n  {error}\n\n  Press r to retry or Esc to close."
                ))
                .style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let Some(index) = &self.index else {
            frame.render_widget(
                Paragraph::new("  Indexing workspace…").style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        };
        if self.matches.is_empty() {
            frame.render_widget(
                Paragraph::new("  No matching paths").style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }

        let items = self
            .matches
            .iter()
            .enumerate()
            .map(|(position, entry_index)| {
                let typography = ChoiceStyle::new(self.choice.is_selected(position), true);
                ListItem::new(index.entries()[*entry_index].path()).style(typography.primary(theme))
            });
        self.choice
            .render(frame, area, items.collect(), true, theme);
    }
}

impl Component for FileFinder {
    type Event = FileFinderEvent;
    type Effect = FileFinderEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            FileFinderEvent::Terminal(Event::Key(key)) => self.update_key(key),
            FileFinderEvent::Terminal(Event::Mouse(mouse))
                if self.choice.contains(Position::new(mouse.column, mouse.row)) =>
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
        self.choice.set_area(Rect::default());
        if area.is_empty() {
            return;
        }

        let title = match (
            self.index
                .as_ref()
                .is_some_and(|index| index.is_truncated()),
            self.matches_truncated,
        ) {
            (true, true) => "Files and directories (index/results capped)",
            (true, false) => "Files and directories (index capped)",
            (false, true) => "Files and directories (best 256)",
            (false, false) => "Files and directories",
        };
        let layout = Dialog::new(title, 72, 14, &KEY_BINDINGS).render(frame, area, theme);
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
        self.choice.set_area(paths_area);
        self.render_search(frame, search_area, theme);
        self.render_paths(frame, paths_area, theme);
    }
}

fn compare_matches(index: &FileIndex, left: &(usize, usize), right: &(usize, usize)) -> Ordering {
    Reverse(left.1).cmp(&Reverse(right.1)).then_with(|| {
        index.entries()[left.0]
            .path()
            .cmp(index.entries()[right.0].path())
    })
}

pub(super) fn fuzzy_score(path: &str, query: &str) -> Option<usize> {
    fuzzy_score_normalized(&path.to_ascii_lowercase(), query)
}

fn fuzzy_score_normalized(path: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }

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
        Component, FileFinder, FileFinderEffect, FileFinderEvent, MAX_MATCHES, fuzzy_score,
    };
    use crate::tui::{file_index::FileIndex, theme::Theme};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use std::{fs, sync::Arc};

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
    fn fuzzy_search_matches_non_contiguous_characters_and_ranks_tight_matches_first() {
        let workspace = workspace();
        let mut finder = FileFinder::new(workspace.path());
        finder.update(FileFinderEvent::Query("ff".to_owned()));

        assert_eq!(finder.matches.len(), 1);
        assert_eq!(
            finder.index.as_ref().unwrap().entries()[finder.matches[0]].path(),
            "src/components/file_finder.rs"
        );
        assert!(fuzzy_score("src/file_finder.rs", "ff").is_some());
        assert!(fuzzy_score("README.md", "ff").is_none());
    }

    #[test]
    fn broad_search_bounds_ranked_results_and_discloses_the_cap() {
        let paths = (0..300)
            .map(|index| format!("matching-file-{index:03}.rs"))
            .collect();
        let mut finder = FileFinder::with_index(Arc::new(FileIndex::from_paths(paths, false)));
        finder.update(FileFinderEvent::Query("match".to_owned()));

        assert_eq!(finder.matches.len(), MAX_MATCHES);
        assert!(finder.matches_truncated);

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| finder.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        assert!(terminal.backend().buffer().content().chunks(80).any(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("Files and directories (best 256)")
        }));
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
        assert_eq!(
            finder.index.as_ref().unwrap().entries()[finder.matches[0]].path(),
            "README.md"
        );
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
        let body = picker.choice.area();
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.choice.selected_or_zero(), 1);
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
        picker.update(FileFinderEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            picker.choice.selected_or_zero(),
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
        assert_eq!(picker.choice.selected_or_zero(), last);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.choice.selected_or_zero(), last);
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        assert_eq!(picker.choice.selected_or_zero(), last);
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
    }
}
