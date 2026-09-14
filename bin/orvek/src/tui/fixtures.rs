//! Rendering fixtures expressed as native host/view observations, never a legacy engine.
use super::{host_projection::ViewChange, transcript::TranscriptRecord};
use orvek_harness::{
    Digest,
    inference::Usage,
    session::{SessionCursor, SessionId},
    state::{Job, JobInvocation, JobStatus, TaskEvent, TaskId},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub(crate) enum DisplaySample {
    Text,
    TextDelta,
    Reasoning,
    Start,
    End,
    ToolStart,
    ToolReturn,
    Usage,
    ApiUsage,
    Retry,
}

pub(crate) fn record(
    sequence: u64,
    at_ms: u64,
    kind: DisplaySample,
    data: Value,
) -> TranscriptRecord {
    let request = Uuid::from_u128(data["model_call_index"].as_u64().unwrap_or(0) as u128);
    let text = data["text"].as_str().unwrap_or_default().to_owned();
    let item = data["item_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("message-{}", data["phase"].as_str().unwrap_or("final")));
    let changes = match kind {
        DisplaySample::Text | DisplaySample::TextDelta => vec![ViewChange::Assistant {
            request: Some(request),
            item,
            text,
            replace: matches!(kind, DisplaySample::Text),
            confirmed: matches!(kind, DisplaySample::Text),
        }],
        DisplaySample::Reasoning => vec![ViewChange::Reasoning {
            request: Some(request),
            item,
            text,
            replace: false,
        }],
        DisplaySample::Start => vec![ViewChange::RequestStarted {
            request: Uuid::nil(),
        }],
        DisplaySample::End => vec![ViewChange::RequestSettled {
            request: Uuid::nil(),
            error: None,
        }],
        DisplaySample::Retry => vec![ViewChange::Warning(
            "Provider retry timing was not recorded by the host".into(),
        )],
        DisplaySample::Usage | DisplaySample::ApiUsage => {
            let value = if matches!(kind, DisplaySample::ApiUsage) {
                &data["event"]["response"]["usage"]
            } else {
                &data["usage"]
            };
            let usage = Usage {
                input_tokens: value["input_tokens"].as_u64(),
                output_tokens: value["output_tokens"].as_u64(),
                total_tokens: value["total_tokens"].as_u64(),
                cached_input_tokens: value["input_tokens_details"]["cached_tokens"].as_u64(),
                reasoning_tokens: value["output_tokens_details"]["reasoning_tokens"].as_u64(),
            };
            vec![ViewChange::ProviderUsage { usage }]
        }
        DisplaySample::ToolStart | DisplaySample::ToolReturn => {
            let name = data["tool"].as_str().unwrap_or("fixture_tool").to_owned();
            let call_id = data["call_id"]
                .as_str()
                .unwrap_or("fixture-call")
                .to_owned();
            let hash = Digest::of(call_id.as_bytes()).to_string();
            let job_id = Uuid::parse_str(&format!(
                "{}-{}-{}-{}-{}",
                &hash[..8],
                &hash[8..12],
                &hash[12..16],
                &hash[16..20],
                &hash[20..32]
            ))
            .unwrap();
            let job = Job {
                id: job_id,
                generation: 1,
                status: JobStatus::Running,
                mutates_candidate: false,
                check: None,
                identity: None,
                started_ms: at_ms,
                deadline_ms: at_ms + 30_000,
                invocation: Some(JobInvocation {
                    session: SessionId(Uuid::nil()),
                    request,
                    call_id: Some(call_id.clone()),
                    capability: name.clone(),
                    input: Digest::of_value(&data["arguments"]).unwrap(),
                    environment: Digest::of(b"fixture"),
                }),
                fence_receipt: None,
                execution_receipt: None,
            };
            if matches!(kind, DisplaySample::ToolStart) {
                vec![
                    ViewChange::ToolProposed {
                        request: Some(request),
                        call_id,
                        name,
                        arguments: data["arguments"].to_string(),
                    },
                    ViewChange::Task {
                        id: TaskId(Uuid::nil()),
                        event: TaskEvent::JobStarted(job),
                    },
                ]
            } else {
                let mut result = data
                    .get("structured_result")
                    .filter(|value| !value.is_null())
                    .or_else(|| data.get("result"))
                    .cloned()
                    .unwrap_or(Value::Null);
                if name == "exec_command" {
                    let code = result["exit_code"].as_i64().unwrap_or(0);
                    let text = result["output"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| result.as_str().unwrap_or_default().to_owned());
                    let elapsed = data["duration_ns"]
                        .as_u64()
                        .map(|ns| ns / 1_000_000)
                        .unwrap_or(0);
                    result = json!({"status":{"kind":"exited","code":code},"stdout":{"encoding":"utf8","data":text},"stderr":{"encoding":"utf8","data":""},"elapsed_ms":elapsed,"output_truncated":false});
                }
                let failed = data["status"] == "failed"
                    || result["exit_code"].as_i64().is_some_and(|code| code != 0);
                vec![
                    ViewChange::Task {
                        id: TaskId(Uuid::nil()),
                        event: TaskEvent::JobStarted(job),
                    },
                    ViewChange::ToolResult {
                        request: Some(request),
                        call_id,
                        output: result.to_string(),
                    },
                    ViewChange::Task {
                        id: TaskId(Uuid::nil()),
                        event: TaskEvent::JobSettled {
                            id: job_id,
                            status: if failed {
                                JobStatus::Failed
                            } else {
                                JobStatus::Succeeded
                            },
                            receipt: Some(
                                Digest::of_value(&json!({"fixture":true,"result":result})).unwrap(),
                            ),
                        },
                    },
                ]
            }
        }
    };
    TranscriptRecord::from_host_batch(
        sequence,
        at_ms,
        SessionCursor {
            version: 1,
            session: SessionId(Uuid::nil()),
            revision: sequence,
        },
        changes,
    )
}

/// Compact numeric IDs keep rendering scenarios readable; their wire form is UUID.
pub(crate) fn native_ids(mut value: Value) -> Value {
    fn visit(value: &mut Value, key: Option<&str>) {
        if matches!(
            key,
            Some("id" | "message_id" | "thread_id" | "child_id" | "to" | "in_reply_to")
        ) && let Some(id) = value.as_u64()
        {
            *value = Uuid::from_u128(id as u128).to_string().into();
            return;
        }
        match value {
            Value::Object(fields) => {
                if fields.get("kind").is_some_and(|kind| kind == "agent") {
                    fields.insert("kind".into(), "child".into());
                }
                for (key, value) in fields {
                    visit(value, Some(key));
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, None)
                }
            }
            _ => {}
        }
    }
    visit(&mut value, None);
    value
}
