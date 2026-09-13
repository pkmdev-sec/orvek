//! Borrowed API telemetry parsing for content-free session facts.

use super::record::TranscriptRecord;
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Deserialize)]
pub(crate) struct ApiEvent<'a> {
    pub(crate) direction: &'a str,
    pub(crate) phase: &'a str,
    #[serde(borrow)]
    pub(crate) event: &'a RawValue,
}

#[derive(Deserialize)]
struct ApiRequest<'a> {
    #[serde(borrow)]
    prompt_cache_key: Option<&'a RawValue>,
    #[serde(borrow)]
    previous_response_id: Option<&'a RawValue>,
}

fn raw_value_is_string(value: &RawValue) -> bool {
    value.get().trim_start().starts_with('"')
}

pub(crate) fn request_context_snapshot(request: &RawValue) -> Option<(bool, bool)> {
    let request = serde_json::from_str::<ApiRequest>(request.get()).ok()?;
    Some((
        request.prompt_cache_key.is_some_and(raw_value_is_string),
        request
            .previous_response_id
            .is_some_and(raw_value_is_string),
    ))
}

pub(crate) fn outbound_context_snapshot(record: &TranscriptRecord) -> Option<(bool, bool)> {
    let payload = record.decode_payload::<ApiEvent>().ok()?;
    if payload.direction != "outbound" || payload.phase != "generation" {
        return None;
    }
    request_context_snapshot(payload.event)
}
