//! Read-only diagnostics; missing provider measurements remain unknown.

use crate::tui::{host_projection::ViewChange, transcript::TranscriptRecord};
use orvek_harness::{
    context_cost::{RepresentationKind, RepresentationObservation, RepresentationProfile},
    inference::UsdCost,
    state::TaskEvent,
};
use std::collections::{BTreeMap, VecDeque};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TokenUsage {
    pub(crate) input: u64,
    pub(crate) cached_input: u64,
    pub(crate) uncached_input: u64,
    pub(crate) output: u64,
    pub(crate) reasoning: Option<u64>,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RepresentationDiagnostics {
    pub(crate) source_bytes: u64,
    pub(crate) native_segments: u64,
    pub(crate) bitmap_segments: u64,
    pub(crate) bitmap_pages: u64,
    pub(crate) observed_pair_savings: Option<UsdCost>,
    pub(crate) estimated_next_call_savings: Option<UsdCost>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContextDiagnostics {
    pub(crate) model_window_tokens: Option<u64>,
    pub(crate) request_token_limit: Option<u64>,
    pub(crate) usage: Option<TokenUsage>,
    usage_history: VecDeque<u64>,
    pub(crate) prompt_cache: Option<bool>,
    pub(crate) representation: Option<RepresentationDiagnostics>,
    representation_profile: RepresentationProfile,
    pub(crate) compactions_started: u64,
    pub(crate) compactions_completed: u64,
    pub(crate) last_compaction: Option<CompactionDiagnostics>,
    pub(crate) billed_tokens: Option<u64>,
    pub(crate) billing_uncertain: bool,
    cost_receipts: BTreeMap<Uuid, Option<UsdCost>>,
    session_cost: UsdCost,
    cost_uncertain: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SessionCost {
    pub(crate) total: UsdCost,
    pub(crate) uncertain: bool,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContextObservation {
    pub(crate) completed_tokens: Option<u64>,
}
impl ContextDiagnostics {
    pub(crate) fn usage_history(&self) -> impl ExactSizeIterator<Item = u64> + '_ {
        self.usage_history.iter().copied()
    }

    fn record_usage(&mut self, tokens: u64) {
        const HISTORY_LIMIT: usize = 32;
        if self.usage_history.len() == HISTORY_LIMIT {
            self.usage_history.pop_front();
        }
        self.usage_history.push_back(tokens);
    }

    fn observe_representation(&mut self, observation: &RepresentationObservation) {
        let native_segments = observation
            .selected
            .values()
            .filter(|kind| **kind == RepresentationKind::Native)
            .count() as u64;
        let bitmap_segments = observation.selected.len() as u64 - native_segments;
        self.representation_profile.observe(observation.clone());
        let savings = observation
            .selected
            .iter()
            .filter(|(_, kind)| **kind == RepresentationKind::Bitmap)
            .try_fold(UsdCost::ZERO, |total, (segment, _)| {
                self.representation_profile
                    .estimate(observation.model, *segment)
                    .filter(|estimate| estimate.selected == RepresentationKind::Bitmap)
                    .and_then(|estimate| total.checked_add(estimate.estimated_savings))
            });
        self.representation = Some(RepresentationDiagnostics {
            source_bytes: observation.source_bytes,
            native_segments,
            bitmap_segments,
            bitmap_pages: observation.bitmap_pages,
            observed_pair_savings: savings,
            estimated_next_call_savings: savings,
        });
    }

    pub(crate) fn session_cost(&self) -> SessionCost {
        SessionCost {
            total: self.session_cost,
            uncertain: self.cost_uncertain,
        }
    }

    pub(crate) fn restore_session_cost(&mut self, cost: SessionCost) {
        self.cost_receipts.clear();
        self.session_cost = cost.total;
        self.cost_uncertain = cost.uncertain;
    }

    pub(crate) fn observe(&mut self, record: &TranscriptRecord) -> ContextObservation {
        let mut observation = ContextObservation::default();
        for change in record.host_changes() {
            match change {
                ViewChange::ProviderCost { call, cost_usd, .. } => {
                    if let Some(previous) = self.cost_receipts.get(call) {
                        if *previous != *cost_usd {
                            self.cost_uncertain = true;
                        }
                    } else {
                        self.cost_receipts.insert(*call, *cost_usd);
                        match cost_usd {
                            Some(cost) => match self.session_cost.checked_add(*cost) {
                                Some(total) => self.session_cost = total,
                                None => self.cost_uncertain = true,
                            },
                            None => self.cost_uncertain = true,
                        }
                    }
                }
                ViewChange::ProviderUsage {
                    call,
                    usage,
                    representation,
                    ..
                } => {
                    if let Some(representation) = representation {
                        self.observe_representation(representation);
                    }
                    if call.is_none_or(|call| !self.cost_receipts.contains_key(&call)) {
                        self.cost_uncertain = true;
                    }
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
                            reasoning: usage.reasoning_tokens,
                            total,
                        });
                    if let Some(usage) = self.usage {
                        self.record_usage(usage.input);
                    }
                    observation.completed_tokens = self.usage.map(|usage| usage.input);
                }
                ViewChange::Task {
                    event: TaskEvent::ModelCallRecorded { receipt, .. },
                    ..
                } => match receipt.tokens {
                    Some(tokens) => {
                        self.billed_tokens =
                            Some(self.billed_tokens.unwrap_or(0).saturating_add(tokens));
                    }
                    None => self.billing_uncertain = true,
                },
                ViewChange::ContextProjected { .. } => {
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
        }
        // Billing totals are not a measurement of the active context window.
        observation
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextDiagnostics, SessionCost};
    use crate::tui::{host_projection::ViewChange, transcript::TranscriptRecord};
    use orvek_harness::{
        Digest,
        context_cost::{RepresentationKind, RepresentationObservation},
        inference::{Model, Usage, UsdCost},
        session::{SessionCursor, SessionId},
    };
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn cost_record(sequence: u64, call: Uuid, cost: Option<&str>) -> TranscriptRecord {
        TranscriptRecord::from_host(
            sequence,
            sequence,
            SessionCursor {
                version: 1,
                session: SessionId::new(),
                revision: sequence,
            },
            ViewChange::ProviderCost {
                request: Uuid::nil(),
                call,
                cost_usd: cost.map(|cost| cost.parse::<UsdCost>().unwrap()),
            },
        )
    }

    fn usage_record(sequence: u64, call: Option<Uuid>) -> TranscriptRecord {
        TranscriptRecord::from_host(
            sequence,
            sequence,
            SessionCursor {
                version: 1,
                session: SessionId::new(),
                revision: sequence,
            },
            ViewChange::ProviderUsage {
                request: Uuid::nil(),
                call,
                representation: None,
                usage: Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    total_tokens: Some(12),
                    cached_input_tokens: Some(4),
                    reasoning_tokens: Some(1),
                    cost_usd: None,
                },
            },
        )
    }

    #[test]
    fn provider_usage_history_is_bounded_for_sparklines() {
        let mut diagnostics = ContextDiagnostics::default();
        for sequence in 1..=40 {
            diagnostics.observe(&usage_record(sequence, None));
        }

        assert_eq!(diagnostics.usage_history().len(), 32);
    }

    #[test]
    fn restored_usage_without_a_linked_cost_receipt_is_uncertain() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.observe(&usage_record(1, None));

        assert!(diagnostics.session_cost().uncertain);
    }

    #[test]
    fn session_cost_sums_each_request_once_and_preserves_full_precision() {
        let mut diagnostics = ContextDiagnostics::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        diagnostics.observe(&cost_record(1, first, Some("0.1")));
        diagnostics.observe(&cost_record(2, second, Some("0.000000250000000001")));
        diagnostics.observe(&cost_record(3, first, Some("0.1")));

        let cost = diagnostics.session_cost();
        assert_eq!(cost.total.to_string(), "$0.100000250000000001");
        assert!(!cost.uncertain);
    }

    #[test]
    fn restored_session_cost_continues_accumulating_live_receipts() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.restore_session_cost(SessionCost {
            total: "0.125".parse().unwrap(),
            uncertain: false,
        });
        diagnostics.observe(&cost_record(1, Uuid::new_v4(), Some("0.25")));

        assert_eq!(diagnostics.session_cost().total.to_string(), "$0.375");
        assert!(!diagnostics.session_cost().uncertain);
    }

    #[test]
    fn missing_or_conflicting_receipts_make_the_total_explicitly_uncertain() {
        let mut diagnostics = ContextDiagnostics::default();
        let request = Uuid::new_v4();
        diagnostics.observe(&cost_record(1, request, Some("0.1")));
        diagnostics.observe(&cost_record(2, request, Some("0.2")));
        diagnostics.observe(&cost_record(3, Uuid::new_v4(), None));

        let cost = diagnostics.session_cost();
        assert_eq!(cost.total.to_string(), "$0.1");
        assert!(cost.uncertain);
    }
    fn representation(kind: RepresentationKind, cost: &str) -> RepresentationObservation {
        RepresentationObservation {
            version: 1,
            model: Model::Sol,
            view_revision: 7,
            source_history: Digest::of(b"history"),
            controls: Digest::of(b"controls"),
            selected: BTreeMap::from([(Digest::of(b"segment"), kind)]),
            paired_input_tokens: BTreeMap::new(),
            source_bytes: 8192,
            bitmap_pages: u64::from(kind == RepresentationKind::Bitmap),
            input_tokens: Some(if kind == RepresentationKind::Native {
                100
            } else {
                60
            }),
            cached_input_tokens: Some(0),
            output_tokens: Some(10),
            reasoning_tokens: Some(2),
            cost: Some(cost.parse().unwrap()),
        }
    }

    #[test]
    fn diagnostics_report_zero_when_representation_did_not_change() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.observe_representation(&representation(RepresentationKind::Native, "2"));

        let observed = diagnostics.representation.unwrap();
        assert_eq!(observed.bitmap_segments, 0);
        assert_eq!(observed.observed_pair_savings, Some(UsdCost::ZERO));
        assert_eq!(observed.estimated_next_call_savings, Some(UsdCost::ZERO));
    }

    #[test]
    fn diagnostics_separate_paired_representation_savings_from_billed_total() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.observe_representation(&representation(RepresentationKind::Native, "2"));
        diagnostics.observe_representation(&representation(RepresentationKind::Bitmap, "1"));

        let observed = diagnostics.representation.unwrap();
        assert_eq!(observed.source_bytes, 8192);
        assert_eq!(observed.bitmap_pages, 1);
        assert_eq!(observed.observed_pair_savings.unwrap().to_string(), "$1");
        assert_eq!(diagnostics.session_cost().total, UsdCost::ZERO);
    }
}
