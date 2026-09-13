//! Searchable, read-only inspection and explicit deletion of stored memories.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    format::{
        format_age, sanitize_terminal_text, sanitize_terminal_text_inline, wrap_display_lines,
    },
    theme::Theme,
};
use chrono::{DateTime, Utc};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use orvek_memory::{MemoryAccess, MemoryKey, MemoryRecord, MemorySource, RemoteRole};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const LIST_KEYS: [(&str, &str); 6] = [
    ("↑↓", "move"),
    ("enter", "inspect"),
    ("ctrl+s", "sort"),
    ("ctrl+d", "remove"),
    ("ctrl+r", "refresh"),
    ("esc", "close"),
];
const REMOTE_LIST_KEYS: [(&str, &str); 6] = [
    ("↑↓", "move"),
    ("enter", "inspect"),
    ("ctrl+s", "sort"),
    ("ctrl+n", "namespaces"),
    ("ctrl+r", "refresh"),
    ("esc", "close"),
];
const REMOTE_WRITABLE_LIST_KEYS: [(&str, &str); 7] = [
    ("↑↓", "move"),
    ("enter", "inspect"),
    ("ctrl+s", "sort"),
    ("ctrl+n", "namespaces"),
    ("ctrl+d", "remove"),
    ("ctrl+r", "refresh"),
    ("esc", "close"),
];
const DETAIL_KEYS: [(&str, &str); 4] = [
    ("↑↓/pgup/pgdn", "scroll"),
    ("d", "delete"),
    ("r", "refresh"),
    ("esc", "back"),
];
const REMOTE_DETAIL_KEYS: [(&str, &str); 3] = [
    ("↑↓/pgup/pgdn", "scroll"),
    ("r", "refresh"),
    ("esc", "back"),
];
const CONFIRM_KEYS: [(&str, &str); 3] =
    [("↑↓", "scroll"), ("d/delete", "confirm"), ("esc", "cancel")];
const DELETING_KEYS: [(&str, &str); 1] = [("", "deleting…")];
const LOAD_ERROR_KEYS: [(&str, &str); 3] = [("↑↓", "scroll"), ("r", "retry"), ("esc", "close")];
const DELETE_ERROR_KEYS: [(&str, &str); 4] = [
    ("↑↓", "scroll"),
    ("d/delete", "retry"),
    ("r", "refresh"),
    ("esc", "back"),
];
const LOADING_KEYS: [(&str, &str); 2] = [("r", "retry"), ("esc", "close")];
const FILTER_LABEL: &str = " Filter: ";
const MAX_PREVIEW_GRAPHEMES: usize = 160;

pub(super) enum MemoryBrowserEvent {
    Terminal(Event),
    Loaded {
        access: MemoryAccess,
        records: Vec<MemoryRecord>,
    },
    LoadFailed {
        source: MemorySource,
        access: Option<MemoryAccess>,
        error: String,
    },
    Deleted {
        key: MemoryKey,
    },
    DeleteFailed {
        error: String,
        conflict: bool,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum MemoryBrowserEffect {
    Dismiss,
    Refresh,
    Delete(MemoryKey),
}

pub(super) struct MemoryBrowser {
    access: Option<MemoryAccess>,
    source: MemorySource,
    records: Vec<MemoryRecord>,
    query: String,
    matches: Vec<usize>,
    selected_key: Option<MemoryKey>,
    sort: SortMode,
    namespace_scope: NamespaceScope,
    state: BrowserState,
    overlay_scroll: u16,
    max_scroll: u16,
    body: Rect,
    list_area: Rect,
    offset: usize,
    last_click: Option<(MemoryKey, Instant)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SortMode {
    MostUseful,
    Newest,
    Oldest,
    LeastUseful,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamespaceScope {
    All,
    Own,
}

impl NamespaceScope {
    const fn next(self) -> Self {
        match self {
            Self::All => Self::Own,
            Self::Own => Self::All,
        }
    }
}

impl SortMode {
    const fn next(self) -> Self {
        match self {
            Self::MostUseful => Self::Newest,
            Self::Newest => Self::Oldest,
            Self::Oldest => Self::LeastUseful,
            Self::LeastUseful => Self::MostUseful,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::MostUseful => "Most useful",
            Self::Newest => "Newest",
            Self::Oldest => "Oldest",
            Self::LeastUseful => "Least useful",
        }
    }

    fn compare(self, left: &MemoryRecord, right: &MemoryRecord) -> std::cmp::Ordering {
        match self {
            Self::MostUseful => right
                .use_count
                .cmp(&left.use_count)
                .then_with(|| compare_newest(left, right)),
            Self::Newest => compare_newest(left, right),
            Self::Oldest => compare_oldest(left, right),
            Self::LeastUseful => left
                .use_count
                .cmp(&right.use_count)
                .then_with(|| compare_oldest(left, right)),
        }
    }
}

#[derive(Clone)]
enum BrowserState {
    Loading,
    Error(BrowserError),
    List,
    Detail {
        key: MemoryKey,
        scroll: u16,
    },
    ConfirmDelete {
        key: MemoryKey,
        return_to: ReturnView,
    },
    Deleting {
        key: MemoryKey,
        return_to: ReturnView,
    },
}

#[derive(Clone)]
struct BrowserError {
    message: String,
    action: ErrorAction,
}

#[derive(Clone)]
enum ErrorAction {
    Load,
    Delete {
        key: MemoryKey,
        return_to: ReturnView,
    },
}

#[derive(Clone, Copy)]
enum ReturnView {
    List,
    Detail { scroll: u16 },
}

impl MemoryBrowser {
    pub(super) const fn new() -> Self {
        Self {
            access: None,
            source: MemorySource::Local,
            records: Vec::new(),
            query: String::new(),
            matches: Vec::new(),
            selected_key: None,
            sort: SortMode::MostUseful,
            namespace_scope: NamespaceScope::All,
            state: BrowserState::Loading,
            overlay_scroll: 0,
            max_scroll: 0,
            body: Rect::new(0, 0, 0, 0),
            list_area: Rect::new(0, 0, 0, 0),
            offset: 0,
            last_click: None,
        }
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<MemoryBrowserEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        self.last_click = None;
        if matches!(
            self.state,
            BrowserState::ConfirmDelete { .. } | BrowserState::Error(_)
        ) {
            let scroll = match key.code {
                KeyCode::Up => Some(self.overlay_scroll.saturating_sub(1)),
                KeyCode::Down => Some(self.overlay_scroll.saturating_add(1)),
                KeyCode::PageUp => {
                    Some(self.overlay_scroll.saturating_sub(self.body.height.max(1)))
                }
                KeyCode::PageDown => {
                    Some(self.overlay_scroll.saturating_add(self.body.height.max(1)))
                }
                KeyCode::Home => Some(0),
                KeyCode::End => Some(self.max_scroll),
                _ => None,
            };
            if let Some(scroll) = scroll {
                self.overlay_scroll = scroll.min(self.max_scroll);
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
        }
        match self.state.clone() {
            BrowserState::Loading => self.update_loading(key),
            BrowserState::Error(error) => self.update_error(key, error.action),
            BrowserState::List => self.update_list(key),
            BrowserState::Detail {
                key: memory_key,
                scroll,
            } => self.update_detail(key, memory_key, scroll),
            BrowserState::ConfirmDelete {
                key: memory_key,
                return_to,
            } => self.update_confirmation(key, memory_key, return_to),
            BrowserState::Deleting { .. } => ComponentUpdate::none(),
        }
    }

    fn update_loading(&mut self, key: KeyEvent) -> ComponentUpdate<MemoryBrowserEffect> {
        match key.code {
            KeyCode::Esc => Self::effect(MemoryBrowserEffect::Dismiss),
            KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => self.refresh(),
            _ => ComponentUpdate::none(),
        }
    }

    fn update_error(
        &mut self,
        key: KeyEvent,
        action: ErrorAction,
    ) -> ComponentUpdate<MemoryBrowserEffect> {
        match (action, key.code) {
            (_, KeyCode::Char('r')) if key.modifiers == KeyModifiers::NONE => self.refresh(),
            (ErrorAction::Load, KeyCode::Esc) => Self::effect(MemoryBrowserEffect::Dismiss),
            (ErrorAction::Delete { return_to, .. }, KeyCode::Esc) => {
                self.restore(return_to);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            (
                ErrorAction::Delete {
                    key: memory_key,
                    return_to,
                },
                KeyCode::Char('d') | KeyCode::Delete,
            ) if key.kind == KeyEventKind::Press => self.delete(memory_key, return_to),
            _ => ComponentUpdate::none(),
        }
    }

    fn update_list(&mut self, key: KeyEvent) -> ComponentUpdate<MemoryBrowserEffect> {
        match key.code {
            KeyCode::Esc => Self::effect(MemoryBrowserEffect::Dismiss),
            KeyCode::Up => self.move_selection(false),
            KeyCode::Down => self.move_selection(true),
            KeyCode::Enter | KeyCode::Tab => self.inspect_selected(),
            KeyCode::Backspace if !self.query.is_empty() => {
                if let Some((index, _)) = self.query.grapheme_indices(true).next_back() {
                    self.query.truncate(index);
                    self.refresh_matches();
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Char('s') if key.modifiers == KeyModifiers::CONTROL => self.cycle_sort(),
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                self.cycle_namespace_scope()
            }
            KeyCode::Char('r') if key.modifiers == KeyModifiers::CONTROL => self.refresh(),
            KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
                self.confirm_selected(ReturnView::List)
            }
            KeyCode::Delete if key.kind == KeyEventKind::Press => {
                self.confirm_selected(ReturnView::List)
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !character.is_control() =>
            {
                self.query.push(character);
                self.refresh_matches();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn update_detail(
        &mut self,
        key: KeyEvent,
        memory_key: MemoryKey,
        scroll: u16,
    ) -> ComponentUpdate<MemoryBrowserEffect> {
        let next_scroll = match key.code {
            KeyCode::Up => Some(scroll.saturating_sub(1)),
            KeyCode::Down => Some(scroll.saturating_add(1)),
            KeyCode::PageUp => Some(scroll.saturating_sub(10)),
            KeyCode::PageDown => Some(scroll.saturating_add(10)),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(self.max_scroll),
            _ => None,
        };
        if let Some(scroll) = next_scroll {
            self.state = BrowserState::Detail {
                key: memory_key,
                scroll: scroll.min(self.max_scroll),
            };
            return ComponentUpdate::render(RenderRequest::Immediate);
        }

        match key.code {
            KeyCode::Esc => {
                self.state = BrowserState::List;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => self.refresh(),
            KeyCode::Char('d') | KeyCode::Delete if key.kind == KeyEventKind::Press => {
                if !self.can_delete(&memory_key) {
                    return ComponentUpdate::none();
                }
                self.overlay_scroll = 0;
                self.state = BrowserState::ConfirmDelete {
                    key: memory_key,
                    return_to: ReturnView::Detail { scroll },
                };
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn update_confirmation(
        &mut self,
        key: KeyEvent,
        memory_key: MemoryKey,
        return_to: ReturnView,
    ) -> ComponentUpdate<MemoryBrowserEffect> {
        match key.code {
            KeyCode::Esc => {
                self.restore(return_to);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Char('d') | KeyCode::Delete if key.kind == KeyEventKind::Press => {
                self.delete(memory_key, return_to)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<MemoryBrowserEffect> {
        if !matches!(self.state, BrowserState::List) {
            return ComponentUpdate::none();
        }
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.refresh_matches();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn refresh(&mut self) -> ComponentUpdate<MemoryBrowserEffect> {
        self.overlay_scroll = 0;
        self.list_area = Rect::default();
        self.state = BrowserState::Loading;
        Self::effect(MemoryBrowserEffect::Refresh)
    }

    fn inspect_selected(&mut self) -> ComponentUpdate<MemoryBrowserEffect> {
        let Some(key) = self.selected_key.clone() else {
            return ComponentUpdate::none();
        };
        self.state = BrowserState::Detail { key, scroll: 0 };
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn confirm_selected(&mut self, return_to: ReturnView) -> ComponentUpdate<MemoryBrowserEffect> {
        let Some(key) = self.selected_key.clone() else {
            return ComponentUpdate::none();
        };
        if !self.can_delete(&key) {
            return ComponentUpdate::none();
        }
        self.overlay_scroll = 0;
        self.state = BrowserState::ConfirmDelete { key, return_to };
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn delete(
        &mut self,
        key: MemoryKey,
        return_to: ReturnView,
    ) -> ComponentUpdate<MemoryBrowserEffect> {
        if !self.can_delete(&key) {
            return ComponentUpdate::none();
        }
        let Some(record) = self.records.iter().find(|record| record.key == key) else {
            self.state = BrowserState::List;
            self.refresh_matches();
            return ComponentUpdate::render(RenderRequest::Immediate);
        };
        let key = record.key.clone();
        self.state = BrowserState::Deleting {
            key: key.clone(),
            return_to,
        };
        Self::effect(MemoryBrowserEffect::Delete(key))
    }

    fn restore(&mut self, return_to: ReturnView) {
        self.state = match return_to {
            ReturnView::List => BrowserState::List,
            ReturnView::Detail { scroll } => {
                let Some(key) = self.selected_key.clone() else {
                    return self.state = BrowserState::List;
                };
                BrowserState::Detail { key, scroll }
            }
        };
    }

    fn replace_records(&mut self, access: MemoryAccess, records: Vec<MemoryRecord>) {
        let fallback = self.selected_match_index().unwrap_or_default();
        self.source = access.source;
        self.access = Some(access);
        self.records = records;
        self.rebuild_matches(fallback);
        self.state = BrowserState::List;
    }

    fn remove_record(&mut self, key: &MemoryKey) {
        let fallback = self.selected_match_index().unwrap_or_default();
        self.records.retain(|record| record.key != *key);
        self.rebuild_matches(fallback);
        self.state = BrowserState::List;
    }

    fn refresh_matches(&mut self) {
        let fallback = self.selected_match_index().unwrap_or_default();
        self.rebuild_matches(fallback);
    }

    fn cycle_sort(&mut self) -> ComponentUpdate<MemoryBrowserEffect> {
        let fallback = self.selected_match_index().unwrap_or_default();
        self.sort = self.sort.next();
        self.rebuild_matches(fallback);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn cycle_namespace_scope(&mut self) -> ComponentUpdate<MemoryBrowserEffect> {
        if !self.is_remote() {
            return ComponentUpdate::none();
        }
        let fallback = self.selected_match_index().unwrap_or_default();
        self.namespace_scope = self.namespace_scope.next();
        self.rebuild_matches(fallback);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn rebuild_matches(&mut self, fallback: usize) {
        self.list_area = Rect::default();
        self.last_click = None;
        let query = self.query.to_lowercase();
        self.matches = self
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| self.namespace_matches(record))
            .filter(|(_, record)| record_matches(record, &query))
            .map(|(index, _)| index)
            .collect();
        self.matches.sort_by(|left, right| {
            self.sort
                .compare(&self.records[*left], &self.records[*right])
        });

        if self.selected_match_index().is_some() {
            return;
        }
        self.selected_key = self
            .matches
            .get(fallback.min(self.matches.len().saturating_sub(1)))
            .map(|index| self.records[*index].key.clone());
    }

    fn selected_match_index(&self) -> Option<usize> {
        let selected_key = self.selected_key.as_ref()?;
        self.matches
            .iter()
            .position(|index| self.records[*index].key == *selected_key)
    }

    fn move_selection(&mut self, down: bool) -> ComponentUpdate<MemoryBrowserEffect> {
        self.last_click = None;
        if self.matches.is_empty() {
            return ComponentUpdate::none();
        }
        let current = self.selected_match_index().unwrap_or_default();
        let next = if down {
            current.saturating_add(1).min(self.matches.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.selected_key = Some(self.records[self.matches[next]].key.clone());
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn effect(effect: MemoryBrowserEffect) -> ComponentUpdate<MemoryBrowserEffect> {
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }

    fn can_delete(&self, key: &MemoryKey) -> bool {
        self.access
            .as_ref()
            .is_some_and(|access| match access.source {
                MemorySource::Local => key.is_local(),
                MemorySource::Remote => {
                    access.role == Some(RemoteRole::Writer)
                        && key.namespace.as_deref() == access.namespace.as_deref()
                }
            })
    }

    fn is_remote(&self) -> bool {
        self.access
            .as_ref()
            .is_some_and(|access| access.source == MemorySource::Remote)
            || self.source == MemorySource::Remote
    }

    fn namespace_matches(&self, record: &MemoryRecord) -> bool {
        self.namespace_scope == NamespaceScope::All
            || !self.is_remote()
            || self
                .access
                .as_ref()
                .and_then(|access| access.namespace.as_deref())
                .is_some_and(|namespace| record.key.namespace.as_deref() == Some(namespace))
    }

    fn namespace_scope_label(&self) -> Option<String> {
        if !self.is_remote() {
            return None;
        }
        Some(match self.namespace_scope {
            NamespaceScope::All => "All namespaces".to_owned(),
            NamespaceScope::Own => self
                .access
                .as_ref()
                .and_then(|access| access.namespace.clone())
                .unwrap_or_else(|| "Our namespace".to_owned()),
        })
    }

    fn context_label(&self) -> String {
        match self.access.as_ref() {
            Some(MemoryAccess {
                source: MemorySource::Remote,
                namespace: Some(namespace),
                ..
            }) => format!(
                "Remote memory · {}",
                sanitize_terminal_text_inline(namespace)
            ),
            Some(MemoryAccess {
                source: MemorySource::Remote,
                ..
            }) => "Remote memory".to_owned(),
            _ if self.source == MemorySource::Remote => "Remote memory".to_owned(),
            _ => "Local memory".to_owned(),
        }
    }

    fn footer(&self) -> &'static [(&'static str, &'static str)] {
        match &self.state {
            BrowserState::Loading => &LOADING_KEYS,
            BrowserState::Error(error) => match error.action {
                ErrorAction::Load => &LOAD_ERROR_KEYS,
                ErrorAction::Delete { .. } => &DELETE_ERROR_KEYS,
            },
            BrowserState::List if self.is_remote() => match self
                .selected_key
                .as_ref()
                .is_some_and(|key| self.can_delete(key))
            {
                true => &REMOTE_WRITABLE_LIST_KEYS,
                false => &REMOTE_LIST_KEYS,
            },
            BrowserState::List => &LIST_KEYS,
            BrowserState::Detail { key, .. } if self.can_delete(key) => &DETAIL_KEYS,
            BrowserState::Detail { .. } => &REMOTE_DETAIL_KEYS,
            BrowserState::ConfirmDelete { .. } => &CONFIRM_KEYS,
            BrowserState::Deleting { .. } => &DELETING_KEYS,
        }
    }

    fn update_mouse(
        &mut self,
        mouse: MouseEvent,
        now: Instant,
    ) -> ComponentUpdate<MemoryBrowserEffect> {
        let point = Position::new(mouse.column, mouse.row);
        if matches!(self.state, BrowserState::List) && self.list_area.contains(point) {
            match mouse.kind {
                MouseEventKind::ScrollUp => return self.move_selection(false),
                MouseEventKind::ScrollDown => return self.move_selection(true),
                MouseEventKind::Down(MouseButton::Left) => {
                    let index = self.offset + usize::from((mouse.row - self.list_area.y) / 2);
                    let Some(record) = self.matches.get(index).map(|index| &self.records[*index])
                    else {
                        return ComponentUpdate::none();
                    };
                    let key = record.key.clone();
                    let inspect = self.last_click.as_ref().is_some_and(|(previous, at)| {
                        *previous == key
                            && now.saturating_duration_since(*at) <= Duration::from_millis(500)
                    });
                    self.selected_key = Some(key.clone());
                    self.last_click = Some((key, now));
                    if inspect {
                        self.last_click = None;
                        return self.inspect_selected();
                    }
                    return ComponentUpdate::render(RenderRequest::Immediate);
                }
                _ => return ComponentUpdate::none(),
            }
        }
        if self.body.contains(point) {
            let code = match mouse.kind {
                MouseEventKind::ScrollUp => KeyCode::Up,
                MouseEventKind::ScrollDown => KeyCode::Down,
                _ => return ComponentUpdate::none(),
            };
            return self.update_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        ComponentUpdate::none()
    }

    fn render_list(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        self.render_filter(frame, Rect { height: 1, ..area }, theme);
        let mut metadata = vec![
            if self.is_remote() {
                let role = match self.access.as_ref().and_then(|access| access.role) {
                    Some(RemoteRole::Reader) => "reader",
                    Some(RemoteRole::Writer) => "writer",
                    None => "unknown",
                };
                let namespace = self
                    .access
                    .as_ref()
                    .and_then(|access| access.namespace.as_deref())
                    .unwrap_or("unknown namespace");
                format!(
                    " Access: {role} · {}",
                    sanitize_terminal_text_inline(namespace)
                )
            } else {
                " Shared across sessions".to_owned()
            },
            format!(" Sort: {}", self.sort.label()),
        ];
        if let Some(scope) = self.namespace_scope_label() {
            metadata.push(format!(" Namespaces: {scope}"));
        }
        let metadata = metadata
            .into_iter()
            .flat_map(|text| wrap_display_lines(&text, usize::from(area.width)))
            .collect::<Vec<_>>();
        let context_height = (metadata.len() as u16).min(area.height.saturating_sub(4));
        frame.render_widget(
            Paragraph::new(
                metadata
                    .into_iter()
                    .take(usize::from(context_height))
                    .map(Line::from)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(theme.muted())),
            Rect::new(area.x, area.y + 1, area.width, context_height),
        );
        let header_height = (context_height + 1 + u16::from(context_height > 0)).min(area.height);
        let detail_height = 2.min(area.height.saturating_sub(header_height + 2));
        let list = Rect::new(
            area.x,
            area.y + header_height,
            area.width,
            area.height.saturating_sub(header_height + detail_height),
        );
        self.render_records(frame, list, theme);
        if detail_height > 0 {
            let identity = self.selected_key.as_ref().map_or_else(
                || format!(" Loaded: {}", self.records.len()),
                |key| {
                    format!(
                        " Selected: {}#{} · v{}",
                        key.namespace.as_deref().unwrap_or("local"),
                        key.id,
                        key.version
                    )
                },
            );
            let permission = self.selected_key.as_ref().map_or("", |key| {
                if self.can_delete(key) {
                    " Removal needs confirmation."
                } else {
                    " Read-only record."
                }
            });
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(fit_width(
                        &sanitize_terminal_text_inline(&identity),
                        usize::from(area.width),
                    )),
                    Line::from(permission),
                ])
                .style(Style::default().fg(theme.muted())),
                Rect::new(
                    area.x,
                    area.bottom() - detail_height,
                    area.width,
                    detail_height,
                ),
            );
        }
    }

    fn render_filter(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let width = usize::from(area.width).saturating_sub(FILTER_LABEL.width());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(FILTER_LABEL, Style::default().fg(theme.muted())),
                Span::styled(
                    visible_tail(&self.query, width),
                    Style::default().fg(theme.text()),
                ),
            ])),
            area,
        );
    }

    fn render_records(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect {
            height: area.height / 2 * 2,
            ..area
        };
        if area.is_empty() {
            return;
        }
        if self.records.is_empty() {
            frame.render_widget(
                Paragraph::new(format!(
                    " {} is empty. Press Ctrl+R to refresh.",
                    self.context_label()
                ))
                .style(Style::default().fg(theme.muted()))
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        if self.matches.is_empty() {
            let message = if self.query.is_empty() {
                format!(
                    " No memories in {}.",
                    self.namespace_scope_label().unwrap_or_default()
                )
            } else {
                format!(" No memories match “{}”.", self.query)
            };
            frame.render_widget(
                Paragraph::new(fit_width(
                    &sanitize_terminal_text_inline(&message),
                    usize::from(area.width),
                ))
                .style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let capacity = usize::from(self.list_area.height / 2);
        if capacity == 0 {
            return;
        }
        let selected = self.selected_match_index().unwrap_or(0);
        self.offset = self
            .offset
            .min(self.matches.len().saturating_sub(capacity))
            .min(selected);
        if selected >= self.offset + capacity {
            self.offset = selected + 1 - capacity;
        }
        let width = usize::from(area.width).saturating_sub(2);
        for (row, index) in self
            .matches
            .iter()
            .skip(self.offset)
            .take(capacity)
            .enumerate()
        {
            let record = &self.records[*index];
            let active = self.offset + row == selected;
            let style = Style::default()
                .fg(if active { theme.accent() } else { theme.text() })
                .add_modifier(Modifier::BOLD);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled(if active { "› " } else { "  " }, style),
                        Span::styled(bounded_preview(&record.content, width), style),
                    ]),
                    Line::styled(
                        format!(
                            "  {}",
                            fit_width(
                                &sanitize_terminal_text_inline(&list_metadata(record)),
                                width
                            )
                        ),
                        Style::default().fg(theme.muted()),
                    ),
                ]),
                Rect::new(area.x, area.y + row as u16 * 2, area.width, 2),
            );
        }
    }

    fn render_document(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        lines: Vec<Line<'static>>,
        scroll: u16,
    ) -> u16 {
        let rows = lines
            .into_iter()
            .flat_map(|line| {
                line.spans
                    .into_iter()
                    .flat_map(|span| {
                        wrap_display_lines(&span.content, usize::from(area.width))
                            .into_iter()
                            .map(move |text| Line::from(vec![Span::styled(text, span.style)]))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        self.max_scroll = rows
            .len()
            .saturating_sub(usize::from(area.height))
            .min(usize::from(u16::MAX)) as u16;
        let scroll = scroll.min(self.max_scroll);
        frame.render_widget(Paragraph::new(rows).scroll((scroll, 0)), area);
        scroll
    }

    fn render_detail(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        key: MemoryKey,
        scroll: u16,
    ) {
        let Some(record) = self.records.iter().find(|record| record.key == key) else {
            self.state = BrowserState::List;
            self.render_list(frame, area, theme);
            return;
        };
        let lines = detail_lines(record, theme);
        let scroll = self.render_document(frame, area, lines, scroll);
        self.state = BrowserState::Detail { key, scroll };
    }

    fn render_confirmation(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        key: &MemoryKey,
        deleting: bool,
    ) {
        let mut lines = vec![
            Line::styled(
                if deleting {
                    "Deletion requested…"
                } else {
                    "Delete this stored memory?"
                },
                Style::default().fg(theme.accent()),
            ),
            Line::default(),
        ];
        lines.extend(identity_lines(key, theme));
        lines.push(Line::default());
        if let Some(record) = self.records.iter().find(|record| record.key == *key) {
            lines.extend(
                sanitize_detail(&record.content)
                    .split('\n')
                    .map(|line| Line::styled(line.to_owned(), Style::default().fg(theme.text()))),
            );
        }
        self.overlay_scroll = self.render_document(frame, area, lines, self.overlay_scroll);
    }

    fn render_error(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        error: &BrowserError,
    ) {
        let mut lines = vec![
            Line::styled(
                match error.action {
                    ErrorAction::Load => "Could not load memories.",
                    ErrorAction::Delete { .. } => "Deletion was not confirmed.",
                },
                Style::default().fg(theme.accent()),
            ),
            Line::from(
                error
                    .message
                    .split('\n')
                    .map(|line| {
                        Span::styled(
                            sanitize_terminal_text(line).into_owned(),
                            Style::default().fg(theme.text()),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            Line::default(),
            Line::styled(
                format!("Source: {}", self.context_label()),
                Style::default().fg(theme.muted()),
            ),
        ];
        if let ErrorAction::Delete { key, .. } = &error.action {
            lines.extend(identity_lines(key, theme));
        }
        self.overlay_scroll = self.render_document(frame, area, lines, self.overlay_scroll);
    }
}

impl Component for MemoryBrowser {
    type Event = MemoryBrowserEvent;
    type Effect = MemoryBrowserEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            MemoryBrowserEvent::Terminal(Event::Key(key)) => self.update_key(key),
            MemoryBrowserEvent::Terminal(Event::Mouse(mouse)) => {
                self.update_mouse(mouse, Instant::now())
            }
            MemoryBrowserEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            MemoryBrowserEvent::Terminal(_) => ComponentUpdate::none(),
            MemoryBrowserEvent::Loaded { access, records } => {
                self.replace_records(access, records);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            MemoryBrowserEvent::LoadFailed {
                source,
                access,
                error,
            } => {
                self.source = source;
                self.access = access;
                self.overlay_scroll = 0;
                self.state = BrowserState::Error(BrowserError {
                    message: error,
                    action: ErrorAction::Load,
                });
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            MemoryBrowserEvent::Deleted { key } => {
                self.remove_record(&key);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            MemoryBrowserEvent::DeleteFailed { error, conflict } => {
                let BrowserState::Deleting { key, return_to } = self.state.clone() else {
                    return ComponentUpdate::none();
                };
                if conflict {
                    return self.refresh();
                }
                self.overlay_scroll = 0;
                self.state = BrowserState::Error(BrowserError {
                    message: error,
                    action: ErrorAction::Delete { key, return_to },
                });
                ComponentUpdate::render(RenderRequest::Immediate)
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect::default();
        let title = self.context_label();
        self.body = Floating::new(&title, 88, 28, self.footer())
            .render(frame, area, theme)
            .body;
        let body = self.body;
        if body.is_empty() {
            self.max_scroll = 0;
            return;
        }
        match self.state.clone() {
            BrowserState::Loading => frame.render_widget(
                Paragraph::new(format!(
                    "Loading {}…\n\nPress r to retry or Esc to close.",
                    self.context_label().to_lowercase()
                ))
                .style(Style::default().fg(theme.muted()))
                .wrap(Wrap { trim: false }),
                body,
            ),
            BrowserState::Error(error) => self.render_error(frame, body, theme, &error),
            BrowserState::List => self.render_list(frame, body, theme),
            BrowserState::Detail { key, scroll } => {
                self.render_detail(frame, body, theme, key, scroll)
            }
            BrowserState::ConfirmDelete { key, .. } => {
                self.render_confirmation(frame, body, theme, &key, false)
            }
            BrowserState::Deleting { key, .. } => {
                self.render_confirmation(frame, body, theme, &key, true)
            }
        }
    }
}

fn record_matches(record: &MemoryRecord, query: &str) -> bool {
    query.is_empty()
        || record.key.id.to_string().contains(query)
        || record
            .key
            .namespace
            .as_deref()
            .is_some_and(|namespace| namespace.to_lowercase().contains(query))
        || record.content.to_lowercase().contains(query)
}

fn identity_lines(key: &MemoryKey, theme: &Theme) -> Vec<Line<'static>> {
    [
        format!(
            "Namespace: {}",
            sanitize_terminal_text_inline(key.namespace.as_deref().unwrap_or("local"))
        ),
        format!("ID: {}", key.id),
        format!("Version: {}", key.version),
    ]
    .into_iter()
    .map(|line| Line::styled(line, Style::default().fg(theme.text())))
    .collect()
}

fn compare_newest(left: &MemoryRecord, right: &MemoryRecord) -> std::cmp::Ordering {
    right
        .updated_at_ms
        .cmp(&left.updated_at_ms)
        .then_with(|| left.key.namespace.cmp(&right.key.namespace))
        .then_with(|| right.key.id.cmp(&left.key.id))
}

fn compare_oldest(left: &MemoryRecord, right: &MemoryRecord) -> std::cmp::Ordering {
    left.updated_at_ms
        .cmp(&right.updated_at_ms)
        .then_with(|| left.key.namespace.cmp(&right.key.namespace))
        .then_with(|| left.key.id.cmp(&right.key.id))
}

fn list_metadata(record: &MemoryRecord) -> String {
    let identity = record.key.namespace.as_ref().map_or_else(
        || format!("local#{}", record.key.id),
        |namespace| format!("{namespace}#{}", record.key.id),
    );
    format!(
        "{} · v{} · updated {} · used {}× · {}",
        identity,
        record.key.version,
        timestamp_age(record.updated_at_ms),
        record.use_count,
        probation_status(record.probation_until_ms),
    )
}

fn detail_lines(record: &MemoryRecord, theme: &Theme) -> Vec<Line<'static>> {
    let label = Style::default().fg(theme.muted());
    let value = Style::default().fg(theme.text());
    let heading = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let mut metadata = vec![
        Line::styled(" Memory metadata", heading),
        fact(" ID", record.key.id.to_string(), label, value),
        fact(
            " Namespace",
            record
                .key
                .namespace
                .clone()
                .unwrap_or_else(|| "local".to_owned()),
            label,
            value,
        ),
        fact(" Version", record.key.version.to_string(), label, value),
        fact(
            " Created",
            format_timestamp(record.created_at_ms),
            label,
            value,
        ),
        fact(
            " Updated",
            format!(
                "{} ({})",
                format_timestamp(record.updated_at_ms),
                timestamp_age(record.updated_at_ms)
            ),
            label,
            value,
        ),
        fact(
            " Last scanned",
            optional_timestamp(record.last_scanned_at_ms),
            label,
            value,
        ),
        fact(" Scan count", record.scan_count.to_string(), label, value),
        fact(
            " Last used",
            optional_timestamp(record.last_used_at_ms),
            label,
            value,
        ),
        fact(" Use count", record.use_count.to_string(), label, value),
        fact(
            " Probation until",
            record
                .probation_until_ms
                .map_or_else(|| "none".to_owned(), format_timestamp),
            label,
            value,
        ),
    ];
    let mut lines = sanitize_detail(&record.content)
        .split('\n')
        .map(|line| Line::styled(line.to_owned(), value))
        .collect::<Vec<_>>();
    lines.push(Line::default());
    lines.append(&mut metadata);
    lines
}

fn fact(label_text: &'static str, value_text: String, label: Style, value: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label_text:<18}"), label),
        Span::styled(value_text, value),
    ])
}

fn optional_timestamp(timestamp_ms: Option<i64>) -> String {
    timestamp_ms.map_or_else(|| "never".to_owned(), format_timestamp)
}

fn format_timestamp(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms).map_or_else(
        || "unknown time".to_owned(),
        |time| time.format("%Y-%m-%d %H:%M:%SZ").to_string(),
    )
}

fn timestamp_age(timestamp_ms: i64) -> String {
    format_age(u64::try_from(timestamp_ms).unwrap_or_default())
}

fn probation_status(until_ms: Option<i64>) -> String {
    let Some(until_ms) = until_ms else {
        return "no probation".to_owned();
    };
    let remaining_ms = until_ms.saturating_sub(now_unix_ms());
    if remaining_ms <= 0 {
        return "probation elapsed".to_owned();
    }
    let minutes = u64::try_from(remaining_ms).unwrap_or_default() / 60_000;
    match minutes {
        0 => "probation <1m".to_owned(),
        1..=59 => format!("probation {minutes}m"),
        60..=1_439 => format!("probation {}h", minutes / 60),
        _ => format!("probation {}d", minutes / 1_440),
    }
}

fn now_unix_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

fn bounded_preview(content: &str, width: usize) -> String {
    let single_line = content
        .graphemes(true)
        .take(MAX_PREVIEW_GRAPHEMES)
        .flat_map(str::chars)
        .map(|character| {
            if character.is_control() || character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let collapsed = single_line.split_whitespace().collect::<Vec<_>>().join(" ");
    fit_width(&collapsed, width)
}

fn sanitize_detail(content: &str) -> String {
    let mut sanitized = String::with_capacity(content.len());
    for character in content.chars() {
        match character {
            '\n' => sanitized.push('\n'),
            '\t' => sanitized.push_str("    "),
            character if character.is_control() => sanitized.push('�'),
            character => sanitized.push(character),
        }
    }
    sanitized
}

fn fit_width(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }

    let content_width = width.saturating_sub(1);
    let mut result = String::new();
    let mut used: usize = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = grapheme.width();
        if used.saturating_add(grapheme_width) > content_width {
            break;
        }
        result.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    result.push('…');
    result
}

fn visible_tail(query: &str, width: usize) -> &str {
    let mut used: usize = 0;
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
        BrowserState, Component, MemoryBrowser, MemoryBrowserEffect, MemoryBrowserEvent,
        NamespaceScope, ReturnView, SortMode,
    };
    use crate::tui::theme::Theme;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use orvek_memory::{MemoryAccess, MemoryKey, MemoryRecord, MemorySource, RemoteRole};
    use ratatui::{Terminal, backend::TestBackend};

    fn local_access() -> MemoryAccess {
        MemoryAccess {
            source: MemorySource::Local,
            namespace: None,
            role: None,
        }
    }

    fn remote_access(namespace: &str, role: RemoteRole) -> MemoryAccess {
        MemoryAccess {
            source: MemorySource::Remote,
            namespace: Some(namespace.to_owned()),
            role: Some(role),
        }
    }

    fn record(id: i64, version: u64, content: &str) -> MemoryRecord {
        MemoryRecord {
            key: MemoryKey::local(id, version),
            content: content.to_owned(),
            created_at_ms: 0,
            updated_at_ms: 0,
            last_scanned_at_ms: None,
            scan_count: 2,
            last_used_at_ms: None,
            use_count: 3,
            probation_until_ms: None,
        }
    }

    fn remote_record(namespace: &str, id: i64, content: &str) -> MemoryRecord {
        let mut record = record(id, 1, content);
        record.key = MemoryKey::remote(namespace.to_owned(), id, 1);
        record
    }

    fn record_with_stats(id: i64, updated_at_ms: i64, use_count: u64) -> MemoryRecord {
        MemoryRecord {
            updated_at_ms,
            use_count,
            ..record(id, 1, &format!("memory {id}"))
        }
    }

    fn ordered_ids(browser: &MemoryBrowser) -> Vec<i64> {
        browser
            .matches
            .iter()
            .map(|index| browser.records[*index].key.id)
            .collect()
    }

    fn key(code: KeyCode) -> MemoryBrowserEvent {
        MemoryBrowserEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> MemoryBrowserEvent {
        MemoryBrowserEvent::Terminal(Event::Key(KeyEvent::new(code, modifiers)))
    }

    fn repeat_key(code: KeyCode) -> MemoryBrowserEvent {
        MemoryBrowserEvent::Terminal(Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )))
    }

    fn loaded(records: Vec<MemoryRecord>) -> MemoryBrowser {
        loaded_with_access(local_access(), records)
    }

    fn loaded_with_access(access: MemoryAccess, records: Vec<MemoryRecord>) -> MemoryBrowser {
        let mut browser = MemoryBrowser::new();
        browser.update(MemoryBrowserEvent::Loaded { access, records });
        browser
    }

    fn render(browser: &mut MemoryBrowser, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| browser.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn filtering_and_reloads_preserve_selection_by_id() {
        let mut browser = loaded(vec![record(1, 1, "cafe moon"), record(42, 2, "cafe sun")]);
        browser.update(key(KeyCode::Down));
        browser.update(MemoryBrowserEvent::Terminal(Event::Paste(
            "cafe".to_owned(),
        )));
        assert_eq!(browser.selected_key, Some(MemoryKey::local(1, 1)));

        browser.update(MemoryBrowserEvent::Loaded {
            access: local_access(),
            records: vec![record(42, 2, "cafe sun"), record(1, 1, "cafe moon")],
        });
        assert_eq!(browser.selected_key, Some(MemoryKey::local(1, 1)));

        browser.query.clear();
        browser.refresh_matches();
        browser.update(MemoryBrowserEvent::Deleted {
            key: MemoryKey::local(42, 2),
        });
        assert_eq!(browser.selected_key, Some(MemoryKey::local(1, 1)));
    }

    #[test]
    fn sort_modes_cycle_from_usefulness_through_age_and_back() {
        let mut browser = loaded(vec![
            record_with_stats(1, 100, 5),
            record_with_stats(2, 300, 1),
            record_with_stats(3, 200, 5),
            record_with_stats(4, 50, 0),
        ]);

        assert_eq!(browser.sort, SortMode::MostUseful);
        assert_eq!(ordered_ids(&browser), [3, 1, 2, 4]);
        assert!(render(&mut browser, 80, 16).contains("Sort: Most useful"));

        browser.update(modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(browser.sort, SortMode::Newest);
        assert_eq!(ordered_ids(&browser), [2, 3, 1, 4]);
        browser.update(MemoryBrowserEvent::Terminal(Event::Paste(
            "memory".to_owned(),
        )));
        browser.update(MemoryBrowserEvent::Loaded {
            access: local_access(),
            records: vec![
                record_with_stats(4, 50, 0),
                record_with_stats(3, 200, 5),
                record_with_stats(2, 300, 1),
                record_with_stats(1, 100, 5),
            ],
        });
        assert_eq!(browser.sort, SortMode::Newest);
        assert_eq!(ordered_ids(&browser), [2, 3, 1, 4]);

        browser.update(modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(browser.sort, SortMode::Oldest);
        assert_eq!(ordered_ids(&browser), [4, 1, 3, 2]);

        browser.update(modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(browser.sort, SortMode::LeastUseful);
        assert_eq!(ordered_ids(&browser), [4, 2, 1, 3]);

        browser.update(modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(browser.sort, SortMode::MostUseful);
        assert_eq!(ordered_ids(&browser), [3, 1, 2, 4]);
    }

    #[test]
    fn backspace_removes_a_whole_unicode_grapheme_and_ids_are_searchable() {
        let mut browser = loaded(vec![record(42, 1, "unrelated")]);
        browser.update(key(KeyCode::Char('e')));
        browser.update(key(KeyCode::Char('\u{301}')));
        browser.update(key(KeyCode::Backspace));
        assert!(browser.query.is_empty());

        browser.update(MemoryBrowserEvent::Terminal(Event::Paste("42".to_owned())));
        assert_eq!(browser.matches, [0]);
        assert_eq!(browser.selected_key, Some(MemoryKey::local(42, 1)));
    }

    #[test]
    fn lowercase_shortcut_letters_remain_available_to_the_filter() {
        let mut browser = loaded(vec![record(1, 1, "functional preference")]);

        for character in "functional".chars() {
            browser.update(key(KeyCode::Char(character)));
        }

        assert_eq!(browser.query, "functional");
        assert_eq!(browser.matches, [0]);
        assert_eq!(
            browser
                .update(modified_key(KeyCode::Char('r'), KeyModifiers::CONTROL))
                .effects,
            [MemoryBrowserEffect::Refresh]
        );
    }

    #[test]
    fn deletion_needs_two_physical_delete_keys_and_emits_once() {
        let mut browser = loaded(vec![record(7, 3, "forget me")]);

        assert!(browser.update(key(KeyCode::Delete)).effects.is_empty());
        assert!(matches!(
            &browser.state,
            BrowserState::ConfirmDelete {
                key,
                return_to: ReturnView::List
            } if *key == MemoryKey::local(7, 3)
        ));
        assert!(
            browser
                .update(repeat_key(KeyCode::Delete))
                .effects
                .is_empty()
        );
        assert_eq!(
            browser.update(key(KeyCode::Delete)).effects,
            [MemoryBrowserEffect::Delete(MemoryKey::local(7, 3))]
        );
        assert!(browser.update(key(KeyCode::Delete)).effects.is_empty());
    }

    #[test]
    fn shared_memories_show_the_author_namespace_and_cannot_be_deleted() {
        let mut browser = loaded_with_access(
            remote_access("bob", RemoteRole::Writer),
            vec![remote_record("alice", 7, "shared invariant")],
        );

        let list = render(&mut browser, 80, 16);
        assert!(list.contains("alice#7"));
        assert!(!list.contains("remove"));
        assert!(browser.update(key(KeyCode::Delete)).effects.is_empty());
        browser.update(key(KeyCode::Enter));
        let detail = render(&mut browser, 80, 20);
        assert!(detail.contains("alice"));
        assert!(browser.update(key(KeyCode::Char('d'))).effects.is_empty());
    }

    #[test]
    fn remote_namespace_scope_toggles_between_all_and_authenticated_namespace() {
        let mut browser = loaded_with_access(
            remote_access("alice", RemoteRole::Writer),
            vec![
                remote_record("alice", 1, "owned invariant"),
                remote_record("bob", 2, "shared convention"),
            ],
        );

        assert_eq!(browser.namespace_scope, NamespaceScope::All);
        assert_eq!(browser.matches.len(), 2);
        let all = render(&mut browser, 100, 16);
        assert!(all.contains("Namespaces: All namespaces"));
        assert!(all.contains("owned invariant"));
        assert!(all.contains("shared convention"));

        browser.update(modified_key(KeyCode::Char('n'), KeyModifiers::CONTROL));

        assert_eq!(browser.namespace_scope, NamespaceScope::Own);
        assert_eq!(browser.matches.len(), 1);
        assert_eq!(
            browser.records[browser.matches[0]].key.namespace.as_deref(),
            Some("alice")
        );
        let own = render(&mut browser, 100, 16);
        assert!(own.contains("Namespaces: alice"));
        assert!(own.contains("owned invariant"));
        assert!(!own.contains("shared convention"));

        browser.update(modified_key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(browser.namespace_scope, NamespaceScope::All);
        assert_eq!(browser.matches.len(), 2);
    }

    #[test]
    fn remote_writer_can_delete_only_authenticated_namespace_and_reader_is_read_only() {
        let own_key = MemoryKey::remote("alice".to_owned(), 7, 3);
        let mut own = record(7, 3, "owned");
        own.key = own_key.clone();
        let mut writer = loaded_with_access(remote_access("alice", RemoteRole::Writer), vec![own]);
        assert!(render(&mut writer, 80, 16).contains("Remote memory · alice"));
        assert!(render(&mut writer, 80, 16).contains("remove"));
        writer.update(key(KeyCode::Delete));
        assert_eq!(
            writer.update(key(KeyCode::Delete)).effects,
            [MemoryBrowserEffect::Delete(own_key)]
        );

        let mut reader = loaded_with_access(
            remote_access("alice", RemoteRole::Reader),
            vec![remote_record("alice", 7, "read only")],
        );
        assert!(!render(&mut reader, 80, 16).contains("remove"));
        assert!(reader.update(key(KeyCode::Delete)).effects.is_empty());
    }

    #[test]
    fn deletion_removes_only_the_exact_key() {
        let old = record(7, 1, "old");
        let current = record(7, 2, "current");
        let mut browser = loaded(vec![old, current]);

        browser.update(MemoryBrowserEvent::Deleted {
            key: MemoryKey::local(7, 1),
        });

        assert_eq!(browser.records.len(), 1);
        assert_eq!(browser.records[0].key, MemoryKey::local(7, 2));
    }

    #[test]
    fn optimistic_delete_conflicts_reload_instead_of_retrying_a_stale_key() {
        let mut browser = loaded(vec![record(7, 3, "changed elsewhere")]);
        browser.update(key(KeyCode::Delete));
        browser.update(key(KeyCode::Delete));

        let update = browser.update(MemoryBrowserEvent::DeleteFailed {
            error: "memory changed since it was read".to_owned(),
            conflict: true,
        });

        assert_eq!(update.effects, [MemoryBrowserEffect::Refresh]);
        assert!(matches!(browser.state, BrowserState::Loading));
    }

    #[test]
    fn escape_returns_from_detail_then_dismisses() {
        let mut browser = loaded(vec![record(1, 1, "inspect me")]);
        browser.update(key(KeyCode::Enter));
        browser.update(key(KeyCode::Down));
        assert!(browser.update(key(KeyCode::Esc)).effects.is_empty());
        assert!(matches!(&browser.state, BrowserState::List));
        assert_eq!(
            browser.update(key(KeyCode::Esc)).effects,
            [MemoryBrowserEffect::Dismiss]
        );
    }

    #[test]
    fn list_render_distinguishes_empty_and_no_matches() {
        let mut empty = loaded(Vec::new());
        assert!(render(&mut empty, 60, 12).contains("Local memory is empty"));

        let mut filtered = loaded(vec![record(1, 1, "alpha")]);
        filtered.update(MemoryBrowserEvent::Terminal(Event::Paste(
            "missing".to_owned(),
        )));
        assert!(render(&mut filtered, 60, 12).contains("No memories match"));
    }

    #[test]
    fn load_errors_keep_remote_backend_context() {
        let mut browser = MemoryBrowser::new();
        browser.update(MemoryBrowserEvent::LoadFailed {
            source: MemorySource::Remote,
            access: Some(remote_access("alice", RemoteRole::Reader)),
            error: "unavailable".to_owned(),
        });

        let rendered = render(&mut browser, 80, 16);
        assert!(rendered.contains("Remote memory · alice"));
        assert!(rendered.contains("Could not load memories."));
        assert!(rendered.contains("unavailable"));
    }

    #[test]
    fn load_errors_preserve_embedded_newlines() {
        let mut browser = MemoryBrowser::new();
        browser.update(MemoryBrowserEvent::LoadFailed {
            source: MemorySource::Local,
            access: None,
            error: "first\nsecond\u{1b}third\nlast".to_owned(),
        });

        let rendered = render(&mut browser, 80, 16);

        assert!(rendered.contains("first"));
        assert!(rendered.contains("second�third"));
        assert!(rendered.contains("last"));
        assert!(!rendered.contains("firstsecond"));
    }

    #[test]
    fn render_sanitizes_controls_and_is_safe_when_narrow() {
        let mut browser = loaded(vec![record(1, 1, "safe\u{1b}[31m\nnext")]);
        let rendered = render(&mut browser, 30, 8);
        assert!(rendered.contains("safe [31m next"));
        assert!(!rendered.contains('\u{1b}'));

        let rendered = render(&mut browser, 3, 2);
        assert!(!rendered.is_empty());
    }

    #[test]
    fn detail_renders_full_content_and_metadata_without_emitting_an_effect() {
        let mut browser = loaded(vec![record(9, 4, "first line\nsecond line")]);
        assert!(browser.update(key(KeyCode::Tab)).effects.is_empty());

        let rendered = render(&mut browser, 80, 28);
        assert!(rendered.contains("Memory metadata"));
        assert!(rendered.contains("first line"));
        assert!(rendered.contains("second line"));
    }
    #[test]
    fn regression_empty_list_advertises_the_actual_refresh_key() {
        let mut browser = loaded(vec![]);
        assert!(render(&mut browser, 88, 28).contains("Press Ctrl+R to refresh."));
    }
    #[test]
    fn confirmation_exposes_the_exact_version_and_enter_never_deletes() {
        let mut browser = loaded_with_access(
            remote_access("alice", RemoteRole::Writer),
            vec![remote_record("alice", 7, "first line\nsecond line")],
        );
        browser.update(key(KeyCode::Delete));
        let text = render(&mut browser, 40, 15);
        assert!(text.contains("Namespace: alice"));
        assert!(text.contains("ID: 7"));
        assert!(text.contains("Version: 1"));
        assert!(browser.update(key(KeyCode::Enter)).effects.is_empty());
        assert!(
            browser
                .update(repeat_key(KeyCode::Char('d')))
                .effects
                .is_empty()
        );
        let expected = browser.selected_key.clone().unwrap();
        assert_eq!(
            browser.update(key(KeyCode::Char('d'))).effects,
            [MemoryBrowserEffect::Delete(expected)]
        );
    }

    #[test]
    fn filter_has_its_own_row_and_content_precedes_all_metadata() {
        let mut browser = loaded_with_access(
            remote_access("long-authenticated-namespace", RemoteRole::Reader),
            vec![remote_record(
                "another-namespace",
                9,
                "abcdefghijklmno full content",
            )],
        );
        browser.update(MemoryBrowserEvent::Terminal(Event::Paste(
            "abcdefghijklmno".into(),
        )));
        let text = render(&mut browser, 40, 20);
        let rows = text
            .chars()
            .collect::<Vec<_>>()
            .chunks(40)
            .map(|row| row.iter().collect::<String>())
            .collect::<Vec<_>>();
        let filter = rows.iter().find(|line| line.contains("Filter:")).unwrap();
        assert!(filter.contains("abcdefghijklmno"));
        assert!(!filter.contains("Sort:"));
        browser.update(key(KeyCode::Enter));
        let text = render(&mut browser, 88, 28);
        assert!(
            text.find("abcdefghijklmno full content").unwrap()
                < text.find("Memory metadata").unwrap()
        );
        assert!(browser.update(key(KeyCode::Char('d'))).effects.is_empty());
    }

    #[test]
    fn full_error_text_is_scrollable_and_all_states_fit_tiny_areas() {
        let mut browser = MemoryBrowser::new();
        browser.update(MemoryBrowserEvent::LoadFailed {
            source: MemorySource::Local,
            access: None,
            error: format!("{} error-tail", "long ".repeat(200)),
        });
        render(&mut browser, 32, 12);
        browser.update(key(KeyCode::End));
        assert!(render(&mut browser, 32, 12).contains("error-tail"));
        for width in 0..20 {
            for height in 0..12 {
                render(&mut browser, width, height);
            }
        }
        let mut browser = loaded(vec![record(1, 1, "漢字\nfull body")]);
        for code in [KeyCode::Enter, KeyCode::Char('d')] {
            browser.update(key(code));
            for width in 0..20 {
                for height in 0..12 {
                    render(&mut browser, width, height);
                }
            }
        }
    }
    #[test]
    fn remote_access_role_and_namespace_are_sanitized_display_only() {
        let namespace = "alice\x1b[31m";
        let mut browser = loaded_with_access(
            remote_access(namespace, RemoteRole::Reader),
            vec![remote_record(namespace, 1, "content")],
        );
        let text = render(&mut browser, 88, 28);
        assert!(text.contains("Access: reader"));
        assert!(!text.contains('\x1b'));
        assert_eq!(
            browser.selected_key.as_ref().unwrap().namespace.as_deref(),
            Some(namespace)
        );
    }
}
