use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Strategy {
    #[default]
    Provider,
    Snapcompact,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Fallback {
    #[default]
    Stop,
    Provider,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Profile {
    #[default]
    #[serde(rename = "validated-auto")]
    ValidatedAuto,
    #[serde(rename = "openai-8x16-experimental-v1")]
    Experimental8x16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CompactionConfig {
    pub(crate) strategy: Strategy,
    pub(crate) fallback: Fallback,
    pub(crate) profile: Profile,
    pub(crate) input_budget_tokens: u64,
    pub(crate) max_generated_pages: usize,
    pub(crate) max_request_bytes: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            strategy: Strategy::Provider,
            fallback: Fallback::Stop,
            profile: Profile::ValidatedAuto,
            input_budget_tokens: 272_000,
            max_generated_pages: 64,
            max_request_bytes: 32 * 1024 * 1024,
        }
    }
}

impl CompactionConfig {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        // The ceiling tracks the largest context window an operator can actually
        // configure, so million-token models are expressible. The default stays
        // conservative; raising it here only widens what is accepted.
        if !(16_384..=1_000_000).contains(&self.input_budget_tokens) {
            return Err("agent.compaction.input_budget_tokens must be between 16384 and 1000000");
        }
        if !(1..=64).contains(&self.max_generated_pages) {
            return Err("agent.compaction.max_generated_pages must be between 1 and 64");
        }
        if !(1024 * 1024..=32 * 1024 * 1024).contains(&self.max_request_bytes) {
            return Err("agent.compaction.max_request_bytes must be between 1048576 and 33554432");
        }
        Ok(())
    }

    pub(crate) fn validate_for_session(&self) -> Result<(), &'static str> {
        self.validate()?;
        if self.strategy == Strategy::Snapcompact && self.profile == Profile::ValidatedAuto {
            return Err(
                "SnapCompact has no validated model profile yet; explicitly select openai-8x16-experimental-v1 to evaluate it",
            );
        }
        Ok(())
    }
}
