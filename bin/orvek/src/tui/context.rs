//! Content-free context diagnostics projected from transcript telemetry.

use crate::sessions::{
    context::{ApiEvent, request_context_snapshot},
    record::TranscriptRecord,
};
use nanocodex::oai::{
    self,
    events::{CompactionCompleted, CompactionStarted, ModelCallCompleted},
    responses::Usage,
};
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContinuationMode {
    FullContext,
    PreviousResponse,
}

pub(crate) const MODEL_WINDOW_TOKENS: u64 = oai::CONTEXT_WINDOW_TOKENS;
pub(crate) const AUTO_COMPACT_TOKEN_LIMIT: u64 = 244_800;

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
    pub(crate) status: CompactionStatus,
    pub(crate) trigger: CompactionTrigger,
    pub(crate) started_at_unix_ms: u64,
    pub(crate) finished_at_unix_ms: Option<u64>,
    pub(crate) before_tokens: Option<u64>,
    pub(crate) after_tokens: Option<u64>,
    pub(crate) after_estimated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactionStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactionTrigger {
    Automatic,
    Manual,
}

/// A count-only projection that never retains request content or opaque identifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContextDiagnostics {
    pub(crate) model_window_tokens: u64,
    pub(crate) auto_compact_token_limit: u64,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) continuation: Option<ContinuationMode>,
    pub(crate) prompt_cache: Option<bool>,
    pub(crate) compactions_started: u64,
    pub(crate) compactions_completed: u64,
    pub(crate) last_compaction: Option<CompactionDiagnostics>,
    awaiting_post_compaction_usage: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContextObservation {
    pub(crate) completed_tokens: Option<u64>,
}

impl Default for ContextDiagnostics {
    fn default() -> Self {
        Self {
            model_window_tokens: MODEL_WINDOW_TOKENS,
            auto_compact_token_limit: AUTO_COMPACT_TOKEN_LIMIT,
            usage: None,
            continuation: None,
            prompt_cache: None,
            compactions_started: 0,
            compactions_completed: 0,
            last_compaction: None,
            awaiting_post_compaction_usage: false,
        }
    }
}

impl ContextDiagnostics {
    #[cfg(test)]
    fn rebuild<'a>(records: impl IntoIterator<Item = &'a TranscriptRecord>) -> Self {
        let mut diagnostics = Self::default();
        for record in records {
            diagnostics.observe(record);
        }
        diagnostics
    }

    /// Adopt the operator's configured input budget as the window.
    ///
    /// The default seeds from the vendored `CONTEXT_WINDOW_TOKENS`, which is
    /// only correct until a run reports its real policy. Seeding from config at
    /// startup keeps the indicator honest before the first `run.started`.
    /// The derived limit matches `observe` exactly so the two paths cannot drift.
    pub(crate) const fn set_window(&mut self, tokens: u64) {
        self.model_window_tokens = tokens;
        self.auto_compact_token_limit = tokens * 9 / 10;
    }

    pub(crate) fn observe(&mut self, record: &TranscriptRecord) -> ContextObservation {
        match (record.source(), record.kind()) {
            ("agent", "run.started") => {
                #[derive(Deserialize)]
                struct Policy {
                    input_budget_tokens: Option<u64>,
                }
                if let Ok(Policy {
                    input_budget_tokens: Some(tokens),
                }) = record.decode_payload::<Policy>()
                {
                    self.model_window_tokens = tokens;
                    self.auto_compact_token_limit = tokens * 9 / 10;
                }
                ContextObservation::default()
            }
            ("agent", "api.event") => self.observe_api_event(record),
            ("agent", "model.call.completed") => self.observe_model_call_completed(record),
            ("agent", "model.compaction.started") => {
                self.observe_compaction_started(record);
                ContextObservation::default()
            }
            ("agent", "model.compaction.completed") => {
                self.observe_compaction_completed(record);
                ContextObservation::default()
            }
            ("agent", "model.compaction.failed") | ("agent", "run.failed") => {
                if let Some(compaction) = &mut self.last_compaction
                    && compaction.status == CompactionStatus::Running
                {
                    compaction.status = CompactionStatus::Failed;
                    compaction.finished_at_unix_ms = Some(record.recorded_at_unix_ms());
                }
                self.awaiting_post_compaction_usage = false;
                ContextObservation::default()
            }
            ("tact", "context.observed") => {
                self.observe_context_snapshot(record);
                ContextObservation::default()
            }
            _ => ContextObservation::default(),
        }
    }

    fn observe_api_event(&mut self, record: &TranscriptRecord) -> ContextObservation {
        let Ok(payload) = record.decode_payload::<ApiEvent>() else {
            return ContextObservation::default();
        };
        if payload.phase != "generation" {
            return ContextObservation::default();
        }
        match payload.direction {
            "outbound" => {
                self.observe_request(payload.event);
                ContextObservation::default()
            }
            "inbound" => self.observe_response_event(payload.event),
            _ => ContextObservation::default(),
        }
    }

    fn observe_request(&mut self, request: &RawValue) {
        let Some((prompt_cache, previous_response)) = request_context_snapshot(request) else {
            return;
        };
        self.prompt_cache = Some(prompt_cache);
        self.continuation = Some(if previous_response {
            ContinuationMode::PreviousResponse
        } else {
            ContinuationMode::FullContext
        });
    }

    fn observe_response_event(&mut self, event: &RawValue) -> ContextObservation {
        let Ok(event) = serde_json::from_str::<ResponseEvent>(event.get()) else {
            return ContextObservation::default();
        };
        if event.kind != "response.completed" {
            return ContextObservation::default();
        }
        let usage = event
            .response
            .and_then(|response| response.usage)
            .map(usage_into_tokens);
        let completed_tokens = usage.map(|usage| usage.total);
        self.set_usage(usage);
        ContextObservation { completed_tokens }
    }

    fn observe_model_call_completed(&mut self, record: &TranscriptRecord) -> ContextObservation {
        let Ok(payload) = record.decode_payload::<ModelCallCompleted>() else {
            return ContextObservation::default();
        };
        let usage = payload.usage.map(usage_into_tokens);
        let completed_tokens = usage.map(|usage| usage.total);
        self.set_usage(usage);
        ContextObservation { completed_tokens }
    }

    fn observe_context_snapshot(&mut self, record: &TranscriptRecord) {
        let Ok(snapshot) = record.decode_payload::<ContextSnapshot>() else {
            return;
        };
        self.prompt_cache = Some(snapshot.prompt_cache);
        self.continuation = Some(if snapshot.previous_response {
            ContinuationMode::PreviousResponse
        } else {
            ContinuationMode::FullContext
        });
    }

    fn set_usage(&mut self, usage: Option<TokenUsage>) {
        let Some(usage) = usage else {
            return;
        };
        self.usage = Some(usage);
        if self.awaiting_post_compaction_usage {
            if let Some(compaction) = &mut self.last_compaction {
                compaction.after_tokens = Some(usage.input);
                compaction.after_estimated = false;
            }
            self.awaiting_post_compaction_usage = false;
        }
    }

    fn observe_compaction_started(&mut self, record: &TranscriptRecord) {
        let payload = record.decode_payload::<CompactionStarted>().ok();
        let before_tokens = payload
            .as_ref()
            .map(|payload| payload.active_context_tokens);
        if let Some(payload) = &payload {
            self.auto_compact_token_limit = payload.auto_compact_token_limit;
        }
        self.compactions_started = self.compactions_started.saturating_add(1);
        self.last_compaction = Some(CompactionDiagnostics {
            status: CompactionStatus::Running,
            trigger: if payload.is_some_and(|payload| payload.manual) {
                CompactionTrigger::Manual
            } else {
                CompactionTrigger::Automatic
            },
            started_at_unix_ms: record.recorded_at_unix_ms(),
            finished_at_unix_ms: None,
            before_tokens,
            after_tokens: None,
            after_estimated: false,
        });
        self.awaiting_post_compaction_usage = false;
    }

    fn observe_compaction_completed(&mut self, record: &TranscriptRecord) {
        self.compactions_completed = self.compactions_completed.saturating_add(1);
        if let Some(compaction) = &mut self.last_compaction {
            compaction.status = CompactionStatus::Completed;
            compaction.finished_at_unix_ms = Some(record.recorded_at_unix_ms());
            if let Ok(payload) = record.decode_payload::<CompactionCompleted>()
                && let Some(after) = payload.after_tokens
            {
                compaction.after_tokens = Some(after);
                compaction.after_estimated = true;
            }
            self.awaiting_post_compaction_usage = true;
        }
    }
}

#[derive(Deserialize)]
struct ResponseEvent<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    response: Option<Response>,
}

#[derive(Deserialize)]
struct Response {
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ContextSnapshot {
    prompt_cache: bool,
    previous_response: bool,
}

fn usage_into_tokens(usage: Usage) -> TokenUsage {
    let cached_input = usage
        .input_tokens_details
        .map_or(0, |details| details.cached_tokens);
    TokenUsage {
        input: usage.input_tokens,
        cached_input,
        uncached_input: usage.input_tokens.saturating_sub(cached_input),
        output: usage.output_tokens,
        total: usage.total_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextDiagnostics, ContinuationMode};
    use crate::sessions::record::TranscriptRecord;
    use nanocodex::agent::events::{AgentEvent, AgentEventKind};
    use serde_json::{Value, json, value::to_raw_value};
    use std::sync::Arc;

    #[test]
    fn cancelled_manual_compaction_is_finished_in_diagnostics() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.observe(&agent(
            1,
            10,
            AgentEventKind::ModelCompactionStarted,
            json!({
                "after_model_call_index": 0, "active_context_tokens": 100000,
                "auto_compact_token_limit": 244800, "manual": true
            }),
        ));
        diagnostics.observe(&agent(
            2,
            30,
            AgentEventKind::ModelCompactionFailed,
            json!({
                "after_model_call_index": 0, "duration_ns": 20000000,
                "error": "Context compaction was cancelled"
            }),
        ));
        let last = diagnostics.last_compaction.unwrap();
        assert_eq!(last.trigger, super::CompactionTrigger::Manual);
        assert_eq!(last.status, super::CompactionStatus::Failed);
        assert_eq!(last.finished_at_unix_ms, Some(30));
        assert_eq!(diagnostics.compactions_completed, 0);
    }

    fn agent(sequence: u64, at: u64, kind: AgentEventKind, payload: Value) -> TranscriptRecord {
        TranscriptRecord::from_agent(
            sequence,
            at,
            AgentEvent {
                protocol_version: 1,
                request_id: Arc::from("secret-request-id"),
                seq: sequence,
                kind,
                payload: to_raw_value(&payload).unwrap().into(),
            },
        )
    }

    fn api(direction: &str, event: Value) -> Value {
        json!({"direction": direction, "phase": "generation", "event": event})
    }

    fn model_call_completed(usage: Value) -> Value {
        json!({
            "call_index": 1,
            "model": "gpt-5.6-sol",
            "response_id": "secret-continuation-token",
            "attempt": 1,
            "connection_generation": 1,
            "status": "completed",
            "duration_ns": 1,
            "time_to_first_event_ns": 1,
            "time_to_first_output_ns": 1,
            "tool_calls": 0,
            "usage": usage
        })
    }

    #[test]
    fn complete_telemetry_projects_only_safe_facts_and_counts() {
        let records = [
            agent(
                1,
                1,
                AgentEventKind::ApiEvent,
                api(
                    "outbound",
                    json!({
                        "prompt_cache_key": "secret-cache-key",
                        "previous_response_id": "secret-response-id",
                        "input": [{"role":"user", "content":"secret prompt"}]
                    }),
                ),
            ),
            agent(
                2,
                2,
                AgentEventKind::ModelCallCompleted,
                model_call_completed(json!({
                        "input_tokens": 1_000,
                        "input_tokens_details": {"cached_tokens": 750},
                        "output_tokens": 80,
                        "total_tokens": 1_080
                })),
            ),
            agent(
                3,
                100,
                AgentEventKind::ModelCompactionStarted,
                json!({
                    "after_model_call_index": 1,
                    "active_context_tokens": 900,
                    "auto_compact_token_limit": 200_000,
                    "previous_response_id": "secret-response-id"
                }),
            ),
            agent(
                4,
                110,
                AgentEventKind::ModelCompactionCompleted,
                json!({
                    "response_id": "secret-compaction-id"
                }),
            ),
            agent(
                5,
                120,
                AgentEventKind::ModelCallCompleted,
                model_call_completed(
                    json!({"input_tokens": 400, "output_tokens": 20, "total_tokens": 420}),
                ),
            ),
        ];
        let diagnostics = ContextDiagnostics::rebuild(records.iter());

        assert_eq!(
            diagnostics.continuation,
            Some(ContinuationMode::PreviousResponse)
        );
        assert_eq!(diagnostics.prompt_cache, Some(true));
        assert_eq!(diagnostics.usage.unwrap().total, 420);
        assert_eq!(diagnostics.compactions_started, 1);
        assert_eq!(diagnostics.compactions_completed, 1);
        assert_eq!(
            diagnostics.model_window_tokens,
            nanocodex::oai::CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(diagnostics.auto_compact_token_limit, 200_000);
        assert_eq!(
            diagnostics.last_compaction.unwrap().before_tokens,
            Some(900)
        );
        assert_eq!(diagnostics.last_compaction.unwrap().after_tokens, Some(400));
        let debug = format!("{diagnostics:?}");
        for secret in [
            "secret-cache-key",
            "secret prompt",
            "secret-response-id",
            "secret-continuation-token",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn partial_and_unavailable_telemetry_remain_explicit() {
        let mut diagnostics = ContextDiagnostics::default();
        diagnostics.observe(&agent(
            1,
            10,
            AgentEventKind::ModelCompactionStarted,
            json!({}),
        ));
        diagnostics.observe(&agent(
            2,
            20,
            AgentEventKind::ModelCompactionCompleted,
            json!({}),
        ));
        diagnostics.observe(&agent(
            3,
            30,
            AgentEventKind::ApiEvent,
            api("outbound", json!({})),
        ));

        assert!(diagnostics.usage.is_none());
        assert_eq!(
            diagnostics.continuation,
            Some(ContinuationMode::FullContext)
        );
        assert_eq!(diagnostics.prompt_cache, Some(false));
        assert_eq!(diagnostics.last_compaction.unwrap().before_tokens, None);
        assert_eq!(diagnostics.last_compaction.unwrap().after_tokens, None);
    }

    #[test]
    fn completed_response_total_remains_available_to_the_composer() {
        let record = agent(
            1,
            1,
            AgentEventKind::ApiEvent,
            api(
                "inbound",
                json!({
                    "type": "response.completed",
                    "response": {"usage": {"total_tokens": 136_000}}
                }),
            ),
        );
        let mut diagnostics = ContextDiagnostics::default();
        let observation = diagnostics.observe(&record);

        assert_eq!(observation.completed_tokens, Some(136_000));
        assert_eq!(diagnostics.usage.unwrap().total, 136_000);
    }
}
