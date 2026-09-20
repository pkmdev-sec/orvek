//! Session-local prompt history and restoration of the draft being edited.

use crate::tui::session::MAX_RECENT_PROMPTS;

#[derive(Default)]
pub(super) struct PromptHistory {
    entries: Vec<String>,
    browsing: Option<Browsing>,
}

struct Browsing {
    index: usize,
    saved_draft: String,
}

impl PromptHistory {
    pub(super) fn record(&mut self, prompt: String) {
        if self.entries.len() == MAX_RECENT_PROMPTS {
            self.entries.remove(0);
        }
        self.entries.push(prompt);
        self.browsing = None;
    }

    pub(super) fn previous(&mut self, draft: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }

        let browsing = self.browsing.get_or_insert_with(|| Browsing {
            index: self.entries.len(),
            saved_draft: draft.to_owned(),
        });
        if browsing.index == 0 {
            return None;
        }

        browsing.index -= 1;
        Some(self.entries[browsing.index].clone())
    }

    pub(super) fn next(&mut self) -> Option<String> {
        let browsing = self.browsing.as_mut()?;
        browsing.index += 1;
        if browsing.index < self.entries.len() {
            return Some(self.entries[browsing.index].clone());
        }

        Some(self.browsing.take()?.saved_draft)
    }

    pub(super) fn is_browsing(&self) -> bool {
        self.browsing.is_some()
    }

    pub(super) fn detach(&mut self) {
        self.browsing = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_RECENT_PROMPTS, PromptHistory};

    #[test]
    fn prompt_history_keeps_only_the_latest_entries() {
        let mut history = PromptHistory::default();
        for index in 0..=MAX_RECENT_PROMPTS {
            history.record(format!("prompt {index}"));
        }

        assert_eq!(history.entries.len(), MAX_RECENT_PROMPTS);
        for expected in (1..=MAX_RECENT_PROMPTS).rev() {
            assert_eq!(
                history.previous("draft"),
                Some(format!("prompt {expected}"))
            );
        }
        assert!(history.previous("draft").is_none());
    }
}
