//! Read-only assistance has its own turn outcome and cannot certify a coding task.
use crate::{Digest, state::ModelCallReceipt};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuxiliaryKind {
    Conversation,
    Reflection,
    Question,
    Handoff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuxiliaryContext {
    Clean,
    CurrentConversation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuxiliaryLimits {
    pub model_calls: u32,
    pub tokens: u64,
    pub elapsed_ms: u64,
}
impl Default for AuxiliaryLimits {
    fn default() -> Self {
        Self {
            model_calls: 8,
            tokens: 32_000,
            elapsed_ms: 120_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuxiliarySpec {
    pub kind: AuxiliaryKind,
    pub context: AuxiliaryContext,
    pub review: Option<Digest>,
    #[serde(default)]
    pub limits: AuxiliaryLimits,
}
impl AuxiliarySpec {
    pub fn visible(&self) -> bool {
        matches!(
            self.kind,
            AuxiliaryKind::Conversation | AuxiliaryKind::Reflection
        )
    }
    pub fn validate(&self) -> Result<(), crate::StoreError> {
        if self.limits.model_calls == 0
            || self.limits.model_calls > 32
            || self.limits.tokens == 0
            || self.limits.tokens > 256_000
            || self.limits.elapsed_ms == 0
            || self.limits.elapsed_ms > 600_000
        {
            return Err(crate::StoreError::Invalid(
                "auxiliary limits exceed the supported bounds",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuxiliaryStatus {
    Completed,
    Failed,
    Cancelled,
    BudgetExhausted,
    UnknownBilling,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuxiliaryReport {
    pub version: u32,
    pub kind: AuxiliaryKind,
    pub status: AuxiliaryStatus,
    pub text: String,
    pub model_calls: usize,
    pub tokens: Option<u64>,
    pub records: Vec<Digest>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AuxiliaryRecord {
    ClassificationIntended {
        input: Digest,
        call: Uuid,
    },
    ClassificationObserved {
        call: Uuid,
        receipt: ModelCallReceipt,
        kind: String,
    },
    Started {
        at_ms: u64,
        source: Option<Digest>,
        review: Option<Digest>,
    },
    ModelIntended {
        call: Uuid,
        input: Digest,
    },
    ModelObserved {
        call: Uuid,
        receipt: ModelCallReceipt,
    },
    ToolObserved {
        call_id: String,
        name: String,
        input: Digest,
        output: Digest,
    },
}

#[derive(Default)]
pub(crate) struct Accounting {
    pub started_ms: Option<u64>,
    pub calls: std::collections::BTreeMap<Uuid, Option<ModelCallReceipt>>,
}
impl Accounting {
    pub fn tokens(&self) -> Option<u64> {
        self.calls.values().try_fold(0u64, |total, receipt| {
            total.checked_add(receipt.as_ref()?.tokens?)
        })
    }
    pub fn apply(&mut self, record: &AuxiliaryRecord) -> Result<(), crate::StoreError> {
        match record {
            AuxiliaryRecord::ClassificationIntended { input, call } => {
                let _ = input;
                if self.started_ms.is_none() {
                    self.started_ms = Some(crate::store::now_ms());
                }
                if self.calls.contains_key(call) {
                    return Err(crate::StoreError::Invalid("classification call reused"));
                }
                self.calls.insert(*call, None);
            }
            AuxiliaryRecord::ClassificationObserved { call, receipt, .. }
                if self.calls.get(call) == Some(&None) =>
            {
                self.calls.insert(*call, Some(receipt.clone()));
            }
            AuxiliaryRecord::Started { at_ms, .. } if self.started_ms.is_none() => {
                self.started_ms = Some(*at_ms)
            }
            AuxiliaryRecord::ModelIntended { call, .. }
                if self.started_ms.is_some() && !self.calls.contains_key(call) =>
            {
                self.calls.insert(*call, None);
            }
            AuxiliaryRecord::ModelObserved { call, receipt }
                if self.calls.get(call) == Some(&None) =>
            {
                self.calls.insert(*call, Some(receipt.clone()));
            }
            AuxiliaryRecord::ToolObserved { .. } if self.started_ms.is_some() => {}
            _ => {
                return Err(crate::StoreError::Invalid(
                    "invalid auxiliary accounting transition",
                ));
            }
        }
        Ok(())
    }
}
