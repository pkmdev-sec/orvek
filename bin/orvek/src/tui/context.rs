//! Read-only diagnostics; missing provider measurements remain unknown.

use crate::tui::{host_projection::ViewChange, transcript::TranscriptRecord};
use orvek_harness::state::TaskEvent;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TokenUsage {
    pub(crate) input: u64,
    pub(crate) cached_input: u64,
    pub(crate) uncached_input: u64,
    pub(crate) output: u64,
    pub(crate) total: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompactionDiagnostics {
    pub(crate) trigger: CompactionTrigger,
    pub(crate) started_at_unix_ms: u64,
    pub(crate) completed_at_unix_ms: Option<u64>,
    pub(crate) before_tokens: Option<u64>,
    pub(crate) after_tokens: Option<u64>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactionTrigger {
    Automatic,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContextDiagnostics {
    pub(crate) model_window_tokens: Option<u64>,
    pub(crate) auto_compact_token_limit: Option<u64>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) prompt_cache: Option<bool>,
    pub(crate) compactions_started: u64,
    pub(crate) compactions_completed: u64,
    pub(crate) last_compaction: Option<CompactionDiagnostics>,
    pub(crate) billed_tokens: Option<u64>,
    pub(crate) billing_uncertain: bool,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContextObservation {
    pub(crate) completed_tokens: Option<u64>,
}
impl ContextDiagnostics {
    pub(crate) fn observe(&mut self, record: &TranscriptRecord) -> ContextObservation {
        match record.host() {
            Some(ViewChange::ProviderUsage { usage }) => {
                self.usage = usage
                    .input_tokens
                    .zip(usage.output_tokens)
                    .zip(usage.total_tokens)
                    .zip(usage.cached_input_tokens)
                    .filter(|(((input, output), total), cached)| {
                        input.checked_add(*output) == Some(*total) && cached <= input
                    })
                    .map(|(((input, output), total), cached)| TokenUsage {
                        input,
                        cached_input: cached,
                        uncached_input: input - cached,
                        output,
                        total,
                    });
                return ContextObservation {
                    completed_tokens: self.usage.map(|usage| usage.input),
                };
            }
            Some(ViewChange::Task {
                event: TaskEvent::ModelCallRecorded { receipt, .. },
                ..
            }) => match receipt.tokens {
                Some(tokens) => {
                    self.billed_tokens =
                        Some(self.billed_tokens.unwrap_or(0).saturating_add(tokens))
                }
                None => self.billing_uncertain = true,
            },
            Some(ViewChange::ContextProjected { .. }) => {
                // The host reports a projection only once it has already happened, so
                // "started" and "completed" advance together; before/after tokens come
                // from the last observed usage, since the host does not report post-
                // projection token counts.
                self.compactions_started = self.compactions_started.saturating_add(1);
                self.compactions_completed = self.compactions_completed.saturating_add(1);
                self.last_compaction = Some(CompactionDiagnostics {
                    trigger: CompactionTrigger::Automatic,
                    started_at_unix_ms: record.recorded_at_unix_ms(),
                    completed_at_unix_ms: Some(record.recorded_at_unix_ms()),
                    before_tokens: self.usage.map(|usage| usage.input),
                    after_tokens: None,
                });
                self.usage = None;
            }
            _ => {}
        }
        // Billing totals are not a measurement of the active context window.
        ContextObservation::default()
    }
}
