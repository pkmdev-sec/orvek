use crate::{Result, session::CommittedSession};

#[derive(Clone, Default)]
pub(crate) struct DurabilityConfig;

impl DurabilityConfig {
    pub(crate) const fn for_new_thread(&self) -> Self {
        Self
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn start(
        &self,
        _session_id: &str,
        _workspace: Option<&str>,
        _instructions: &str,
        _origin_kind: &'static str,
        _parent_session_id: Option<&str>,
        _resume_history_len: Option<usize>,
    ) -> Result<Durability> {
        Ok(Durability)
    }
}

#[derive(Clone)]
pub(crate) struct Durability;

impl Durability {
    pub(crate) const fn start_turn(
        &self,
        _prompt: &nanocodex_oai_api::Prompt,
        _effort: nanocodex_oai_api::Thinking,
    ) -> DurabilityTurn {
        DurabilityTurn
    }

    pub(crate) const fn start_compaction(
        &self,
        _effort: nanocodex_oai_api::Thinking,
    ) -> DurabilityTurn {
        DurabilityTurn
    }

    pub(crate) async fn persist(&self, _checkpoint: &CommittedSession, _turn: DurabilityTurn) {}

    pub(crate) async fn persist_compaction(
        &self,
        _checkpoint: &CommittedSession,
        _turn: DurabilityTurn,
    ) {
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct DurabilityTurn;

impl DurabilityTurn {
    pub(crate) fn completed(self, _final_message: String) -> Self {
        self
    }

    pub(crate) const fn completed_without_message(self) -> Self {
        self
    }

    pub(crate) const fn interrupted(self) -> Self {
        self
    }

    pub(crate) const fn replaced(self) -> Self {
        self
    }

    pub(crate) const fn failed(self) -> Self {
        self
    }
}
