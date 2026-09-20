//! Durable local event configuration and receipts. Payloads carry no authority.
use crate::{
    Digest, StoreError, admission::RequestPolicy, contract::Limits, session::SessionId,
    submission::Submission,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_PENDING_EVENTS: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TriggerKind {
    Webhook,
    /// UTC milliseconds. Missed occurrences coalesce into the latest one.
    Interval {
        first_due_ms: u64,
        interval_ms: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub id: Uuid,
    pub session: SessionId,
    pub objective: String,
    pub policy: RequestPolicy,
    pub limits: Limits,
    pub trigger: TriggerKind,
}

impl SourceConfig {
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        if self.objective.trim().is_empty() || self.objective.len() > 16 * 1024 {
            return Err(StoreError::Invalid(
                "event objective must contain 1..16384 bytes",
            ));
        }
        if serde_json::to_vec(self)?.len() > 256 * 1024 {
            return Err(StoreError::Invalid(
                "event source configuration exceeds 262144 bytes",
            ));
        }
        self.policy.validate()?;
        self.limits.validate()?;
        if let TriggerKind::Interval {
            first_due_ms,
            interval_ms,
        } = self.trigger
            && (interval_ms < 1000 || first_due_ms.checked_add(interval_ms).is_none())
        {
            return Err(StoreError::Invalid(
                "interval requires at least 1000ms and a representable next occurrence",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceRecord {
    pub config: SourceConfig,
    pub authority: Digest,
    pub disabled: bool,
    pub next_due_ms: Option<u64>,
    pub last_key: Option<String>,
    /// Latest source-admission failure; cleared by a successful validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventRecord {
    pub source: Uuid,
    pub key: String,
    pub payload_digest: Digest,
    /// Pinned queue input; recovery never re-renders a historical event.
    pub input: Digest,
    pub session: SessionId,
    pub request: Uuid,
    pub received_ms: u64,
    /// Durable cancellation intent; never grants permission to repeat effects.
    pub cancel_requested: bool,
    /// Normal queue receipt, refreshed until the normal queue settles.
    pub submission: Option<Submission>,
    pub settled: bool,
    /// Admission errors are observable without losing the durable event.
    pub error: Option<String>,
}

pub(crate) fn validate_delivery(key: &str, payload: &str) -> Result<(), StoreError> {
    if key.is_empty() || key.len() > 256 || payload.len() > MAX_PAYLOAD_BYTES {
        return Err(StoreError::Invalid(
            "event key must contain 1..256 bytes; payload limit is 65536 bytes",
        ));
    }
    Ok(())
}

/// Integer arithmetic avoids a catch-up loop even after years of downtime.
pub(crate) fn latest_due(next: u64, interval: u64, now: u64) -> Option<(u64, u64, u64)> {
    if now < next {
        return None;
    }
    let count = (now - next) / interval + 1;
    let latest = next + (count - 1) * interval;
    Some((latest, latest.checked_add(interval)?, count))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catchup_is_latest_only_and_clock_rollback_waits() {
        assert_eq!(latest_due(1000, 1000, 999), None);
        assert_eq!(
            latest_due(1000, 1000, 1_000_999),
            Some((1_000_000, 1_001_000, 1000))
        );
        assert_eq!(latest_due(1_001_000, 1000, 900_000), None);
        assert_eq!(latest_due(u64::MAX - 100, 1000, u64::MAX), None);
    }
}
