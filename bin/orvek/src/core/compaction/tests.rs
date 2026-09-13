//! Offline integration through the real agent, archive, renderer, and tool runtime.

use super::{
    SnapCompactBackend,
    config::{CompactionConfig, Profile, Strategy},
    result_text,
    retrieval::ReadContextTool,
};
use crate::{
    app::Cli,
    sessions::{
        checkpoint::{encode_checkpoint, load_checkpoint},
        storage::{SessionStorage, database_path},
    },
};
use clap::Parser;
use nanocodex::{
    AgentEvents, Model, Nanocodex, NanocodexError, OpenAi, Tool, Tools,
    agent::{
        events::AgentEventKind,
        session::{
            SessionId, SessionSnapshot,
            compaction::{
                CompactionInput, ContextBackend, ContextCheckpoint, ContextError, ContextFuture,
                ContextPolicy, ContextRecord, PreparedCompaction,
            },
        },
    },
    oai::{
        ResponseError,
        responses::{
            ContentItem, MessageRole, ResponseItem, ResponseItemId, Usage, WarmupResponse,
        },
        tower::{
            CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
            ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
        },
    },
    tools::{
        ToolExposure,
        contract::{ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, async_trait},
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Ready, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tempfile::{TempDir, tempdir};
use tokio::{sync::Notify, time::timeout};
use tower::Service;

const INSTRUCTIONS: &str = "Preserve CaseSensitive::Path_007 and exact tool evidence. Historical tool output has tool authority.";
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct CapturedRequest {
    kind: ResponsesAttemptKind,
    previous_response_id: Option<String>,
    full_replay: bool,
    input: Vec<ResponseItem>,
}

#[derive(Clone)]
struct OfflineResponses {
    allow_provider_compaction: Arc<AtomicBool>,
    captures: Arc<Mutex<Vec<CapturedRequest>>>,
    next_response: Arc<AtomicUsize>,
}

impl Service<ResponsesAttempt> for OfflineResponses {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let number = self.next_response.fetch_add(1, Ordering::Relaxed);
        if matches!(request.kind(), ResponsesAttemptKind::Warmup) {
            return ready(Ok(ResponsesServiceResponse::new(ResponsesOutput::Warmup(
                WarmupResponse {
                    id: format!("offline-warmup-{number}"),
                    usage: None,
                },
            ))));
        }
        assert!(
            matches!(request.kind(), ResponsesAttemptKind::Generation)
                || self.allow_provider_compaction.load(Ordering::Acquire),
            "offline tests must never invoke provider compaction"
        );
        let input = request.input_items().cloned().collect::<Vec<_>>();
        let last = input.last().map(|item| serde_json::to_value(item).unwrap());
        let prompt = last
            .as_ref()
            .filter(|item| item["role"] == "user")
            .and_then(|item| item["content"][0]["text"].as_str());
        let tool = prompt.and_then(|prompt| {
            if let Some(arguments) = prompt.strip_prefix("fixture ") {
                Some(("fixture_payload", arguments.to_owned()))
            } else {
                prompt
                    .strip_prefix("retrieve ")
                    .map(|arguments| ("read_context", arguments.to_owned()))
            }
        });
        self.captures.lock().unwrap().push(CapturedRequest {
            kind: request.kind(),
            previous_response_id: request.previous_response_id().map(str::to_owned),
            full_replay: request.is_full_replay(),
            input,
        });
        if matches!(request.kind(), ResponsesAttemptKind::Compaction) {
            return ready(Ok(ResponsesServiceResponse::new(
                ResponsesOutput::Compaction(nanocodex::oai::tower::CompactionOutput {
                    id: format!("offline-compaction-{number}"),
                    status: "completed".to_owned(),
                    item: ResponseItem::Compaction {
                        id: Some(format!("cmp_offline_{number}").into()),
                        encrypted_content: "opaque-local-fixture-state".into(),
                        created_by: None,
                        internal_chat_message_metadata_passthrough: None,
                    },
                    usage: Some(Usage {
                        input_tokens: 100,
                        output_tokens: 10,
                        total_tokens: 110,
                        ..Usage::default()
                    }),
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                }),
            )));
        }
        let (output_items, code_calls, final_message) = if let Some((name, arguments)) = tool {
            let call_id = format!("call_offline_{number}");
            let item = serde_json::from_value(json!({
                "type": "function_call", "id": format!("fc_offline_{number}"),
                "call_id": call_id, "name": name, "arguments": arguments,
            }))
            .unwrap();
            (
                vec![item],
                vec![CodeCall {
                    call_id,
                    name: name.to_owned(),
                    namespace: None,
                    input: arguments,
                    kind: CodeCallKind::Function,
                }],
                None,
            )
        } else {
            (
                vec![ResponseItem::message(
                    MessageRole::Assistant,
                    [ContentItem::output_text("done")],
                )],
                Vec::new(),
                Some("done".to_owned()),
            )
        };
        ready(Ok(ResponsesServiceResponse::new(
            ResponsesOutput::Generation(GenerationOutput {
                id: format!("offline-response-{number}"),
                status: "completed".to_owned(),
                end_turn: Some(code_calls.is_empty()),
                final_message,
                output_items,
                code_calls,
                usage: Some(Usage {
                    input_tokens: 100,
                    output_tokens: 1,
                    total_tokens: 101,
                    ..Usage::default()
                }),
                time_to_first_event_ns: 0,
                time_to_first_output_ns: None,
                pipeline_stats: ResponsePipelineStats::default(),
            }),
        )))
    }
}

struct FixturePayload;

#[async_trait]
impl Tool for FixturePayload {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "fixture_payload",
            "Returns deterministic local fixture text.",
            json!({
                "type":"object", "properties":{"index":{"type":"integer"}, "style":{"type":"string"}},
                "required":["index", "style"], "additionalProperties":false,
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input: Value = input.decode_json()?;
        if input["style"] == "media" {
            use base64::{Engine as _, engine::general_purpose::STANDARD};
            use nanocodex::tools::contract::ToolOutputContent;
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1, 1)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            return Ok(ToolOutput::content(vec![
                ToolOutputContent::InputText {
                    text: payload(input["index"].as_u64().unwrap(), "ascii"),
                },
                ToolOutputContent::InputImage {
                    image_url: format!(
                        "data:image/png;base64,{}",
                        STANDARD.encode(bytes.into_inner())
                    ),
                    detail: nanocodex::oai::ImageDetail::Original,
                },
                ToolOutputContent::InputAudio {
                    audio_url: "data:audio/wav;base64,AQID".to_owned(),
                },
            ]));
        }
        Ok(ToolOutput::text(payload(
            input["index"].as_u64().unwrap(),
            input["style"].as_str().unwrap(),
        )))
    }
}

fn payload(index: u64, style: &str) -> String {
    let mut text = format!(
        "source_{index:03}\n  let exact_identifier_{index} = \"CaseSensitive::Path_007\";\n\tassert_eq!(value,  17);\n"
    );
    if style == "unicode" {
        text.push_str("lambda=λ; 日本語;\n");
    }
    let line = if style == "tall" {
        "  preserve eighty character lines with whitespace and exact punctuation: []{}();\n"
    } else {
        "  value += exact_identifier; // preserve  two spaces and punctuation! "
    };
    let length = if style == "expanded" { 52_000 } else { 40_000 };
    while text.len() < length {
        text.push_str(line);
    }
    text.truncate(text.floor_char_boundary(length));
    text
}

struct Fixture {
    directory: TempDir,
    backend: Arc<SnapCompactBackend>,
    agent_backend: Arc<dyn ContextBackend>,
    service: OfflineResponses,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let config = CompactionConfig {
            strategy: Strategy::Snapcompact,
            profile: Profile::Experimental8x16,
            ..CompactionConfig::default()
        };
        let backend =
            SnapCompactBackend::new(config, &directory.path().join("config.toml")).unwrap();
        Self {
            directory,
            agent_backend: backend.clone(),
            backend,
            service: OfflineResponses {
                allow_provider_compaction: Arc::new(AtomicBool::new(false)),
                captures: Arc::new(Mutex::new(Vec::new())),
                next_response: Arc::new(AtomicUsize::new(0)),
            },
        }
    }

    fn build(
        &self,
        snapshot: Option<SessionSnapshot>,
    ) -> Result<(Nanocodex, AgentEvents), NanocodexError> {
        self.build_with_instructions(snapshot, INSTRUCTIONS)
    }

    fn build_with_instructions(
        &self,
        snapshot: Option<SessionSnapshot>,
        instructions: &str,
    ) -> Result<(Nanocodex, AgentEvents), NanocodexError> {
        let service = self.service.clone();
        let openai = OpenAi::builder("unused-local-fixture")
            .service(move || service.clone())
            .build()
            .unwrap();
        let tools = Tools::builder()
            .without_defaults()
            .exposure(ToolExposure::DirectAndCodeMode)
            .tool(FixturePayload)
            .tool(ReadContextTool(Arc::clone(&self.backend)))
            .build()
            .unwrap();
        let builder = Nanocodex::builder(openai)
            .model(Model::Sol)
            .workspace(self.directory.path())
            .instructions(instructions.to_owned())
            .tools(tools)
            .context_backend(self.agent_backend.clone());
        match snapshot {
            Some(snapshot) => builder.resume(snapshot).build(),
            None => builder.build(),
        }
    }

    fn last_request(&self) -> CapturedRequest {
        self.service
            .captures
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .clone()
    }
}

async fn prompt(agent: &Nanocodex, text: impl Into<String>) -> SessionSnapshot {
    let text = text.into();
    let turn = timeout(DEADLINE, agent.prompt(text))
        .await
        .expect("prompt admission timed out")
        .unwrap();
    let completed = timeout(DEADLINE, turn.result())
        .await
        .expect("offline model turn timed out")
        .unwrap();
    assert_eq!(completed.final_message(), "done");
    completed.snapshot()
}

async fn seed(agent: &Nanocodex, start: u64, count: u64, style: &str) -> SessionSnapshot {
    let mut snapshot = None;
    for index in start..start + count {
        snapshot = Some(
            prompt(
                agent,
                format!("fixture {}", json!({"index":index,"style":style})),
            )
            .await,
        );
    }
    snapshot.unwrap()
}

async fn compact(agent: &Nanocodex) -> SessionSnapshot {
    timeout(DEADLINE, agent.compact())
        .await
        .expect("local compaction timed out")
        .unwrap();
    timeout(DEADLINE, agent.snapshot())
        .await
        .expect("snapshot timed out")
        .unwrap()
}

fn items(history: &[ResponseItem]) -> Vec<Value> {
    history
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect()
}

fn outputs(history: &[ResponseItem]) -> BTreeMap<String, Value> {
    items(history)
        .into_iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| (item["call_id"].as_str().unwrap().to_owned(), item))
        .collect()
}

fn calls(history: &[ResponseItem]) -> BTreeMap<String, Value> {
    items(history)
        .into_iter()
        .filter(|item| item["type"] == "function_call")
        .map(|item| (item["call_id"].as_str().unwrap().to_owned(), item))
        .collect()
}

fn images(history: &[ResponseItem]) -> BTreeMap<String, Vec<String>> {
    outputs(history)
        .into_iter()
        .filter_map(|(call, item)| {
            let urls = item["output"]
                .as_array()?
                .iter()
                .filter(|part| part["type"] == "input_image")
                .map(|part| {
                    assert_eq!(part["detail"], "original");
                    let url = part["image_url"].as_str().unwrap();
                    assert!(url.starts_with("data:image/png;base64,"));
                    url.to_owned()
                })
                .collect::<Vec<_>>();
            (!urls.is_empty()).then_some((call, urls))
        })
        .collect()
}

fn last_output(snapshot: &SessionSnapshot) -> Value {
    items(snapshot.history())
        .into_iter()
        .rev()
        .find(|item| item["type"] == "function_call_output")
        .unwrap()
}

fn output_text(item: &Value) -> &str {
    item["output"]
        .as_str()
        .expect("fixture result is native text")
}

async fn retrieve(agent: &Nanocodex, input: Value) -> Value {
    let snapshot = prompt(agent, format!("retrieve {input}")).await;
    serde_json::from_str(output_text(&last_output(&snapshot))).expect("read_context returned JSON")
}

#[tokio::test]
async fn manual_compaction_sends_real_images_with_native_roles_pairing_and_recent_groups() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    let before = seed(&agent, 0, 8, "ascii").await;
    let before_json = serde_json::to_value(&before).unwrap();
    let before_outputs = outputs(before.history());
    assert_eq!(before_outputs.len(), 8);
    assert!(images(before.history()).is_empty());
    assert!(
        before_outputs
            .values()
            .all(|item| output_text(item).len() == 40_000)
    );
    let compacted = compact(&agent).await;
    let prefix: Vec<ResponseItem> =
        serde_json::from_value(before_json["request_prefix"].clone()).unwrap();
    let native_tokens = fixture
        .backend
        .estimate(Model::Sol, &prefix, before.history())
        .unwrap();
    let projected_tokens = fixture
        .backend
        .estimate(Model::Sol, &prefix, compacted.history())
        .unwrap();
    assert!(
        projected_tokens <= native_tokens * 9 / 10,
        "projection must save at least ten percent: {native_tokens} -> {projected_tokens}"
    );
    assert!(projected_tokens <= fixture.backend.policy().input_tokens * 7 / 10);
    let projected_images = images(compacted.history());
    assert!(
        !projected_images.is_empty(),
        "manual compaction must actually replace text with images"
    );
    assert_eq!(calls(before.history()), calls(compacted.history()));
    let after_outputs = outputs(compacted.history());
    assert_eq!(
        before_outputs.keys().collect::<Vec<_>>(),
        after_outputs.keys().collect::<Vec<_>>()
    );
    let checkpoint = before.context_checkpoint().unwrap();
    let mut protected = 0;
    for (call, output) in &before_outputs {
        let source_id = output["id"].as_str().unwrap();
        let archived = fixture.backend.read(agent.session_id(), source_id).unwrap();
        if archived.checkpoint.model_generation > checkpoint.model_generation - 2 {
            protected += 1;
            assert_eq!(
                &after_outputs[call], output,
                "the newest provider-response groups remain native"
            );
        }
        if projected_images.contains_key(call) {
            let original: ResponseItem = serde_json::from_slice(&archived.original).unwrap();
            assert_eq!(result_text(&original)[0].1, output_text(output));
        }
    }
    assert!(protected > 0);
    prompt(&agent, "inspect compacted history").await;
    let captured = fixture.last_request();
    assert!(captured.full_replay);
    assert_eq!(captured.previous_response_id, None);
    assert_eq!(images(&captured.input), projected_images);
    let input = items(&captured.input);
    let prefix = before_json["request_prefix"].as_array().unwrap();
    assert_eq!(&input[..prefix.len()], prefix);
    assert!(
        prefix
            .iter()
            .any(|item| item["role"] == "developer" && item.to_string().contains(INSTRUCTIONS))
    );
    assert!(
        input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .all(|item| item.get("role").is_none())
    );
    agent.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn serialized_restore_and_initial_fork_keep_the_same_provider_ready_projection() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    seed(&agent, 0, 8, "ascii").await;
    let compacted = compact(&agent).await;
    let original_images = images(compacted.history());
    assert!(!original_images.is_empty());
    let saved_branch = compacted.context_checkpoint().unwrap().branch;
    let encoded = serde_json::to_vec(&compacted).unwrap();
    let decoded: SessionSnapshot = serde_json::from_slice(&encoded).unwrap();
    let (restored, restored_events) = fixture.build(Some(decoded)).unwrap();
    let initial = timeout(DEADLINE, restored.snapshot())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(items(initial.history()), items(compacted.history()));
    assert_ne!(initial.context_checkpoint().unwrap().branch, saved_branch);
    assert_eq!(initial.context_checkpoint().unwrap().sequence, 0);
    prompt(&restored, "inspect restored history").await;
    let replay = fixture.last_request();
    assert!(replay.full_replay);
    assert!(replay.previous_response_id.is_none());
    assert_eq!(images(&replay.input), original_images);
    let (fork, fork_events) = timeout(DEADLINE, agent.fork()).await.unwrap().unwrap();
    let initial_fork = timeout(DEADLINE, fork.snapshot()).await.unwrap().unwrap();
    assert_ne!(
        initial_fork.context_checkpoint().unwrap().branch,
        saved_branch
    );
    assert_ne!(
        initial_fork.context_checkpoint().unwrap().branch,
        initial.context_checkpoint().unwrap().branch
    );
    assert_eq!(initial_fork.context_checkpoint().unwrap().sequence, 0);
    assert_eq!(items(initial_fork.history()), items(compacted.history()));
    fixture.backend.validate(&initial_fork).unwrap();
    fork.shutdown().await.unwrap();
    restored.shutdown().await.unwrap();
    agent.shutdown().await.unwrap();
    drop((events, restored_events, fork_events));
}

#[tokio::test]
async fn provider_override_restores_native_text_without_losing_archive_access() {
    let mut fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    let native = seed(&agent, 0, 8, "ascii").await;
    let compacted = compact(&agent).await;
    assert!(!images(compacted.history()).is_empty());
    agent.shutdown().await.unwrap();
    drop(events);
    fixture.backend = SnapCompactBackend::new(
        CompactionConfig {
            strategy: Strategy::Provider,
            profile: Profile::Experimental8x16,
            ..CompactionConfig::default()
        },
        &fixture.directory.path().join("config.toml"),
    )
    .unwrap();
    fixture.agent_backend = fixture.backend.clone();
    let (restored, events) = fixture.build(Some(compacted)).unwrap();
    let recovered = restored.snapshot().await.unwrap();
    assert!(recovered.context_checkpoint().unwrap().manifest.is_none());
    assert_eq!(items(recovered.history()), items(native.history()));
    fixture.backend.validate(&recovered).unwrap();
    prompt(&restored, "continue with native text").await;
    let request = fixture.last_request();
    assert!(request.full_replay);
    assert!(images(&request.input).is_empty());
    let first = native
        .history()
        .iter()
        .find(|item| !result_text(item).is_empty())
        .unwrap();
    assert!(
        fixture
            .backend
            .read(restored.session_id(), first.id().unwrap().as_str())
            .is_ok()
    );
    fixture
        .service
        .allow_provider_compaction
        .store(true, Ordering::Release);
    restored.compact().await.unwrap();
    let request = fixture.last_request();
    assert!(matches!(request.kind, ResponsesAttemptKind::Compaction));
    assert!(images(&request.input).is_empty());
    let after = restored.snapshot().await.unwrap();
    assert!(after.context_checkpoint().unwrap().manifest.is_none());
    assert!(images(after.history()).is_empty());
    restored.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn provider_recovery_uses_intact_sources_when_bitmap_storage_is_corrupt() {
    let mut fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    let native = seed(&agent, 0, 8, "ascii").await;
    let compacted = compact(&agent).await;
    agent.shutdown().await.unwrap();
    drop(events);
    let path = database_path(&fixture.directory.path().join("config.toml"));
    let database = Connection::open(&path).unwrap();
    database
        .execute("UPDATE context_archive_pages SET png = X'00'", [])
        .unwrap();
    assert!(fixture.build(Some(compacted.clone())).is_err());
    fixture.backend = SnapCompactBackend::new(
        CompactionConfig {
            strategy: Strategy::Provider,
            profile: Profile::Experimental8x16,
            ..CompactionConfig::default()
        },
        &fixture.directory.path().join("config.toml"),
    )
    .unwrap();
    fixture.agent_backend = fixture.backend.clone();
    let captures = fixture.service.captures.lock().unwrap().len();
    let (recovered, events) = fixture.build(Some(compacted)).unwrap();
    let snapshot = recovered.snapshot().await.unwrap();
    assert_eq!(items(snapshot.history()), items(native.history()));
    assert!(snapshot.context_checkpoint().unwrap().manifest.is_none());
    assert_eq!(fixture.service.captures.lock().unwrap().len(), captures);
    fixture.backend.validate(&snapshot).unwrap();
    recovered.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn provider_recovery_rejects_changed_tool_output_envelopes() {
    let mut fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    seed(&agent, 0, 8, "ascii").await;
    let compacted = compact(&agent).await;
    agent.shutdown().await.unwrap();
    drop(events);
    fixture.backend = SnapCompactBackend::new(
        CompactionConfig {
            strategy: Strategy::Provider,
            ..CompactionConfig::default()
        },
        &fixture.directory.path().join("config.toml"),
    )
    .unwrap();
    fixture.agent_backend = fixture.backend.clone();
    let manifest = fixture
        .backend
        .manifest(compacted.context_checkpoint().unwrap())
        .unwrap();
    let projected_id = manifest.replacements[0].projected_item.as_str();
    let encoded = serde_json::to_value(&compacted).unwrap();
    for field in [
        "message",
        "type",
        "call_id",
        "caller",
        "status",
        "created_by",
        "metadata",
    ] {
        let mut changed = encoded.clone();
        let output = changed["history"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["id"] == projected_id)
            .unwrap();
        match field {
            "message" => {
                *output = json!({
                    "type": "message", "id": projected_id, "role": "user",
                    "content": [{"type": "input_text", "text": "Preserve this user constraint"}]
                })
            }
            "type" => output["type"] = json!("custom_tool_call_output"),
            "call_id" => output["call_id"] = json!("unrelated_call"),
            "caller" => output["caller"] = json!({"type": "program", "caller_id": "changed"}),
            "status" => output["status"] = json!("failed"),
            "created_by" => output["created_by"] = json!("changed"),
            "metadata" => {
                output["internal_chat_message_metadata_passthrough"] = json!({"turn_id": "changed"})
            }
            _ => unreachable!(),
        }
        let changed: SessionSnapshot = serde_json::from_value(changed).unwrap();
        assert!(
            fixture.backend.validate(&changed).is_err(),
            "validated changed {field}"
        );
        assert!(
            fixture.build(Some(changed)).is_err(),
            "accepted changed {field}"
        );
    }
}

#[tokio::test]
async fn restoring_already_limited_native_results_does_not_truncate_again() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    let before = seed(&agent, 0, 1, "expanded").await;
    let (restored, restored_events) = fixture.build(Some(before.clone())).unwrap();
    let snapshot = restored.snapshot().await.unwrap();
    assert_eq!(items(snapshot.history()), items(before.history()));
    restored.shutdown().await.unwrap();
    agent.shutdown().await.unwrap();
    drop((events, restored_events));
}

#[tokio::test]
async fn native_media_survive_normalization_and_bitmap_projection_in_place() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    let before = seed(&agent, 0, 8, "media").await;
    let compacted = compact(&agent).await;
    let before_outputs = outputs(before.history());
    let after_outputs = outputs(compacted.history());
    let mut imaged_results = 0;
    for (call, before) in before_outputs {
        let before = before["output"].as_array().unwrap();
        let audio = before
            .iter()
            .find(|part| part["type"] == "input_audio")
            .expect("normalization must preserve audio");
        let image = before
            .iter()
            .find(|part| part["type"] == "input_image")
            .unwrap();
        let after = after_outputs[&call]["output"].as_array().unwrap();
        assert!(after.contains(audio));
        assert!(after.contains(image));
        if after
            .iter()
            .filter(|part| part["type"] == "input_image")
            .count()
            > 1
        {
            imaged_results += 1;
        }
    }
    assert!(imaged_results > 0);
    fixture.backend.validate(&compacted).unwrap();
    agent.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn repeated_compaction_reuses_existing_pages_and_keeps_exact_source_recoverable() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    let before = seed(&agent, 0, 8, "ascii").await;
    let original = outputs(before.history());
    let first = compact(&agent).await;
    let first_images = images(first.history());
    assert!(!first_images.is_empty());
    seed(&agent, 8, 4, "ascii").await;
    let second = compact(&agent).await;
    let second_images = images(second.history());
    assert!(second_images.len() > first_images.len());
    assert_ne!(
        first.context_checkpoint().unwrap().manifest,
        second.context_checkpoint().unwrap().manifest
    );
    for (call, pages) in &first_images {
        assert_eq!(
            &second_images[call], pages,
            "existing pages must not be rerendered"
        );
        let source_id = original[call]["id"].as_str().unwrap();
        let archived = fixture.backend.read(agent.session_id(), source_id).unwrap();
        let item: ResponseItem = serde_json::from_slice(&archived.original).unwrap();
        assert_eq!(result_text(&item)[0].1, output_text(&original[call]));
    }
    fixture.backend.validate(&second).unwrap();
    agent.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn real_read_context_enforces_original_visible_utf8_and_fork_boundaries() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    let expanded = seed(&agent, 90, 1, "expanded").await;
    let source_id = last_output(&expanded)["id"].as_str().unwrap().to_owned();
    fixture.backend.flush().await.unwrap();
    let archived = fixture
        .backend
        .read(agent.session_id(), &source_id)
        .unwrap();
    let original_item: ResponseItem = serde_json::from_slice(&archived.original).unwrap();
    let visible_item: ResponseItem = serde_json::from_slice(&archived.visible).unwrap();
    let original = result_text(&original_item)[0].1;
    let visible = result_text(&visible_item)[0].1;
    assert_eq!(original, payload(90, "expanded"));
    assert!(
        visible.len() < original.len(),
        "fixture must cross the ordinary output limit"
    );
    let slice = retrieve(
        &agent,
        json!({"item":source_id,"source":"model_visible","offset":0,"limit_bytes":128}),
    )
    .await;
    assert_eq!(slice["text"], &visible[..128]);
    assert_eq!(slice["next_offset"], 128);
    assert_eq!(slice["role"], "tool");
    let original_tail = retrieve(
        &agent,
        json!({"item":source_id,"source":"original","offset":visible.len(),"limit_bytes":128}),
    )
    .await;
    assert_eq!(
        original_tail["text"],
        &original[visible.len()..visible.len() + 128]
    );
    assert_eq!(original_tail["total_bytes"], original.len());
    let visible_end = retrieve(
        &agent,
        json!({"item":source_id,"source":"model_visible","offset":visible.len(),"limit_bytes":128}),
    )
    .await;
    assert_eq!(visible_end["text"], "");
    assert_eq!(visible_end["total_bytes"], visible.len());

    let unicode = seed(&agent, 91, 1, "unicode").await;
    let unicode_output = last_output(&unicode);
    let unicode_id = unicode_output["id"].as_str().unwrap();
    let invalid_offset = output_text(&unicode_output).find('λ').unwrap() + 1;
    let failed = prompt(
        &agent,
        format!(
            "retrieve {}",
            json!({"item":unicode_id,"offset":invalid_offset,"limit_bytes":128})
        ),
    )
    .await;
    assert!(output_text(&last_output(&failed)).contains("UTF-8 boundary"));

    let (fork, fork_events) = agent.fork().await.unwrap();
    let (sibling, sibling_events) = agent.fork().await.unwrap();
    let late = seed(&agent, 92, 1, "ascii").await;
    let late_id = last_output(&late)["id"].as_str().unwrap().to_owned();
    fixture.backend.flush().await.unwrap();
    assert!(fixture.backend.read(fork.session_id(), &source_id).is_ok());
    assert!(fixture.backend.read(fork.session_id(), &late_id).is_err());
    let fork_result = seed(&fork, 93, 1, "ascii").await;
    let fork_only = last_output(&fork_result)["id"].as_str().unwrap().to_owned();
    fixture.backend.flush().await.unwrap();
    assert!(
        fixture
            .backend
            .read(sibling.session_id(), &fork_only)
            .is_err()
    );
    let denied = prompt(
        &sibling,
        format!("retrieve {}", json!({"item":fork_only,"limit_bytes":128})),
    )
    .await;
    assert!(output_text(&last_output(&denied)).contains("context archive"));
    let (independent, independent_events) = agent.spawn().await.unwrap();
    prompt(&independent, "initialize independent child").await;
    assert!(
        fixture
            .backend
            .read(independent.session_id(), &source_id)
            .is_err()
    );
    independent.shutdown().await.unwrap();
    fork.shutdown().await.unwrap();
    sibling.shutdown().await.unwrap();
    agent.shutdown().await.unwrap();
    drop((events, fork_events, sibling_events, independent_events));
}

#[tokio::test]
async fn unsupported_unicode_and_nonpositive_savings_preserve_the_entire_snapshot() {
    for style in ["unicode", "tall"] {
        let fixture = Fixture::new();
        let (agent, mut events) = fixture.build(None).unwrap();
        let before = seed(&agent, 0, 8, style).await;
        let before_json = serde_json::to_value(&before).unwrap();
        assert!(images(before.history()).is_empty());
        let result = timeout(DEADLINE, agent.compact()).await.unwrap();
        assert!(
            result.is_err(),
            "ineligible {style} fixture must fail without installing a projection"
        );
        let after = agent.snapshot().await.unwrap();
        assert_eq!(serde_json::to_value(&after).unwrap(), before_json);
        let mut failure = false;
        while let Some(event) = events.try_recv_timed() {
            failure |= event.event.kind == AgentEventKind::ModelCompactionFailed;
        }
        assert!(failure, "failed compaction must emit its failure event");
        agent.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn corrupted_saved_pages_fail_restoration_before_any_model_request() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    seed(&agent, 0, 8, "ascii").await;
    let saved = compact(&agent).await;
    assert!(!images(saved.history()).is_empty());
    agent.shutdown().await.unwrap();
    drop(events);
    let database =
        Connection::open(database_path(&fixture.directory.path().join("config.toml"))).unwrap();
    database
        .execute("UPDATE context_archive_pages SET png = X'00'", [])
        .unwrap();
    let count = fixture.service.captures.lock().unwrap().len();
    let result = fixture.build(Some(saved));
    assert!(
        result.is_err(),
        "corruption must fail the restore explicitly"
    );
    assert_eq!(fixture.service.captures.lock().unwrap().len(), count);
}

struct PausedPreparation {
    backend: Arc<SnapCompactBackend>,
    entered: Notify,
    dropped: Arc<AtomicBool>,
}

struct PreparationDropped(Arc<AtomicBool>);

impl Drop for PreparationDropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl ContextBackend for PausedPreparation {
    fn policy(&self) -> ContextPolicy {
        self.backend.policy()
    }
    fn open(
        &self,
        session: SessionId,
        inherited: Option<&ContextCheckpoint>,
    ) -> Result<ContextCheckpoint, ContextError> {
        self.backend.open(session, inherited)
    }
    fn record(&self, record: ContextRecord<'_>) -> Result<(), ContextError> {
        self.backend.record(record)
    }
    fn successful_results(
        &self,
        checkpoint: &ContextCheckpoint,
    ) -> Result<Vec<ResponseItemId>, ContextError> {
        self.backend.successful_results(checkpoint)
    }
    fn estimate(
        &self,
        model: Model,
        prefix: &[ResponseItem],
        history: &[ResponseItem],
    ) -> Result<u64, ContextError> {
        self.backend.estimate(model, prefix, history)
    }
    fn prepare<'a>(&'a self, _input: CompactionInput<'a>) -> ContextFuture<'a, PreparedCompaction> {
        Box::pin(async move {
            let _dropped = PreparationDropped(Arc::clone(&self.dropped));
            self.entered.notify_one();
            std::future::pending().await
        })
    }
    fn restore_text(
        &self,
        checkpoint: &ContextCheckpoint,
        history: &[ResponseItem],
    ) -> Result<Vec<ResponseItem>, ContextError> {
        self.backend.restore_text(checkpoint, history)
    }
    fn flush(&self) -> ContextFuture<'_, ()> {
        self.backend.flush()
    }
    fn validate(&self, snapshot: &SessionSnapshot) -> Result<(), ContextError> {
        self.backend.validate(snapshot)
    }
}

#[tokio::test]
async fn cancellation_drops_preparation_and_preserves_the_previous_durable_snapshot() {
    let mut fixture = Fixture::new();
    let barrier = Arc::new(PausedPreparation {
        backend: Arc::clone(&fixture.backend),
        entered: Notify::new(),
        dropped: Arc::new(AtomicBool::new(false)),
    });
    fixture.agent_backend = barrier.clone();
    let (agent, events) = fixture.build(None).unwrap();
    let before = seed(&agent, 0, 8, "ascii").await;
    let compacting_agent = agent.clone();
    let pending = tokio::spawn(async move { compacting_agent.compact().await });
    timeout(DEADLINE, barrier.entered.notified())
        .await
        .expect("preparation barrier was not reached");
    let during = timeout(DEADLINE, agent.snapshot()).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&during).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    timeout(DEADLINE, agent.cancel_compaction())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        timeout(DEADLINE, pending).await.unwrap().unwrap(),
        Err(NanocodexError::TurnCancelled)
    ));
    assert!(barrier.dropped.load(Ordering::Acquire));
    let after = timeout(DEADLINE, agent.snapshot()).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    fixture.backend.validate(&before).unwrap();
    let continued = prompt(&agent, "continue after cancelling compaction").await;
    assert_eq!(
        &items(continued.history())[..before.history().len()],
        items(before.history())
    );
    fixture.backend.validate(&continued).unwrap();
    let (restored, restored_events) = fixture.build(Some(before.clone())).unwrap();
    let recovered = restored.snapshot().await.unwrap();
    assert_eq!(items(recovered.history()), items(before.history()));
    let database =
        Connection::open(database_path(&fixture.directory.path().join("config.toml"))).unwrap();
    let count: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM context_archive_manifests",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "cancelled preparation must not publish a manifest"
    );
    restored.shutdown().await.unwrap();
    agent.shutdown().await.unwrap();
    drop((events, restored_events));
}

#[tokio::test]
async fn legacy_upgrade_verifies_the_old_contract_before_installing_archive_tools() {
    let fixture = Fixture::new();
    let openai = || {
        let service = fixture.service.clone();
        OpenAi::builder("unused-local-fixture")
            .service(move || service.clone())
            .build()
            .unwrap()
    };
    let original_tools = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectAndCodeMode)
        .tool(FixturePayload)
        .build()
        .unwrap();
    let (legacy_agent, legacy_events) = Nanocodex::builder(openai())
        .model(Model::Sol)
        .workspace(fixture.directory.path())
        .instructions(INSTRUCTIONS)
        .tools(original_tools.clone())
        .build()
        .unwrap();
    let legacy = seed(&legacy_agent, 0, 1, "ascii").await;
    assert_eq!(legacy.version(), 1);
    assert!(legacy.context_checkpoint().is_none());
    let original_history = items(legacy.history());
    let original_calls = calls(legacy.history());
    let original_outputs = outputs(legacy.history());
    legacy_agent.shutdown().await.unwrap();
    drop(legacy_events);

    let archive_tools = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectAndCodeMode)
        .tool(FixturePayload)
        .tool(ReadContextTool(Arc::clone(&fixture.backend)))
        .build()
        .unwrap();
    let archive_instructions = format!("{INSTRUCTIONS}\n{}", super::INSTRUCTIONS);
    let incorrect_tools = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectAndCodeMode)
        .build()
        .unwrap();
    let generations_before = fixture.service.captures.lock().unwrap().len();
    for (previous_tools, previous_instructions) in [
        (incorrect_tools, INSTRUCTIONS),
        (original_tools.clone(), "Different prior instructions."),
    ] {
        let rejected = Nanocodex::builder(openai())
            .model(Model::Sol)
            .workspace(fixture.directory.path())
            .instructions(archive_instructions.clone())
            .tools(archive_tools.clone())
            .context_backend(fixture.backend.clone())
            .resume_with_context_upgrade(legacy.clone(), previous_tools, previous_instructions)
            .build();
        assert!(
            rejected.is_err(),
            "a mismatched original contract must not be upgraded"
        );
        assert_eq!(
            fixture.service.captures.lock().unwrap().len(),
            generations_before
        );
    }
    let (upgraded, upgraded_events) = Nanocodex::builder(openai())
        .model(Model::Sol)
        .workspace(fixture.directory.path())
        .instructions(archive_instructions.clone())
        .tools(archive_tools)
        .context_backend(fixture.backend.clone())
        .resume_with_context_upgrade(legacy.clone(), original_tools, INSTRUCTIONS)
        .build()
        .unwrap();
    let initial = timeout(DEADLINE, upgraded.snapshot())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.version(), 2);
    assert!(initial.context_checkpoint().is_some());
    assert_eq!(items(initial.history()), original_history);
    assert!(images(initial.history()).is_empty());
    fixture.backend.validate(&initial).unwrap();
    prompt(&upgraded, "inspect upgraded legacy history").await;
    let replay = fixture.last_request();
    assert!(replay.full_replay);
    assert!(replay.previous_response_id.is_none());
    assert_eq!(calls(&replay.input), original_calls);
    assert_eq!(outputs(&replay.input), original_outputs);
    let input = items(&replay.input);
    assert!(input.iter().any(
        |item| item["type"] == "additional_tools" && item.to_string().contains("read_context")
    ));
    assert!(
        input
            .iter()
            .any(|item| item["role"] == "developer"
                && item.to_string().contains(super::INSTRUCTIONS))
    );
    assert_eq!(serde_json::to_value(&legacy).unwrap()["version"], 1);
    upgraded.shutdown().await.unwrap();
    drop(upgraded_events);
}

#[tokio::test]
async fn oversized_native_user_prompt_fails_before_generation_but_remains_archived() {
    let fixture = Fixture::new();
    let (agent, events) = fixture.build(None).unwrap();
    prompt(&agent, "establish a completed boundary").await;
    let generations_before = fixture.service.captures.lock().unwrap().len();
    let oversized = format!(
        "REQUIRED_NEW_USER_INSTRUCTION\n{}",
        "native user text; ".repeat(80_000)
    );
    let turn = timeout(DEADLINE, agent.prompt(oversized.clone()))
        .await
        .unwrap()
        .unwrap();
    let result = timeout(DEADLINE, turn.result()).await.unwrap();
    assert!(
        result.is_err(),
        "oversized protected native input cannot be compacted away"
    );
    assert_eq!(
        fixture.service.captures.lock().unwrap().len(),
        generations_before,
        "the rejected request must not reach model generation"
    );
    let snapshot = timeout(DEADLINE, agent.snapshot()).await.unwrap().unwrap();
    let accepted = snapshot.history().iter().find(|item| matches!(item,
        ResponseItem::Message { role: MessageRole::User, content, .. }
            if content.iter().any(|part| matches!(part, ContentItem::InputText { text } if text.as_ref() == oversized.as_str()))
    )).expect("the newly accepted user instruction must survive the failed preflight");
    fixture.backend.flush().await.unwrap();
    let archived = fixture
        .backend
        .read(agent.session_id(), accepted.id().unwrap().as_str())
        .unwrap();
    let original: ResponseItem = serde_json::from_slice(&archived.original).unwrap();
    assert!(matches!(original,
        ResponseItem::Message { role: MessageRole::User, content, .. }
            if content.iter().any(|part| matches!(part, ContentItem::InputText { text } if text.as_ref() == oversized.as_str()))
    ));
    agent.shutdown().await.unwrap();
    drop(events);
}

fn original_tools() -> Tools {
    Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectAndCodeMode)
        .tool(FixturePayload)
        .build()
        .unwrap()
}

fn build_legacy(fixture: &Fixture, snapshot: Option<SessionSnapshot>) -> (Nanocodex, AgentEvents) {
    let service = fixture.service.clone();
    let openai = OpenAi::builder("unused-local-fixture")
        .service(move || service.clone())
        .build()
        .unwrap();
    let builder = Nanocodex::builder(openai)
        .model(Model::Sol)
        .workspace(fixture.directory.path())
        .instructions(INSTRUCTIONS)
        .tools(original_tools());
    match snapshot {
        Some(snapshot) => builder.resume(snapshot).build(),
        None => builder.build(),
    }
    .unwrap()
}

struct PublishedUpgrade {
    agent: Nanocodex,
    events: AgentEvents,
    session_id: String,
    legacy: SessionSnapshot,
    legacy_encoded: Vec<u8>,
    instructions: String,
}

async fn publish_legacy_and_upgrade(fixture: &Fixture) -> PublishedUpgrade {
    let config_path = fixture.directory.path().join("config.toml");
    let (legacy_agent, legacy_events) = build_legacy(fixture, None);
    let legacy = seed(&legacy_agent, 100, 1, "ascii").await;
    assert_eq!(legacy.version(), 1);
    assert!(legacy.context_checkpoint().is_none());
    let session_id = legacy_agent.session_id().to_string();
    let legacy_encoded = encode_checkpoint(&legacy, INSTRUCTIONS, false, None).unwrap();
    SessionStorage::open(&config_path)
        .unwrap()
        .save_resume_state(&session_id, &legacy_encoded)
        .unwrap();
    legacy_agent.shutdown().await.unwrap();
    drop(legacy_events);

    let service = fixture.service.clone();
    let openai = OpenAi::builder("unused-local-fixture")
        .service(move || service.clone())
        .build()
        .unwrap();
    let tools = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectAndCodeMode)
        .tool(FixturePayload)
        .tool(ReadContextTool(Arc::clone(&fixture.backend)))
        .build()
        .unwrap();
    let instructions = format!("{INSTRUCTIONS}\n{}", super::INSTRUCTIONS);
    let (agent, events) = Nanocodex::builder(openai)
        .model(Model::Sol)
        .workspace(fixture.directory.path())
        .session_id(session_id.parse().unwrap())
        .instructions(instructions.clone())
        .tools(tools)
        .context_backend(fixture.backend.clone())
        .resume_with_context_upgrade(legacy.clone(), original_tools(), INSTRUCTIONS)
        .build()
        .unwrap();
    let initial = timeout(DEADLINE, agent.snapshot()).await.unwrap().unwrap();
    assert_eq!(initial.version(), 2);
    assert!(initial.context_checkpoint().is_some());
    let initial_encoded = encode_checkpoint(
        &initial,
        &instructions,
        false,
        Some(&fixture.backend.config),
    )
    .unwrap();
    SessionStorage::open(&config_path)
        .unwrap()
        .save_resume_state(&session_id, &initial_encoded)
        .unwrap();
    let database = Connection::open(database_path(&config_path)).unwrap();
    let backup: Vec<u8> = database
        .query_row(
            "SELECT state_zstd FROM context_resume_backups WHERE session_id = ?1",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(zstd::decode_all(backup.as_slice()).unwrap(), legacy_encoded);
    PublishedUpgrade {
        agent,
        events,
        session_id,
        legacy,
        legacy_encoded,
        instructions,
    }
}

#[tokio::test]
async fn context_checkpoint_recovery_restores_exact_legacy_bytes_and_retains_the_replacement() {
    let fixture = Fixture::new();
    let upgraded = publish_legacy_and_upgrade(&fixture).await;
    let config_path = fixture.directory.path().join("config.toml");
    seed(&upgraded.agent, 0, 8, "ascii").await;
    let compacted = compact(&upgraded.agent).await;
    assert!(!images(compacted.history()).is_empty());
    let replacement = encode_checkpoint(
        &compacted,
        &upgraded.instructions,
        false,
        Some(&fixture.backend.config),
    )
    .unwrap();
    SessionStorage::open(&config_path)
        .unwrap()
        .save_resume_state(&upgraded.session_id, &replacement)
        .unwrap();
    assert_eq!(
        SessionStorage::open(&config_path)
            .unwrap()
            .load_resume_state(&upgraded.session_id)
            .unwrap()
            .unwrap(),
        replacement
    );
    upgraded.agent.shutdown().await.unwrap();
    drop(upgraded.events);
    let database = Connection::open(database_path(&config_path)).unwrap();
    database
        .execute("UPDATE context_archive_pages SET png = X'00'", [])
        .unwrap();
    let generations_before = fixture.service.captures.lock().unwrap().len();
    assert!(
        fixture
            .build_with_instructions(Some(compacted), &upgraded.instructions)
            .is_err(),
        "normal archived restore must reject the corrupted page"
    );
    assert_eq!(
        fixture.service.captures.lock().unwrap().len(),
        generations_before
    );

    std::fs::write(&config_path, "[agent]\nweb_search = false\nimage_generation = false\n[skills]\nenabled = false\n[memory]\nenabled = false\n[subagents]\nenabled = false\n").unwrap();
    for _ in 0..2 {
        let cli = Cli::try_parse_from([
            "orvek",
            "--config",
            config_path.to_str().unwrap(),
            "--workspace",
            fixture.directory.path().to_str().unwrap(),
            "context",
            "restore-backup",
            &upgraded.session_id,
        ])
        .unwrap();
        timeout(DEADLINE, cli.run()).await.unwrap().unwrap();
        let restored = SessionStorage::open(&config_path)
            .unwrap()
            .load_resume_state(&upgraded.session_id)
            .unwrap()
            .unwrap();
        assert_eq!(restored, upgraded.legacy_encoded);
        let (backup, replaced): (Vec<u8>, Vec<u8>) = database.query_row(
            "SELECT state_zstd, replaced_state_zstd FROM context_resume_backups WHERE session_id = ?1",
            [&upgraded.session_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(
            zstd::decode_all(backup.as_slice()).unwrap(),
            upgraded.legacy_encoded
        );
        assert_eq!(zstd::decode_all(replaced.as_slice()).unwrap(), replacement);
        assert_eq!(
            fixture.service.captures.lock().unwrap().len(),
            generations_before,
            "recovery must not call the model"
        );
    }
    let recovered = load_checkpoint(&config_path, &upgraded.session_id).unwrap();
    assert!(recovered.compaction().is_none());
    let (snapshot, instructions, skills) = recovered.into_parts();
    assert_eq!(snapshot.version(), 1);
    assert_eq!(instructions, INSTRUCTIONS);
    assert_eq!(skills, Some(false));
    assert_eq!(items(snapshot.history()), items(upgraded.legacy.history()));
    let (legacy_agent, legacy_events) = build_legacy(&fixture, Some(snapshot));
    prompt(
        &legacy_agent,
        "continue from the recovered legacy checkpoint",
    )
    .await;
    let replay = fixture.last_request();
    assert!(replay.full_replay);
    assert!(replay.previous_response_id.is_none());
    assert_eq!(calls(&replay.input), calls(upgraded.legacy.history()));
    assert_eq!(outputs(&replay.input), outputs(upgraded.legacy.history()));
    legacy_agent.shutdown().await.unwrap();
    drop(legacy_events);
}

#[tokio::test]
async fn context_checkpoint_publication_rejects_missing_or_corrupt_manifests_without_replacement() {
    for missing in [true, false] {
        let fixture = Fixture::new();
        let upgraded = publish_legacy_and_upgrade(&fixture).await;
        let config_path = fixture.directory.path().join("config.toml");
        let previous = SessionStorage::open(&config_path)
            .unwrap()
            .load_resume_state(&upgraded.session_id)
            .unwrap()
            .unwrap();
        seed(&upgraded.agent, 0, 8, "ascii").await;
        let compacted = compact(&upgraded.agent).await;
        assert!(!images(compacted.history()).is_empty());
        let manifest = compacted
            .context_checkpoint()
            .unwrap()
            .manifest
            .as_ref()
            .unwrap();
        let candidate = encode_checkpoint(
            &compacted,
            &upgraded.instructions,
            false,
            Some(&fixture.backend.config),
        )
        .unwrap();
        let database = Connection::open(database_path(&config_path)).unwrap();
        if missing {
            database
                .execute(
                    "DELETE FROM context_archive_manifest_pages WHERE manifest_id = ?1",
                    [manifest],
                )
                .unwrap();
            database
                .execute(
                    "DELETE FROM context_archive_manifests WHERE id = ?1",
                    [manifest],
                )
                .unwrap();
        } else {
            database
                .execute(
                    "UPDATE context_archive_manifests SET encoded = X'00' WHERE id = ?1",
                    [manifest],
                )
                .unwrap();
        }
        let mut storage = SessionStorage::open(&config_path).unwrap();
        assert!(
            storage
                .save_resume_state(&upgraded.session_id, &candidate)
                .is_err(),
            "invalid manifest publication must fail before replacing the checkpoint"
        );
        assert_eq!(
            storage
                .load_resume_state(&upgraded.session_id)
                .unwrap()
                .unwrap(),
            previous
        );
        let (backup, replaced): (Vec<u8>, Option<Vec<u8>>) = database.query_row(
            "SELECT state_zstd, replaced_state_zstd FROM context_resume_backups WHERE session_id = ?1",
            [&upgraded.session_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(
            zstd::decode_all(backup.as_slice()).unwrap(),
            upgraded.legacy_encoded
        );
        assert!(replaced.is_none());
        upgraded.agent.shutdown().await.unwrap();
        drop(upgraded.events);
    }
}
