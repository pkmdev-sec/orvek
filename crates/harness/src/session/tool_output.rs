use crate::Digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

/// Shared lossless assembly for journal replay and disposable client projections.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolOutputBuffers(BTreeMap<String, PendingOutput>);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PendingOutput {
    request: Uuid,
    text: String,
}

#[derive(Debug, Error)]
pub enum ToolOutputError {
    #[error("tool output part is out of order or belongs to another request")]
    Part,
    #[error("tool output end has no parts or its digest does not match")]
    End,
}

impl ToolOutputBuffers {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn append(
        &mut self,
        request: Uuid,
        call_id: &str,
        offset: usize,
        text: &str,
    ) -> Result<(), ToolOutputError> {
        if text.is_empty() || (!self.0.contains_key(call_id) && offset != 0) {
            return Err(ToolOutputError::Part);
        }
        let pending = self
            .0
            .entry(call_id.to_owned())
            .or_insert_with(|| PendingOutput {
                request,
                text: String::new(),
            });
        if pending.request != request || pending.text.len() != offset {
            return Err(ToolOutputError::Part);
        }
        pending.text.push_str(text);
        Ok(())
    }

    pub fn finish(
        &mut self,
        request: Uuid,
        call_id: &str,
        digest: Digest,
    ) -> Result<String, ToolOutputError> {
        let pending = self.0.get(call_id).ok_or(ToolOutputError::End)?;
        if pending.request != request || Digest::of(pending.text.as_bytes()) != digest {
            return Err(ToolOutputError::End);
        }
        Ok(self.0.remove(call_id).ok_or(ToolOutputError::End)?.text)
    }
}
