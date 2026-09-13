use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::value::{RawValue, to_raw_value};
use std::{
    io::Write,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::mpsc;
use web_time::Instant;

const PROTOCOL_VERSION: u32 = 1;
static PROCESS_MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();

/// Returns a process-relative monotonic timestamp for private cross-layer timing.
#[doc(hidden)]
#[must_use]
pub fn monotonic_now_ns() -> u64 {
    let elapsed = PROCESS_MONOTONIC_EPOCH
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos();
    u64::try_from(elapsed).unwrap_or(u64::MAX)
}

/// Failure while encoding, writing, or consuming the contractual event stream.
#[derive(Debug, thiserror::Error)]
pub enum EventError {
    /// A typed event could not be encoded as JSON.
    #[error("failed to encode agent event")]
    Encode(#[source] serde_json::Error),

    /// An encoded event could not be written to the supplied output.
    #[error("failed to write agent event")]
    Write(#[source] std::io::Error),

    /// The stream closed before the accepted turn emitted a terminal event.
    #[error("agent event stream closed before the turn emitted a terminal event")]
    ClosedBeforeTerminal,
}

/// One ordered event emitted by an agent run.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentEvent {
    /// Version of the stable event protocol.
    pub protocol_version: u32,
    /// Stable session/request identity shared by this event stream.
    pub request_id: Arc<str>,
    /// Monotonic sequence number within the stream.
    pub seq: u64,
    /// Stable event category.
    #[serde(rename = "type")]
    pub kind: AgentEventKind,
    /// Complete typed-event payload encoded as retained raw JSON.
    pub payload: Arc<RawValue>,
}

/// Private in-process timing carried beside an event without changing JSONL.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct AgentEventTiming {
    /// Process-relative nanoseconds when the event was emitted.
    pub emitted_ns: u64,
    /// Process-relative nanoseconds when the transport observed the source event.
    pub source_received_ns: Option<u64>,
}

/// An agent event plus private in-process delivery timing.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TimedAgentEvent {
    /// Contractual event visible to consumers.
    pub event: AgentEvent,
    /// Private in-process timing carried beside the contractual event.
    pub timing: AgentEventTiming,
}

/// Stable event categories emitted by the agent runtime.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentEventKind {
    /// Complete provider event in original order.
    #[serde(rename = "api.event")]
    ApiEvent,
    /// Incremental assistant text.
    #[serde(rename = "assistant.delta")]
    AssistantDelta,
    /// Completed assistant message.
    #[serde(rename = "assistant.message")]
    AssistantMessage,
    /// Incremental reasoning summary.
    #[serde(rename = "reasoning.summary.delta")]
    ReasoningSummaryDelta,
    /// Accepted turn started.
    #[serde(rename = "run.started")]
    RunStarted,
    /// Input was added to an active turn.
    #[serde(rename = "run.steered")]
    RunSteered,
    /// Recoverable run-level error was observed.
    #[serde(rename = "run.error")]
    RunError,
    /// Turn completed successfully.
    #[serde(rename = "run.completed")]
    RunCompleted,
    /// Turn terminated with an error.
    #[serde(rename = "run.failed")]
    RunFailed,
    /// Tool invocation started.
    #[serde(rename = "tool.call")]
    ToolCall,
    /// Tool invocation completed.
    #[serde(rename = "tool.result")]
    ToolResult,
    /// Optional model connection warmup started.
    #[serde(rename = "model.warmup.started")]
    ModelWarmupStarted,
    /// Optional model connection warmup completed.
    #[serde(rename = "model.warmup.completed")]
    ModelWarmupCompleted,
    /// Optional model connection warmup failed.
    #[serde(rename = "model.warmup.failed")]
    ModelWarmupFailed,
    /// Logical model call started.
    #[serde(rename = "model.call.started")]
    ModelCallStarted,
    /// Logical model call completed.
    #[serde(rename = "model.call.completed")]
    ModelCallCompleted,
    /// Logical model call failed.
    #[serde(rename = "model.call.failed")]
    ModelCallFailed,
    /// Model-side context compaction started.
    #[serde(rename = "model.compaction.started")]
    ModelCompactionStarted,
    /// Model-side context compaction completed.
    #[serde(rename = "model.compaction.completed")]
    ModelCompactionCompleted,
    /// Model-side context compaction failed.
    #[serde(rename = "model.compaction.failed")]
    ModelCompactionFailed,
    /// One transport attempt started.
    #[serde(rename = "model.attempt.started")]
    ModelAttemptStarted,
    /// One transport attempt failed.
    #[serde(rename = "model.attempt.failed")]
    ModelAttemptFailed,
    /// The SDK scheduled another transport attempt.
    #[serde(rename = "model.attempt.retrying")]
    ModelAttemptRetrying,
    /// A model transport connection started.
    #[serde(rename = "model.connection.started")]
    ModelConnectionStarted,
    /// A model transport connection completed.
    #[serde(rename = "model.connection.completed")]
    ModelConnectionCompleted,
    /// A model transport connection failed.
    #[serde(rename = "model.connection.failed")]
    ModelConnectionFailed,
}

/// The receiving half of an agent's typed event stream.
pub struct AgentEvents {
    request_id: Arc<str>,
    receiver: mpsc::UnboundedReceiver<TimedAgentEvent>,
}

impl AgentEvents {
    /// Stable session/request identifier shared by every event in this stream.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Receives the next event, or `None` after all emitters are dropped.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.recv_timed().await.map(|event| event.event)
    }

    /// Receives one event with private process-relative timing metadata.
    #[doc(hidden)]
    pub async fn recv_timed(&mut self) -> Option<TimedAgentEvent> {
        self.receiver.recv().await
    }

    /// Receives one immediately available event without waiting.
    #[doc(hidden)]
    pub fn try_recv_timed(&mut self) -> Option<TimedAgentEvent> {
        self.receiver.try_recv().ok()
    }

    /// Writes every event as one flushed JSONL record.
    ///
    /// # Errors
    ///
    /// Returns an error when an event cannot be encoded or written.
    pub async fn write_jsonl(mut self, mut output: impl Write) -> Result<(), EventError> {
        while let Some(event) = self.recv().await {
            write_jsonl_event(&mut output, &event)?;
        }
        Ok(())
    }

    /// Writes one turn through its terminal event and leaves the session stream
    /// available for follow-on turns.
    ///
    /// # Errors
    ///
    /// Returns an error when an event cannot be written or the agent stops
    /// before emitting `run.completed` or `run.failed`.
    pub async fn write_turn_jsonl(&mut self, mut output: impl Write) -> Result<(), EventError> {
        while let Some(event) = self.recv().await {
            let terminal = event.kind.is_terminal();
            write_jsonl_event(&mut output, &event)?;
            if terminal {
                return Ok(());
            }
        }
        Err(EventError::ClosedBeforeTerminal)
    }
}

impl Stream for AgentEvents {
    type Item = AgentEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.receiver
            .poll_recv(context)
            .map(|event| event.map(|event| event.event))
    }
}

impl AgentEventKind {
    /// Returns whether this event completes a turn.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::RunCompleted | Self::RunFailed)
    }
}

impl AgentEvent {
    /// Returns a stable typed projection of this event.
    ///
    /// Raw `OpenAI` frames and lower-level transport diagnostics remain
    /// lossless; application-facing run, assistant, reasoning, tool, model,
    /// and context events decode into named types.
    ///
    /// # Errors
    ///
    /// Returns an error when a payload does not satisfy the contract declared
    /// by its event kind.
    pub fn data(&self) -> Result<crate::AgentEventData, serde_json::Error> {
        use crate::{
            AgentEventData, AssistantEvent, ContextEvent, ModelEvent, ReasoningEvent, RunEvent,
            ToolEvent, TransportEvent,
        };

        Ok(match self.kind {
            AgentEventKind::ApiEvent => AgentEventData::OpenAi(self.decode_payload()?),
            AgentEventKind::AssistantDelta => {
                AgentEventData::Assistant(AssistantEvent::Delta(self.decode_payload()?))
            }
            AgentEventKind::AssistantMessage => {
                AgentEventData::Assistant(AssistantEvent::Message(self.decode_payload()?))
            }
            AgentEventKind::ReasoningSummaryDelta => {
                AgentEventData::Reasoning(ReasoningEvent::SummaryDelta(self.decode_payload()?))
            }
            AgentEventKind::RunStarted => {
                AgentEventData::Run(RunEvent::Started(self.decode_payload()?))
            }
            AgentEventKind::RunSteered => {
                AgentEventData::Run(RunEvent::Steered(self.decode_payload()?))
            }
            AgentEventKind::RunError => {
                AgentEventData::Run(RunEvent::Error(self.decode_payload()?))
            }
            AgentEventKind::RunCompleted => {
                AgentEventData::Run(RunEvent::Completed(Box::new(self.decode_payload()?)))
            }
            AgentEventKind::RunFailed => {
                AgentEventData::Run(RunEvent::Failed(Box::new(self.decode_payload()?)))
            }
            AgentEventKind::ToolCall => {
                AgentEventData::Tool(ToolEvent::Call(self.decode_payload()?))
            }
            AgentEventKind::ToolResult => {
                AgentEventData::Tool(ToolEvent::Result(self.decode_payload()?))
            }
            AgentEventKind::ModelWarmupStarted => {
                AgentEventData::Model(ModelEvent::WarmupStarted(self.decode_payload()?))
            }
            AgentEventKind::ModelWarmupCompleted => {
                AgentEventData::Model(ModelEvent::WarmupCompleted(self.decode_payload()?))
            }
            AgentEventKind::ModelWarmupFailed => {
                AgentEventData::Model(ModelEvent::WarmupFailed(self.decode_payload()?))
            }
            AgentEventKind::ModelCallStarted => {
                AgentEventData::Model(ModelEvent::CallStarted(self.decode_payload()?))
            }
            AgentEventKind::ModelCallCompleted => {
                AgentEventData::Model(ModelEvent::CallCompleted(self.decode_payload()?))
            }
            AgentEventKind::ModelCallFailed => {
                AgentEventData::Model(ModelEvent::CallFailed(self.decode_payload()?))
            }
            AgentEventKind::ModelCompactionStarted => {
                AgentEventData::Context(ContextEvent::CompactionStarted(self.decode_payload()?))
            }
            AgentEventKind::ModelCompactionCompleted => {
                AgentEventData::Context(ContextEvent::CompactionCompleted(self.decode_payload()?))
            }
            AgentEventKind::ModelCompactionFailed => {
                AgentEventData::Context(ContextEvent::CompactionFailed(self.decode_payload()?))
            }
            AgentEventKind::ModelAttemptStarted
            | AgentEventKind::ModelAttemptFailed
            | AgentEventKind::ModelAttemptRetrying
            | AgentEventKind::ModelConnectionStarted
            | AgentEventKind::ModelConnectionCompleted
            | AgentEventKind::ModelConnectionFailed => {
                AgentEventData::Transport(TransportEvent::new(self.kind, Arc::clone(&self.payload)))
            }
        })
    }

    /// Decodes the event payload into a caller-selected typed shape.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained payload does not match `T`.
    pub fn decode_payload<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.payload.get())
    }
}

fn write_jsonl_event(output: &mut impl Write, event: &AgentEvent) -> Result<(), EventError> {
    serde_json::to_writer(&mut *output, event).map_err(EventError::Encode)?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(EventError::Write)
}

/// Internal emission handle shared by orchestration and transport crates.
#[derive(Clone)]
pub struct EventSink {
    request_id: Arc<str>,
    next_seq: Arc<AtomicU64>,
    sender: mpsc::UnboundedSender<TimedAgentEvent>,
    mirror: Option<mpsc::UnboundedSender<TimedAgentEvent>>,
}

impl EventSink {
    /// Creates an emission handle and its independently consumed event stream.
    #[must_use]
    pub fn channel(request_id: String) -> (Self, AgentEvents) {
        let request_id = Arc::<str>::from(request_id);
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                request_id: Arc::clone(&request_id),
                next_seq: Arc::new(AtomicU64::new(1)),
                sender,
                mirror: None,
            },
            AgentEvents {
                request_id,
                receiver,
            },
        )
    }

    /// Returns the stable request/session identity attached to emitted events.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Creates a sink that mirrors its events into one independently owned stream.
    ///
    /// The returned sink preserves the parent sink's request identity and
    /// sequence counter. Dropping every clone of it closes only the mirror;
    /// the original session stream remains available.
    #[must_use]
    pub fn mirrored_channel(&self) -> (Self, AgentEvents) {
        let (mirror, receiver) = mpsc::unbounded_channel();
        (
            Self {
                request_id: Arc::clone(&self.request_id),
                next_seq: Arc::clone(&self.next_seq),
                sender: self.sender.clone(),
                mirror: Some(mirror),
            },
            AgentEvents {
                request_id: Arc::clone(&self.request_id),
                receiver,
            },
        )
    }

    /// Emits an event when a receiver is present and otherwise discards it.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be converted to JSON.
    pub fn emit<P: Serialize>(&self, kind: AgentEventKind, payload: P) -> Result<(), EventError> {
        self.emit_with_sequence(kind, payload).map(|_| ())
    }

    /// Emits an event and returns its session-monotonic sequence number.
    ///
    /// This is intended for transport telemetry that must correlate the point
    /// of emission with a downstream consumer without retaining payload data.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be converted to JSON.
    pub fn emit_with_sequence<P: Serialize>(
        &self,
        kind: AgentEventKind,
        payload: P,
    ) -> Result<u64, EventError> {
        self.emit_with_source_sequence(kind, payload, None)
    }

    /// Emits an event correlated with the process-monotonic source receipt time.
    pub fn emit_with_source_sequence<P: Serialize>(
        &self,
        kind: AgentEventKind,
        payload: P,
        source_received_ns: Option<u64>,
    ) -> Result<u64, EventError> {
        if self.sender.is_closed()
            && self
                .mirror
                .as_ref()
                .is_none_or(tokio::sync::mpsc::UnboundedSender::is_closed)
        {
            return Ok(self.next_seq.fetch_add(1, Ordering::Relaxed));
        }
        let payload = Arc::from(to_raw_value(&payload).map_err(EventError::Encode)?);
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let event = TimedAgentEvent {
            event: AgentEvent {
                protocol_version: PROTOCOL_VERSION,
                request_id: Arc::clone(&self.request_id),
                seq,
                kind,
                payload,
            },
            timing: AgentEventTiming {
                emitted_ns: monotonic_now_ns(),
                source_received_ns,
            },
        };
        drop(self.sender.send(event.clone()));
        if let Some(mirror) = &self.mirror {
            drop(mirror.send(event));
        }
        Ok(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentEventKind, EventSink};
    use crate::{AgentEventData, AssistantEvent, ToolEvent, TransportEvent};
    use serde::{Serialize, Serializer};
    use serde_json::json;

    #[test]
    fn events_are_ordered_and_receiver_drop_is_not_an_error() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        assert_eq!(receiver.request_id(), "request-1");
        events
            .emit(AgentEventKind::RunStarted, json!({ "n": 1 }))
            .unwrap();
        events
            .emit(AgentEventKind::RunCompleted, json!({ "n": 2 }))
            .unwrap();
        let first = receiver.receiver.try_recv().unwrap().event;
        let second = receiver.receiver.try_recv().unwrap().event;
        assert_eq!((first.seq, first.kind), (1, AgentEventKind::RunStarted));
        assert_eq!((second.seq, second.kind), (2, AgentEventKind::RunCompleted));
        assert_eq!(
            second.decode_payload::<serde_json::Value>().unwrap()["n"],
            2
        );
        drop(receiver);
        events.emit(AgentEventKind::RunFailed, json!({})).unwrap();
    }

    #[test]
    fn receiver_drop_skips_payload_serialization() {
        struct MustNotSerialize;

        impl Serialize for MustNotSerialize {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                panic!("closed event streams must not serialize payloads")
            }
        }

        let (events, receiver) = EventSink::channel("request-1".to_owned());
        drop(receiver);

        assert_eq!(
            events
                .emit_with_sequence(AgentEventKind::ApiEvent, MustNotSerialize)
                .unwrap(),
            1
        );
    }

    #[test]
    fn timing_is_private_and_preserves_the_jsonl_contract() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        let source_received_ns = super::monotonic_now_ns();
        events
            .emit_with_source_sequence(
                AgentEventKind::AssistantDelta,
                json!({ "text": "x" }),
                Some(source_received_ns),
            )
            .unwrap();

        let timed = receiver.receiver.try_recv().unwrap();
        assert_eq!(timed.timing.source_received_ns, Some(source_received_ns));
        assert!(timed.timing.emitted_ns >= source_received_ns);
        let encoded = serde_json::to_value(&timed.event).unwrap();
        assert!(encoded.get("timing").is_none());
        assert!(encoded.get("source_received_ns").is_none());
        assert_eq!(encoded["type"], "assistant.delta");
    }

    #[test]
    fn timed_events_can_be_drained_without_async_receive_round_trips() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        for n in 1..=3 {
            events
                .emit(AgentEventKind::AssistantDelta, json!({ "n": n }))
                .unwrap();
        }

        let sequences = std::iter::from_fn(|| receiver.try_recv_timed())
            .map(|event| event.event.seq)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2, 3]);
        assert!(receiver.try_recv_timed().is_none());
    }

    #[test]
    fn mirrored_stream_preserves_session_order_and_closes_independently() {
        let (events, mut session) = EventSink::channel("request-1".to_owned());
        let (turn_events, mut turn) = events.mirrored_channel();

        turn_events
            .emit(AgentEventKind::RunStarted, json!({ "turn": 1 }))
            .unwrap();
        turn_events
            .emit(AgentEventKind::RunCompleted, json!({ "turn": 1 }))
            .unwrap();

        let session_first = session.receiver.try_recv().unwrap().event;
        let session_second = session.receiver.try_recv().unwrap().event;
        let turn_first = turn.receiver.try_recv().unwrap().event;
        let turn_second = turn.receiver.try_recv().unwrap().event;
        assert_eq!(
            (session_first.seq, session_second.seq),
            (turn_first.seq, turn_second.seq)
        );
        assert_eq!(turn_second.kind, AgentEventKind::RunCompleted);

        drop(turn_events);
        assert!(turn.receiver.try_recv().is_err());
        events
            .emit(AgentEventKind::RunStarted, json!({ "turn": 2 }))
            .unwrap();
        assert_eq!(
            session.receiver.try_recv().unwrap().event.seq,
            session_second.seq + 1
        );
    }

    #[test]
    fn typed_projection_preserves_domain_values_and_raw_diagnostics() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        events
            .emit(
                AgentEventKind::AssistantDelta,
                json!({
                    "model_call_index": 2,
                    "item_id": "item-1",
                    "phase": "final_answer",
                    "text": "hello"
                }),
            )
            .unwrap();
        events
            .emit(
                AgentEventKind::ToolCall,
                json!({
                    "call_id": "call-1",
                    "tool": "deployment_region",
                    "arguments": {"service": "api"},
                    "model_call_index": 2
                }),
            )
            .unwrap();
        events
            .emit(
                AgentEventKind::ModelAttemptRetrying,
                json!({"attempt": 1, "next_attempt": 2}),
            )
            .unwrap();

        let assistant = receiver.receiver.try_recv().unwrap().event;
        let AgentEventData::Assistant(AssistantEvent::Delta(delta)) = assistant.data().unwrap()
        else {
            panic!("assistant delta should use the typed assistant projection");
        };
        assert_eq!(delta.text, "hello");
        assert_eq!(delta.model_call_index, 2);

        let tool = receiver.receiver.try_recv().unwrap().event;
        let AgentEventData::Tool(ToolEvent::Call(call)) = tool.data().unwrap() else {
            panic!("tool call should use the typed tool projection");
        };
        assert_eq!(call.tool, "deployment_region");
        assert_eq!(
            call.decode_arguments::<serde_json::Value>().unwrap()["service"],
            "api"
        );

        let diagnostic = receiver.receiver.try_recv().unwrap().event;
        let AgentEventData::Transport(transport) = diagnostic.data().unwrap() else {
            panic!("retry should remain a lossless transport diagnostic");
        };
        assert_eq!(
            TransportEvent::kind(&transport),
            AgentEventKind::ModelAttemptRetrying
        );
        assert_eq!(
            transport.decode_payload::<serde_json::Value>().unwrap()["next_attempt"],
            2
        );
    }
}
