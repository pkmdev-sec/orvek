use super::*;
use nanocodex_oai_api::PromptValidationError;

/// Completion handle for an accepted turn.
///
/// A turn is both a [`Future`] for its final typed result and a [`Stream`] of
/// optional per-turn events. Result readiness is independent from consuming or
/// closing that event stream.
///
/// Dropping this handle does not cancel the accepted turn. Use [`Self::cancel`]
/// before dropping it when the work should stop.
#[must_use = "a turn continues running when dropped; await result(), control it, or explicitly drop it"]
pub struct Turn {
    pub(super) control: TurnControl,
    pub(super) events: AgentEvents,
    pub(super) result: oneshot::Receiver<Result<TurnResult>>,
}

/// Outcome of routing live user input into an agent session.
///
/// Live input adapters normally want to steer the current regular turn when
/// one exists and start a new turn only when the agent is idle.
/// [`Nanocodex::route_prompt`](crate::Nanocodex::route_prompt) performs that
/// decision atomically in the agent driver and returns this outcome.
pub enum PromptRoute {
    /// The agent was idle, so the prompt started a new independently awaitable turn.
    Started(Turn),
    /// The prompt was admitted to the current turn's steering queue.
    Steered,
}

impl Turn {
    /// Returns a cheap cloneable capability targeting this exact turn.
    #[must_use]
    pub fn control(&self) -> TurnControl {
        self.control.clone()
    }

    /// Injects additional input into this turn at its next safe model boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, when this turn is queued or no
    /// longer active, when its steering queue is full, or if the driver stops.
    pub async fn steer(&self, prompt: impl Into<Prompt>) -> Result<()> {
        self.control.steer(prompt).await
    }

    /// Cancels this exact unfinished turn.
    ///
    /// A queued turn is removed before execution and acknowledged immediately;
    /// its result and terminal event retain their FIFO position behind earlier
    /// turns. An active turn waits for its model and tool resources to stop
    /// before cancellation is acknowledged.
    ///
    /// # Errors
    ///
    /// Returns an error when this turn has already finished or if the driver
    /// stops.
    pub async fn cancel(&self) -> Result<()> {
        self.control.cancel().await
    }

    /// Waits for and returns the final typed turn result.
    ///
    /// This is equivalent to awaiting the turn directly. It does not wait for
    /// the per-turn event stream to be consumed or closed. Applications that
    /// need every event should consume the independently returned
    /// [`AgentEvents`] stream.
    ///
    /// # Errors
    ///
    /// Returns the model-run failure or an error if the driver stopped early.
    pub async fn result(self) -> Result<TurnResult> {
        self.await
    }
}

impl Stream for Turn {
    type Item = AgentEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.events).poll_next(context)
    }
}

impl Future for Turn {
    type Output = Result<TurnResult>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.result)
            .poll(context)
            .map(|result| result.map_err(|_| NanocodexError::TurnStopped)?)
    }
}

/// Cheap cloneable control capability for one accepted turn.
#[derive(Clone)]
pub struct TurnControl {
    pub(super) key: TurnKey,
    pub(super) commands: mpsc::Sender<Command>,
}

impl TurnControl {
    /// Injects additional input into the targeted turn.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, when the turn is not active, when
    /// its steering queue is full, or if the driver stops.
    pub async fn steer(&self, prompt: impl Into<Prompt>) -> Result<()> {
        let prompt = prompt.into();
        prompt.validate().map_err(steer_validation_error)?;
        request_command(&self.commands, |result| Command::Steer {
            key: self.key,
            prompt,
            result,
        })
        .await
    }

    /// Cancels the targeted unfinished turn.
    ///
    /// # Errors
    ///
    /// Returns an error when the turn has already finished or if the driver
    /// stops.
    pub async fn cancel(&self) -> Result<()> {
        request_command(&self.commands, |result| Command::Cancel {
            key: self.key,
            result,
        })
        .await
    }
}

fn steer_validation_error(error: PromptValidationError) -> NanocodexError {
    let message = match error {
        PromptValidationError::EmptyInstruction => "steer instruction must not be empty".to_owned(),
        error => error.to_string(),
    };
    NanocodexError::InvalidRequest(message)
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct TurnKey(pub(super) u64);

/// Final result of a completed turn.
#[derive(Clone)]
#[non_exhaustive]
pub struct TurnResult {
    pub(super) final_message: String,
    pub(super) usage: TurnUsage,
    pub(super) checkpoint: Arc<CommittedSession>,
}

impl TurnResult {
    /// Returns the final assistant message for this completed turn.
    #[must_use]
    pub fn final_message(&self) -> &str {
        &self.final_message
    }

    /// Consumes the result and returns its final assistant message.
    #[must_use]
    pub fn into_final_message(self) -> String {
        self.final_message
    }

    /// Returns exact aggregate token usage for this logical agent turn.
    #[must_use]
    pub const fn usage(&self) -> &TurnUsage {
        &self.usage
    }

    /// Copies this completed boundary into a serializable, caller-owned session snapshot.
    ///
    /// The snapshot contains the complete unredacted model-visible conversation,
    /// including reasoning payloads and tool inputs and outputs. Applications are
    /// responsible for protecting and retaining serialized snapshots appropriately.
    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        self.checkpoint.snapshot()
    }
}

impl fmt::Debug for TurnResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnResult")
            .field("final_message", &self.final_message)
            .finish_non_exhaustive()
    }
}

pub(super) enum Command {
    Prompt {
        key: TurnKey,
        prompt: Prompt,
        thinking: Option<Thinking>,
        fast_mode: Option<bool>,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Steer {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<()>>,
    },
    RoutePrompt {
        key: TurnKey,
        prompt: Prompt,
        parent: Option<tracing::Span>,
        events: EventSink,
        turn_result: oneshot::Sender<Result<TurnResult>>,
        route_result: oneshot::Sender<Result<PromptRouteKind>>,
    },
    Cancel {
        key: TurnKey,
        result: oneshot::Sender<Result<()>>,
    },
    Fork {
        checkpoint: Option<Arc<CommittedSession>>,
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    Spawn {
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    SetThinking {
        thinking: Thinking,
        result: oneshot::Sender<Result<()>>,
    },
    SetFastMode {
        enabled: bool,
        result: oneshot::Sender<Result<()>>,
    },
    Compact {
        parent: Option<tracing::Span>,
        result: oneshot::Sender<Result<()>>,
    },
    AppendDeveloperMessage {
        text: String,
        result: oneshot::Sender<Result<AgentSessionContext>>,
    },
    Snapshot {
        result: oneshot::Sender<Result<SessionSnapshot>>,
    },
    CancelCompaction {
        result: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}

#[derive(Clone, Copy)]
pub(super) enum PromptRouteKind {
    Started,
    Steered,
}

pub(super) enum QueuedTurn {
    Pending {
        key: TurnKey,
        prompt: Prompt,
        thinking: Thinking,
        fast_mode: bool,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Cancelled {
        prompt: Prompt,
        thinking: Thinking,
        fast_mode: bool,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
}
