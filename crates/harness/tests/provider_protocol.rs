use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use orvek_harness::{
    Digest, Store,
    inference::{
        ArgumentValidity, AttemptStatus, CallOutcome, Delta, FailureKind, InferenceRequest, Limits,
        Model, ModelSettings, OutputItem, PromptInput, ReasoningMode, ResponseStatus,
        ResponsesClient, Route, Thinking, Transport, UsdCost,
        auth::{
            Auth, AuthError, AuthMode, ChatGptLogin, SecretString, chatgpt_auth_status,
            logout_chatgpt,
        },
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::{Instant, timeout},
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

struct Captured {
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct Reply {
    status: u16,
    mime: Option<&'static str>,
    body: Vec<u8>,
    fragment: bool,
    stall: Duration,
    header_stall: Duration,
    cost_usd: Option<&'static str>,
}
impl Reply {
    fn sse(events: Vec<Value>) -> Self {
        let body = events
            .into_iter()
            .map(|v| {
                format!(
                    "event: {}\r\ndata: {v}\r\n\r\n",
                    v["type"].as_str().unwrap()
                )
            })
            .collect::<String>()
            .into_bytes();
        Self {
            status: 200,
            mime: Some("text/event-stream; charset=utf-8"),
            body,
            fragment: false,
            stall: Duration::ZERO,
            header_stall: Duration::ZERO,
            cost_usd: None,
        }
    }
    fn reject(status: u16) -> Self {
        Self {
            status,
            mime: Some("application/json"),
            body: b"{\"error\":\"provider-controlled secret text\"}".to_vec(),
            fragment: false,
            stall: Duration::ZERO,
            header_stall: Duration::ZERO,
            cost_usd: None,
        }
    }
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            mime: Some("application/json"),
            body: value.to_string().into_bytes(),
            fragment: false,
            stall: Duration::ZERO,
            header_stall: Duration::ZERO,
            cost_usd: None,
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> Captured {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 32 * 1024);
    }
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.lines();
    let path = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_lowercase(), value.trim().to_owned()))
        .collect();
    let length = headers
        .get("content-length")
        .map(|n| n.parse().unwrap())
        .unwrap_or(0);
    assert!(length < 4 * 1024 * 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    Captured {
        path,
        headers,
        body,
    }
}

async fn server(replies: Vec<Reply>) -> (String, JoinHandle<Vec<Captured>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();
        for reply in replies {
            let (mut socket, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            captured.push(read_request(&mut socket).await);
            let cost_header = reply
                .cost_usd
                .map(|cost| format!("X-LiteLLM-Response-Cost: {cost}\r\n"))
                .unwrap_or_default();
            let content_type = reply
                .mime
                .map(|mime| format!("Content-Type: {mime}\r\n"))
                .unwrap_or_default();
            let header = format!(
                "HTTP/1.1 {} Fixture\r\n{content_type}Content-Length: {}\r\nRetry-After: 0\r\n{cost_header}Connection: close\r\n\r\n",
                reply.status,
                reply.body.len()
            );
            tokio::time::sleep(reply.header_stall).await;
            if socket.write_all(header.as_bytes()).await.is_err() {
                continue;
            }
            if !reply.stall.is_zero() {
                tokio::time::sleep(reply.stall).await;
            }
            for chunk in reply.body.chunks(if reply.fragment {
                3
            } else {
                reply.body.len().max(1)
            }) {
                if socket.write_all(chunk).await.is_err() {
                    break;
                }
                if reply.fragment {
                    tokio::task::yield_now().await;
                }
            }
        }
        captured
    });
    (base, task)
}

fn settings() -> ModelSettings {
    ModelSettings::default()
}
fn request_with(settings: ModelSettings, tools: Vec<Value>) -> InferenceRequest {
    InferenceRequest::new(
        settings,
        vec![json!({"role":"user","content":"perform the authorized task"})],
        tools,
        "Controller instructions".into(),
        "fixture-session".into(),
        1024,
    )
    .unwrap()
}
fn request() -> InferenceRequest {
    request_with(settings(), vec![])
}
fn limits() -> Limits {
    Limits {
        total_timeout: Duration::from_secs(3),
        idle_timeout: Duration::from_secs(1),
        connect_timeout: Duration::from_secs(1),
        retry_delay: Duration::ZERO,
        max_retry_delay: Duration::ZERO,
        ..Limits::default()
    }
}
fn client(base: &str, limits: Limits) -> ResponsesClient {
    let auth = Auth::api_key(SecretString::new("fixture-api-token".into())).unwrap();
    ResponsesClient::new(
        auth,
        Route::new(Transport::Http, &format!("{base}/responses")).unwrap(),
        limits,
    )
    .unwrap()
}
fn created() -> Value {
    json!({"type":"response.created","response":{"id":"response-fixture"}})
}
fn text_delta(text: &str) -> Value {
    json!({"type":"response.output_text.delta","item_id":"message-fixture","delta":text})
}
fn message(text: &str) -> Value {
    json!({"type":"message","id":"message-fixture","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]})
}
fn usage() -> Value {
    json!({"input_tokens":13,"output_tokens":7,"total_tokens":20,"input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":2}})
}
fn terminal(status: &str, output: Vec<Value>, usage: Value) -> Value {
    json!({"type":format!("response.{status}"),"response":{"id":"response-fixture","status":status,"output":output,"usage":usage}})
}
fn tool(arguments: &str) -> Value {
    json!({"type":"function_call","id":"item-call","call_id":"call-1","name":"read_file","arguments":arguments,"status":"completed"})
}
fn tool_schema() -> Value {
    json!({"type":"function","name":"read_file","description":"Read authorized source","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false},"strict":true})
}

// Persist through the same artifact store as parent reports and child response spans.
fn persisted_outcome(outcome: &CallOutcome) -> Value {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let artifacts = store.public_artifacts();
    let receipt = artifacts
        .write(&serde_json::to_vec(outcome).unwrap())
        .unwrap();
    let bytes = artifacts.resolve(receipt).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    for secret in [
        "fixture-api-token",
        "fixture-refresh-token",
        "fixture-rotated",
        "fixture-key",
        "fixture-account",
        "authorization",
    ] {
        assert!(
            !text.contains(secret),
            "provenance must not retain auth data"
        );
    }
    serde_json::from_slice(&bytes).unwrap()
}

fn assert_prepared_body(
    outcome: &CallOutcome,
    body: &[u8],
    transport: &str,
    dialect: &str,
    route: &str,
) {
    let persisted = persisted_outcome(outcome);
    let request = &persisted["request"];
    assert_eq!(request["status"], "prepared");
    assert_eq!(request["transport"], transport);
    assert_eq!(request["dialect"], dialect);
    assert_eq!(request["route"], route);
    assert_eq!(request["body"].as_str().unwrap().as_bytes(), body);
    assert!(outcome.attempts.iter().any(|attempt| attempt.dispatched));
}

#[tokio::test]
async fn input_token_count_uses_the_matching_responses_endpoint() {
    let (base, served) = server(vec![Reply::json(json!({"input_tokens": 123}))]).await;
    let input = vec![json!({"role":"user","content":"count this"})];

    let count = client(&base, limits())
        .count_input_tokens(settings(), &input)
        .await
        .unwrap();

    assert_eq!(count, 123);
    let captures = served.await.unwrap();
    assert_eq!(captures[0].path, "/responses/input_tokens");
    assert_eq!(
        captures[0].headers["authorization"],
        "Bearer fixture-api-token"
    );
    let wire: Value = serde_json::from_slice(&captures[0].body).unwrap();
    assert_eq!(wire, json!({"model":"gpt-5.6-sol","input":input}));
}

#[tokio::test]
async fn http_streaming_preserves_unicode_deltas_tools_usage_and_request_settings() {
    let proposed = tool("{\"path\":\"src/lib.rs\"}");
    let mut reply = Reply::sse(vec![
        created(),
        text_delta("héllo"),
        json!({"type":"response.function_call_arguments.delta","item_id":"item-call","delta":"{\"path\":"}),
        json!({"type":"response.output_item.done","output_index":1,"item":proposed}),
        terminal(
            "completed",
            vec![message("héllo"), proposed.clone()],
            usage(),
        ),
    ]);
    reply.fragment = true;
    reply.cost_usd = Some("0.000000250000000001");
    let (base, served) = server(vec![reply]).await;
    let mut deltas = Vec::new();
    let settings = ModelSettings {
        model: Model::Terra,
        thinking: Thinking::Max,
        reasoning_mode: ReasoningMode::Pro,
        fast_mode: true,
    };
    let request = request_with(settings, vec![tool_schema()])
        .with_prompt_cache_key("shared-prefix")
        .unwrap();
    let outcome = client(&base, limits())
        .respond(&request, &CancellationToken::new(), |delta| {
            deltas.push(delta);
        })
        .await;
    assert!(outcome.failure.is_none());
    assert!(!outcome.billing_uncertain());
    assert_eq!(
        outcome.accounted_cost().unwrap().to_string(),
        "$0.000000250000000001"
    );
    let response = outcome.response.unwrap();
    assert_eq!(response.status, ResponseStatus::Completed);
    assert_eq!(response.usage.input_tokens, Some(13));
    assert_eq!(response.usage.cached_input_tokens, Some(0));
    assert_eq!(
        response.usage.cost_usd.unwrap().to_string(),
        "$0.000000250000000001"
    );
    assert!(
        matches!(&response.output[1], OutputItem::ToolProposal(p) if p.validity == ArgumentValidity::JsonObject && p.arguments == "{\"path\":\"src/lib.rs\"}")
    );
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, Delta::Text{text,..} if text == "héllo"))
    );
    let captures = served.await.unwrap();
    let wire: Value = serde_json::from_slice(&captures[0].body).unwrap();
    assert_eq!(wire["model"], "gpt-5.6-terra");
    assert_eq!(
        wire["reasoning"],
        json!({"mode":"pro","effort":"max","context":"all_turns","summary":"auto"})
    );
    assert_eq!(wire["service_tier"], "priority");
    assert_eq!(wire["store"], false);
    assert_eq!(wire["stream"], true);
    assert_eq!(wire["tools"], json!([tool_schema()]));
    assert_eq!(wire["prompt_cache_key"], "shared-prefix");
    assert_eq!(captures[0].path, "/responses");
    assert_eq!(captures[0].headers["session-id"], "fixture-session");
    assert_eq!(captures[0].headers["thread-id"], "fixture-session");
    assert_eq!(
        captures[0].headers["authorization"],
        "Bearer fixture-api-token"
    );
    assert_eq!(
        captures[0].headers["x-openai-internal-codex-responses-lite"],
        "true"
    );
}

#[tokio::test]
async fn segmented_requests_keep_the_old_prefix_and_cache_lineage_stable() {
    let replies = vec![
        Reply::sse(vec![terminal("completed", vec![], usage())]),
        Reply::sse(vec![terminal("completed", vec![], usage())]),
    ];
    let (base, served) = server(replies).await;
    let stable = vec![json!({"role":"user","content":"settled history"})];
    let first_live = vec![json!({"role":"user","content":"current request"})];
    let mut second_live = first_live.clone();
    second_live.push(json!({"role":"assistant","content":"new live output"}));
    let segment = Digest::of_value(&stable).unwrap();
    let make_request = |live| {
        InferenceRequest::new_segmented(
            settings(),
            PromptInput::segmented(stable.clone(), live, vec![segment]).unwrap(),
            vec![tool_schema()],
            "Controller instructions".into(),
            "fixture-session".into(),
            1024,
        )
        .unwrap()
        .with_prompt_cache_key("shared-root")
        .unwrap()
    };
    let first = make_request(first_live);
    let second = make_request(second_live);
    assert_eq!(first.cache_lineage(), second.cache_lineage());
    assert_eq!(
        first.prompt_input().stable(),
        second.prompt_input().stable()
    );
    assert_ne!(first.prompt_input().live(), second.prompt_input().live());
    assert_eq!(first.cache_identity().stable_segments, vec![segment]);
    assert_eq!(
        first.cache_identity().instructions,
        second.cache_identity().instructions
    );

    let client = client(&base, limits());
    assert!(
        client
            .respond(&first, &CancellationToken::new(), |_| {})
            .await
            .failure
            .is_none()
    );
    assert!(
        client
            .respond(&second, &CancellationToken::new(), |_| {})
            .await
            .failure
            .is_none()
    );
    let captures = served.await.unwrap();
    let first_wire: Value = serde_json::from_slice(&captures[0].body).unwrap();
    let second_wire: Value = serde_json::from_slice(&captures[1].body).unwrap();
    let first_input = first_wire["input"].as_array().unwrap();
    let second_input = second_wire["input"].as_array().unwrap();
    assert_eq!(first_input, &second_input[..first_input.len()]);
    assert_eq!(
        serde_json::to_vec(&first_input[..stable.len()]).unwrap(),
        serde_json::to_vec(&second_input[..stable.len()]).unwrap()
    );

    let changed_controls = InferenceRequest::new_segmented(
        settings(),
        PromptInput::segmented(
            stable,
            vec![json!({"role":"user","content":"current request"})],
            vec![segment],
        )
        .unwrap(),
        vec![tool_schema()],
        "Changed controller instructions".into(),
        "fixture-session".into(),
        1024,
    )
    .unwrap()
    .with_prompt_cache_key("shared-root")
    .unwrap();
    assert_ne!(first.cache_lineage(), changed_controls.cache_lineage());
    assert_ne!(
        first.cache_identity().instructions,
        changed_controls.cache_identity().instructions
    );
    assert_eq!(
        first.cache_identity().stable_segments,
        changed_controls.cache_identity().stable_segments
    );
}

#[tokio::test]
async fn missing_gateway_cost_uses_the_fixed_model_catalog() {
    let reply = Reply::sse(vec![terminal("completed", vec![], usage())]);
    let (base, served) = server(vec![reply]).await;

    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;

    assert_eq!(outcome.accounted_cost().unwrap().to_string(), "$0.000192");
    assert_eq!(
        outcome
            .response
            .unwrap()
            .usage
            .cost_usd
            .unwrap()
            .to_string(),
        "$0.000192"
    );
    assert_eq!(served.await.unwrap().len(), 1);
}

#[tokio::test]
async fn conflicting_gateway_cost_receipts_are_unknown_instead_of_selected() {
    let mut terminal_usage = usage();
    terminal_usage["cost"] = json!("0.2");
    let mut reply = Reply::sse(vec![terminal("completed", vec![], terminal_usage)]);
    reply.cost_usd = Some("0.1");
    let (base, served) = server(vec![reply]).await;

    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;

    assert!(outcome.accounted_cost().is_none());
    assert_eq!(outcome.response.unwrap().usage.cost_usd, None);
    assert_eq!(served.await.unwrap().len(), 1);
}

#[tokio::test]
async fn every_existing_model_effort_mode_and_fast_setting_is_sent_without_substitution() {
    let cases: Vec<_> = [Model::Sol, Model::Terra, Model::Luna]
        .into_iter()
        .flat_map(|model| {
            [
                Thinking::Low,
                Thinking::Medium,
                Thinking::High,
                Thinking::Xhigh,
                Thinking::Max,
            ]
            .into_iter()
            .flat_map(move |thinking| {
                [ReasoningMode::Standard, ReasoningMode::Pro]
                    .into_iter()
                    .flat_map(move |reasoning_mode| {
                        [false, true].map(|fast_mode| ModelSettings {
                            model,
                            thinking,
                            reasoning_mode,
                            fast_mode,
                        })
                    })
            })
        })
        .collect();
    let replies = cases
        .iter()
        .map(|_| Reply::sse(vec![terminal("completed", vec![message("done")], usage())]))
        .collect();
    let (base, served) = server(replies).await;
    let client = client(&base, limits());
    for &case in &cases {
        assert!(
            client
                .respond(
                    &request_with(case, vec![]),
                    &CancellationToken::new(),
                    |_| {}
                )
                .await
                .failure
                .is_none()
        );
    }
    for (captured, case) in served.await.unwrap().into_iter().zip(cases) {
        let wire: Value = serde_json::from_slice(&captured.body).unwrap();
        assert_eq!(wire["model"], case.model.as_str());
        assert_eq!(wire["reasoning"]["effort"], case.thinking.as_str());
        assert_eq!(
            wire["reasoning"].get("mode").and_then(Value::as_str),
            if case.reasoning_mode == ReasoningMode::Pro {
                Some("pro")
            } else {
                None
            }
        );
        assert_eq!(
            wire.get("service_tier").and_then(Value::as_str),
            if case.fast_mode {
                Some("priority")
            } else {
                None
            }
        );
    }
}

#[tokio::test]
async fn malformed_tool_arguments_remain_exact_untrusted_proposals() {
    for (arguments, validity) in [
        ("{\"path\":", ArgumentValidity::MalformedJson),
        ("[]", ArgumentValidity::NotObject),
    ] {
        let (base, served) = server(vec![Reply::sse(vec![terminal(
            "completed",
            vec![tool(arguments)],
            usage(),
        )])])
        .await;
        let outcome = client(&base, limits())
            .respond(
                &request_with(settings(), vec![tool_schema()]),
                &CancellationToken::new(),
                |_| {},
            )
            .await;
        let OutputItem::ToolProposal(proposal) = &outcome.response.unwrap().output[0] else {
            panic!("missing tool proposal");
        };
        assert_eq!(proposal.arguments, arguments);
        assert_eq!(proposal.validity, validity);
        served.await.unwrap();
    }
}

#[tokio::test]
async fn failed_and_incomplete_responses_keep_their_status_and_unknown_usage() {
    for (status, expected) in [
        ("failed", ResponseStatus::Failed),
        ("incomplete", ResponseStatus::Incomplete),
    ] {
        let (base, served) = server(vec![Reply::sse(vec![
            created(),
            terminal(status, vec![message("partial")], Value::Null),
        ])])
        .await;
        let outcome = client(&base, limits())
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert!(outcome.failure.is_none());
        assert_eq!(outcome.response.as_ref().unwrap().status, expected);
        assert_eq!(outcome.response.as_ref().unwrap().usage.input_tokens, None);
        assert!(outcome.billing_uncertain());
        served.await.unwrap();
    }
}

#[tokio::test]
async fn closed_stream_keeps_partial_output_and_does_not_retry_a_possibly_billed_call() {
    let (base, served) = server(vec![Reply::sse(vec![created(), text_delta("partial")])]).await;
    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(outcome.failure.unwrap().kind, FailureKind::Interrupted);
    assert_eq!(outcome.partial_text, "partial");
    assert_eq!(outcome.attempts.len(), 1);
    assert!(outcome.response.is_none());
    assert!(outcome.attempts[0].billing_uncertain);
    served.await.unwrap();
}

#[tokio::test]
async fn rate_limit_retries_remain_visible_after_success() {
    let (base, served) = server(vec![
        Reply::reject(429),
        Reply::reject(429),
        Reply::sse(vec![terminal("completed", vec![message("done")], usage())]),
    ])
    .await;
    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert!(outcome.failure.is_none());
    assert_eq!(outcome.attempts.len(), 3);
    assert!(!outcome.billing_uncertain());
    assert!(!outcome.rate_limited());
    let captures = served.await.unwrap();
    assert_eq!(captures.len(), 3);
    for (index, capture) in captures.iter().enumerate() {
        assert_prepared_body(&outcome, &capture.body, "http", "open_ai", "default");
        assert_eq!(outcome.attempts[index].number, index as u32 + 1);
    }
}

#[tokio::test]
async fn ambiguous_server_rejections_are_not_retried() {
    for status in [408, 502, 503, 504] {
        let (base, served) = server(vec![Reply::reject(status)]).await;
        let outcome = client(&base, limits())
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert_eq!(outcome.attempts.len(), 1);
        assert!(outcome.billing_uncertain());
        assert!(outcome.accounted_cost().is_none());
        assert!(!outcome.rate_limited());
        assert_eq!(served.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn single_admitted_rate_limit_rejection_is_safe_to_readmit() {
    let (base, served) = server(vec![Reply::reject(429)]).await;
    let outcome = client(
        &base,
        Limits {
            max_attempts: 1,
            ..limits()
        },
    )
    .respond(&request(), &CancellationToken::new(), |_| {})
    .await;
    assert_eq!(outcome.attempts.len(), 1);
    assert!(outcome.rate_limited());
    assert!(!outcome.billing_uncertain());
    assert_eq!(outcome.accounted_cost(), Some(UsdCost::ZERO));
    assert_eq!(served.await.unwrap().len(), 1);
    let mut ambiguous = outcome.clone();
    ambiguous.attempts[0].billing_uncertain = true;
    assert!(!ambiguous.rate_limited());
    let mut partial = outcome;
    partial.partial_text = "provisional output".into();
    assert!(!partial.rate_limited());
}

#[tokio::test]
async fn exhausted_retries_and_permanent_rejections_are_bounded_and_redacted() {
    for (code, attempts) in [(429, 3), (503, 1), (400, 1), (401, 2), (307, 1)] {
        let (base, served) = server((0..attempts).map(|_| Reply::reject(code)).collect()).await;
        let outcome = client(&base, limits())
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert_eq!(outcome.attempts.len(), attempts);
        assert!(outcome.failure.is_some());
        assert!(!format!("{outcome:?}").contains("provider-controlled secret text"));
        let captures = served.await.unwrap();
        assert_eq!(captures.len(), attempts);
        for capture in captures {
            assert_prepared_body(&outcome, &capture.body, "http", "open_ai", "default");
        }
    }
}

#[tokio::test]
async fn malformed_events_and_inconsistent_terminal_ids_do_not_finish() {
    let variants = vec![
        vec![
            created(),
            json!({"type":"response.completed","response":{"id":"other","status":"completed","output":[],"usage":usage()}}),
        ],
        // A terminal event whose status names a different outcome now decodes
        // as that outcome; bridges rely on it. It must not be treated as
        // malformed.
        vec![terminal("completed", vec![tool("{}"), tool("{}")], usage())],
        vec![json!({"type":"error","error":{"message":"unsafe provider detail"}})],
        vec![
            json!({"type":"response.output_item.done","output_index":0,"item":tool("{}")}),
            terminal("completed", vec![tool("{\"path\":\"changed\"}")], usage()),
        ],
        vec![
            json!({"type":"response.completed","response":{"id":"response-fixture","status":"completed","output":[],"error":{"code":"failure"},"usage":usage()}}),
        ],
    ];
    for events in variants {
        let (base, served) = server(vec![Reply::sse(events)]).await;
        let outcome = client(&base, limits())
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert!(outcome.failure.is_some());
        assert!(outcome.response.is_none());
        assert_eq!(outcome.attempts.len(), 1);
        served.await.unwrap();
    }
    let mut reply = Reply::sse(vec![]);
    reply.body = b"data: {broken\n\n".to_vec();
    let (base, served) = server(vec![reply]).await;
    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(
        outcome.failure.unwrap().kind,
        FailureKind::MalformedResponse
    );
    served.await.unwrap();
}

#[tokio::test]
async fn byte_limits_and_usage_inconsistencies_preserve_uncertainty() {
    let (base, served) = server(vec![Reply::sse(vec![text_delta(&"x".repeat(500))])]).await;
    let outcome = client(
        &base,
        Limits {
            max_event_bytes: 128,
            ..limits()
        },
    )
    .respond(&request(), &CancellationToken::new(), |_| {})
    .await;
    assert_eq!(outcome.failure.unwrap().kind, FailureKind::SizeLimit);
    served.await.unwrap();
    let (base, served) = server(vec![Reply::sse(vec![terminal(
        "completed",
        vec![],
        json!({"input_tokens":13,"output_tokens":7,"total_tokens":1}),
    )])])
    .await;
    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert!(outcome.billing_uncertain());
    assert_eq!(outcome.response.unwrap().usage.total_tokens, None);
    served.await.unwrap();
}

#[tokio::test]
async fn cancellation_retains_partial_text_and_never_reports_provider_completion() {
    let (base, served) = server(vec![Reply::sse(vec![created(), text_delta("stop here")])]).await;
    let cancel = CancellationToken::new();
    let outcome = client(&base, limits())
        .respond(&request(), &cancel, |delta| {
            if matches!(delta, Delta::Text { .. }) {
                cancel.cancel();
            }
        })
        .await;
    assert_eq!(outcome.failure.unwrap().kind, FailureKind::Cancelled);
    assert_eq!(outcome.partial_text, "stop here");
    assert_eq!(outcome.attempts[0].status, AttemptStatus::Cancelled);
    assert!(outcome.attempts[0].billing_uncertain);
    served.await.unwrap();
}

#[tokio::test]
async fn cancellation_before_dispatch_has_no_attempts_or_spend() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcome = client(&base, limits())
        .respond(&request(), &cancel, |_| {})
        .await;
    assert_eq!(
        outcome.failure.as_ref().unwrap().kind,
        FailureKind::Cancelled
    );
    assert!(outcome.attempts.is_empty());
    assert!(!outcome.billing_uncertain());
    assert_eq!(
        persisted_outcome(&outcome)["request"],
        json!({"status":"unavailable"})
    );
}

#[tokio::test]
async fn response_headers_use_idle_budget_after_connection() {
    for (idle, total, succeeds) in [
        (Duration::from_secs(1), Duration::from_secs(3), true),
        (Duration::from_millis(40), Duration::from_secs(3), false),
        (Duration::from_secs(1), Duration::from_millis(150), false),
    ] {
        let mut reply = Reply::sse(vec![
            created(),
            terminal("completed", vec![message("complete")], usage()),
        ]);
        reply.header_stall = Duration::from_millis(200);
        let (base, served) = server(vec![reply]).await;
        let outcome = client(
            &base,
            Limits {
                connect_timeout: Duration::from_millis(100),
                idle_timeout: idle,
                total_timeout: total,
                ..limits()
            },
        )
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
        assert_eq!(served.await.unwrap().len(), 1);
        assert_eq!(outcome.attempts.len(), 1);
        if succeeds {
            assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
            assert!(outcome.response.is_some());
        } else {
            assert_eq!(outcome.failure.unwrap().kind, FailureKind::Timeout);
            assert!(outcome.attempts[0].billing_uncertain);
            assert!(outcome.response.is_none());
        }
    }
}

#[tokio::test]
async fn idle_timeout_is_bounded_and_does_not_retry() {
    let mut reply = Reply::sse(vec![terminal("completed", vec![], usage())]);
    reply.stall = Duration::from_millis(200);
    let (base, served) = server(vec![reply]).await;
    let started = Instant::now();
    let outcome = client(
        &base,
        Limits {
            idle_timeout: Duration::from_millis(30),
            ..limits()
        },
    )
    .respond(&request(), &CancellationToken::new(), |_| {})
    .await;
    assert!(started.elapsed() < Duration::from_millis(150));
    assert_eq!(outcome.failure.unwrap().kind, FailureKind::Timeout);
    assert_eq!(outcome.attempts.len(), 1);
    assert!(outcome.attempts[0].billing_uncertain);
    served.await.unwrap();
}

#[tokio::test]
#[allow(clippy::result_large_err)] // The tungstenite callback fixes this error type.
async fn websocket_transport_uses_auth_and_normalizes_the_actual_event_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/responses", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(socket, |request: &Request, response: Response| {
            assert_eq!(
                request.headers()["authorization"],
                "Bearer fixture-api-token"
            );
            assert_eq!(
                request.headers()["openai-beta"],
                "responses_websockets=2026-02-06"
            );
            Ok(response)
        })
        .await
        .unwrap();
        let Message::Text(request) = socket.next().await.unwrap().unwrap() else {
            panic!("request must be text");
        };
        let request = request.as_bytes().to_vec();
        socket.send(Message::Ping(vec![1, 2].into())).await.unwrap();
        for event in [
            created(),
            text_delta("ws"),
            terminal("completed", vec![message("ws"), tool("{}")], usage()),
        ] {
            socket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        request
    });
    let auth = Auth::api_key(SecretString::new("fixture-api-token".into())).unwrap();
    let client = ResponsesClient::new(
        auth,
        Route::new(Transport::WebSocket, &endpoint).unwrap(),
        limits(),
    )
    .unwrap();
    let outcome = client
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert!(outcome.failure.is_none());
    assert_eq!(
        outcome.response.as_ref().unwrap().status,
        ResponseStatus::Completed
    );
    let body = served.await.unwrap();
    assert_prepared_body(&outcome, &body, "web_socket", "open_ai", "default");
    let wire: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(wire["type"], "response.create");
    assert!(wire.get("stream").is_none());
    assert_eq!(wire["model"], "gpt-5.6-sol");
    assert_eq!(wire["max_output_tokens"], 1024);
    assert_eq!(wire["truncation"], "disabled");
}

#[test]
fn endpoint_and_input_validation_are_explicit_and_nonleaking() {
    for endpoint in [
        "http://example.com/v1/responses",
        "https://user:secret@example.com/v1/responses",
        "https://example.com/responses?token=secret",
        "ftp://example.com/responses",
    ] {
        let error = Route::new(Transport::Http, endpoint).unwrap_err();
        assert_eq!(error, FailureKind::InvalidEndpoint);
        assert!(!error.to_string().contains("secret"));
    }
    assert!(
        InferenceRequest::new(
            settings(),
            vec![json!({"role":"user","content":"test"})],
            vec![json!({"type":"web_search"})],
            String::new(),
            "id".into(),
            100
        )
        .is_err()
    );
    assert!(
        InferenceRequest::new(
            settings(),
            vec![json!({"type":"invented","content":"test"})],
            vec![],
            String::new(),
            "id".into(),
            100
        )
        .is_err()
    );
    assert!(
        request()
            .with_prompt_cache_key("cache key containing spaces")
            .is_err()
    );
    assert!("gpt-other".parse::<Model>().is_err());
}

fn jwt(claims: Value) -> String {
    format!(
        "fixture.{}.unsigned",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}
fn credentials(account: &str, expired: bool) -> Value {
    json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,"preserve":{"extra":"setting"},"tokens":{
        "id_token":jwt(json!({"email":"fixture@example.invalid","https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_plan_type":"fixture-plan","chatgpt_account_is_fedramp":true}})),
        "access_token":jwt(json!({"exp":if expired {1u64}else{9_999_999_999u64}})),
        "refresh_token":"fixture-refresh-token","account_id":account,"extra_token_metadata":"preserved"
    }})
}

#[test]
fn credential_status_selection_logout_and_secret_redaction_use_only_temp_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
    let auth = Auth::load(AuthMode::Auto, &path, || panic!("must not read API key")).unwrap();
    assert_eq!(auth.mode(), AuthMode::ChatGpt);
    assert!(!format!("{auth:?}").contains("fixture-refresh-token"));
    let status = chatgpt_auth_status(&path).unwrap();
    assert_eq!(status.account_id, "fixture-account");
    assert_eq!(status.plan.as_deref(), Some("fixture-plan"));
    assert!(status.fedramp);
    fs::write(&path, "invalid document containing fixture-refresh-token").unwrap();
    let result = Auth::load(AuthMode::Auto, &path, || {
        panic!("present invalid file must not switch billing")
    });
    assert!(result.is_err());
    assert!(!format!("{result:?}").contains("fixture-refresh-token"));
    assert!(logout_chatgpt(&path).unwrap());
    assert!(!logout_chatgpt(&path).unwrap());
    assert_eq!(
        Auth::load(AuthMode::Auto, &path, || Ok(Some(SecretString::new(
            "fixture-key".into()
        ))))
        .unwrap()
        .mode(),
        AuthMode::ApiKey
    );
    let mut secret = SecretString::new("fixture-owned-secret".into());
    assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");
    secret.zeroize();
    assert!(secret.expose_secret().is_empty());
}

#[tokio::test]
async fn unauthorized_api_key_request_retries_when_rejection_is_pre_generation() {
    let (base, served) = server(vec![
        Reply::reject(401),
        Reply::sse(vec![terminal("completed", vec![], usage())]),
    ])
    .await;
    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;

    assert!(outcome.failure.is_none());
    assert_eq!(outcome.attempts.len(), 2);
    assert!(
        outcome
            .attempts
            .iter()
            .all(|attempt| !attempt.billing_uncertain)
    );
    assert_eq!(served.await.unwrap().len(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn unauthorized_command_credential_refreshes_and_retries_once() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().unwrap();
    let helper = temporary.path().join("credential-helper");
    fs::write(
        &helper,
        r#"#!/bin/sh
count_file="$(dirname "$0")/count"
count=$(cat "$count_file" 2>/dev/null || echo 0)
count=$((count + 1))
printf '%s' "$count" >"$count_file"
if [ "$count" -eq 1 ]; then printf fixture-expired; else printf fixture-fresh; fi
"#,
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

    let (base, served) = server(vec![
        Reply::reject(401),
        Reply::sse(vec![terminal("completed", vec![], usage())]),
    ])
    .await;
    let auth =
        Auth::api_key_command(helper, Duration::from_secs(300), Duration::from_secs(5)).unwrap();
    let route = Route::from_overrides(&auth, Transport::Http, Some(&base), None).unwrap();
    let outcome = ResponsesClient::new(auth, route, limits())
        .unwrap()
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;

    assert!(outcome.failure.is_none());
    assert_eq!(outcome.attempts.len(), 2);
    let captures = served.await.unwrap();
    assert_eq!(
        captures[0].headers["authorization"],
        "Bearer fixture-expired"
    );
    assert_eq!(captures[1].headers["authorization"], "Bearer fixture-fresh");
    assert_eq!(
        fs::read_to_string(temporary.path().join("count")).unwrap(),
        "2"
    );
}

#[tokio::test]
async fn command_credential_validation_rejects_a_missing_helper() {
    let auth = Auth::api_key_command(
        std::path::PathBuf::from("/definitely/missing/orvek-credential-helper"),
        Duration::from_secs(300),
        Duration::from_secs(1),
    )
    .unwrap();

    assert_eq!(auth.validate().await.unwrap_err(), AuthError::Transport);
}

#[tokio::test]
async fn chatgpt_http_uses_effective_auth_to_omit_unsupported_parameters() {
    for (default_chatgpt, chatgpt) in [(true, true), (false, false), (true, false), (false, true)] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("auth.json");
        fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
        let auth = |chatgpt| {
            if chatgpt {
                Auth::chatgpt(path.clone()).unwrap()
            } else {
                Auth::api_key(SecretString::new("fixture-api-token".into())).unwrap()
            }
        };
        let (base, served) = server(vec![Reply::sse(vec![terminal(
            "completed",
            vec![message("ok")],
            usage(),
        )])])
        .await;
        let route = Route::new(Transport::Http, &format!("{base}/responses")).unwrap();
        let provider =
            ResponsesClient::new(auth(default_chatgpt), route.clone(), limits()).unwrap();
        let provider = if default_chatgpt == chatgpt {
            provider
        } else {
            provider.with_model_route(Model::Sol, auth(chatgpt), route)
        };
        let outcome = provider
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert!(outcome.failure.is_none());
        let captures = served.await.unwrap();
        assert_prepared_body(
            &outcome,
            &captures[0].body,
            "http",
            if chatgpt { "chat_gpt" } else { "open_ai" },
            if default_chatgpt == chatgpt {
                "default"
            } else {
                "model_override"
            },
        );
        let wire: Value = serde_json::from_slice(&captures[0].body).unwrap();
        if chatgpt {
            assert!(
                wire.get("max_output_tokens").is_none(),
                "ChatGPT rejects max_output_tokens"
            );
            assert!(
                wire.get("truncation").is_none(),
                "ChatGPT rejects truncation"
            );
            assert_eq!(captures[0].headers["chatgpt-account-id"], "fixture-account");
        } else {
            assert_eq!(wire["max_output_tokens"], 1024);
            assert_eq!(wire["truncation"], "disabled");
        }
        assert_eq!(wire["store"], false);
        assert_eq!(wire["stream"], true);
        assert_eq!(
            wire["input"],
            json!([{"role":"user","content":"perform the authorized task"}])
        );
    }
}

#[tokio::test]
async fn chatgpt_websocket_omits_unsupported_parameters() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
    let auth = Auth::chatgpt(path).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/responses", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        let Message::Text(request) = socket.next().await.unwrap().unwrap() else {
            panic!("request must be text");
        };
        socket
            .send(Message::Text(
                json!({"type":"response.output_item.done","output_index":0,"item":message("ok")})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                terminal("completed", vec![], usage()).to_string().into(),
            ))
            .await
            .unwrap();
        request.as_bytes().to_vec()
    });
    // The effective model route must override both the default auth and transport.
    let provider = client("http://127.0.0.1:1", limits()).with_model_route(
        Model::Sol,
        auth,
        Route::new(Transport::WebSocket, &endpoint).unwrap(),
    );
    let outcome = provider
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert!(outcome.failure.is_none());
    assert!(
        matches!(&outcome.response.as_ref().unwrap().output[0], OutputItem::Message { text, .. } if text == "ok")
    );
    let body = served.await.unwrap();
    assert_prepared_body(&outcome, &body, "web_socket", "chat_gpt", "model_override");
    let wire: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(wire["type"], "response.create");
    assert!(wire.get("stream").is_none());
    assert!(wire.get("max_output_tokens").is_none());
    assert!(wire.get("truncation").is_none());
    assert_eq!(wire["store"], false);
    assert_eq!(wire["model"], "gpt-5.6-sol");
}

#[tokio::test]
async fn only_chatgpt_accepts_completed_stream_items_when_terminal_output_is_empty() {
    for chatgpt in [true, false] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("auth.json");
        fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
        let auth = if chatgpt {
            Auth::chatgpt(path).unwrap()
        } else {
            Auth::api_key(SecretString::new("fixture-api-token".into())).unwrap()
        };
        let items = vec![message("ok"), tool("{\"path\":\"file.rs\"}")];
        let (base, served) = server(vec![Reply::sse(vec![
            created(),
            json!({"type":"response.output_item.done","output_index":0,"item":items[0]}),
            json!({"type":"response.output_item.done","output_index":1,"item":items[1]}),
            terminal("completed", vec![], usage()),
        ])])
        .await;
        let route = Route::new(Transport::Http, &format!("{base}/responses")).unwrap();
        let outcome = ResponsesClient::new(auth, route, limits())
            .unwrap()
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        if chatgpt {
            assert!(outcome.failure.is_none(), "{outcome:?}");
            let response = outcome.response.unwrap();
            assert_eq!(response.status, ResponseStatus::Completed);
            assert_eq!(response.history_items, items);
            assert!(
                matches!(&response.output[0], OutputItem::Message { text, .. } if text == "ok")
            );
            assert!(
                matches!(&response.output[1], OutputItem::ToolProposal(proposal) if proposal.name == "read_file")
            );
        } else {
            assert_eq!(
                outcome.failure.unwrap().kind,
                FailureKind::MalformedResponse
            );
        }
        served.await.unwrap();
    }
}

#[tokio::test]
async fn chatgpt_stream_items_do_not_bypass_terminal_confirmation_checks() {
    let cases = [
        vec![
            created(),
            json!({"type":"response.output_item.done","output_index":1,"item":message("gap")}),
            terminal("completed", vec![], usage()),
        ],
        vec![
            created(),
            json!({"type":"response.output_item.done","output_index":0,"item":message("before")}),
            terminal("completed", vec![message("conflict")], usage()),
        ],
        vec![
            created(),
            json!({"type":"response.output_item.done","output_index":0,"item":message("unfinished")}),
        ],
        vec![
            created(),
            json!({"type":"response.output_item.done","output_index":0,"item":tool("{}")}),
            json!({"type":"response.output_item.done","output_index":1,"item":tool("{}")}),
            terminal("completed", vec![], usage()),
        ],
    ];
    for events in cases {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("auth.json");
        fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
        let auth = Auth::chatgpt(path).unwrap();
        let (base, served) = server(vec![Reply::sse(events)]).await;
        let route = Route::new(Transport::Http, &format!("{base}/responses")).unwrap();
        let outcome = ResponsesClient::new(auth, route, limits())
            .unwrap()
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert!(outcome.failure.is_some());
        assert!(outcome.response.is_none());
        served.await.unwrap();
    }
}

#[tokio::test]
async fn only_chatgpt_accepts_a_valid_event_stream_without_content_type() {
    for (chatgpt, mime, valid_stream, accepted) in [
        (true, None, true, true),
        (false, None, true, false),
        (true, Some("text/html"), true, false),
        (true, None, false, false),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("auth.json");
        fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
        let auth = if chatgpt {
            Auth::chatgpt(path).unwrap()
        } else {
            Auth::api_key(SecretString::new("fixture-api-token".into())).unwrap()
        };
        let mut reply = Reply::sse(vec![
            created(),
            terminal("completed", vec![message("ok")], usage()),
        ]);
        reply.mime = mime;
        if !valid_stream {
            reply.body = b"<html>not a model response</html>".to_vec();
        }
        let (base, served) = server(vec![reply]).await;
        let provider = ResponsesClient::new(
            auth,
            Route::new(Transport::Http, &format!("{base}/responses")).unwrap(),
            limits(),
        )
        .unwrap();
        let outcome = provider
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert_eq!(outcome.failure.is_none(), accepted, "{outcome:?}");
        assert_eq!(outcome.response.is_some(), accepted);
        served.await.unwrap();
    }
}

#[tokio::test]
async fn unauthorized_chatgpt_refresh_preserves_shared_file_fields_and_retries_once() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
    let new_access = jwt(json!({"exp":9_999_999_999u64,"generation":2}));
    let (base, served) = server(vec![
        Reply::reject(401),
        Reply::json(json!({"access_token":new_access,"refresh_token":"fixture-rotated"})),
        Reply::sse(vec![terminal("completed", vec![], usage())]),
    ])
    .await;
    let auth = Auth::chatgpt_with_issuer(path.clone(), &base).unwrap();
    let route = Route::from_overrides(&auth, Transport::Http, Some(&base), None).unwrap();
    let outcome = ResponsesClient::new(auth, route, limits())
        .unwrap()
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert!(outcome.failure.is_none());
    assert_eq!(outcome.attempts.len(), 2);
    let captures = served.await.unwrap();
    for index in [0, 2] {
        assert_prepared_body(
            &outcome,
            &captures[index].body,
            "http",
            "chat_gpt",
            "default",
        );
    }
    assert!(
        !persisted_outcome(&outcome)
            .to_string()
            .contains(&new_access)
    );
    assert_eq!(
        captures.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        ["/responses", "/oauth/token", "/responses"]
    );
    assert_eq!(captures[0].headers["chatgpt-account-id"], "fixture-account");
    assert_eq!(captures[0].headers["x-openai-fedramp"], "true");
    assert_eq!(
        captures[2].headers["authorization"],
        format!("Bearer {new_access}")
    );
    let refresh: Value = serde_json::from_slice(&captures[1].body).unwrap();
    assert_eq!(refresh["grant_type"], "refresh_token");
    let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["tokens"]["refresh_token"], "fixture-rotated");
    assert_eq!(persisted["tokens"]["extra_token_metadata"], "preserved");
    assert_eq!(persisted["preserve"], json!({"extra":"setting"}));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn concurrent_clients_refresh_rotating_credentials_only_once() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    fs::write(&path, credentials("fixture-account", true).to_string()).unwrap();
    let (base,served) = server(vec![Reply::json(json!({"access_token":jwt(json!({"exp":9_999_999_999u64})),"refresh_token":"fixture-rotated"})),Reply::sse(vec![terminal("completed",vec![],usage())]),Reply::sse(vec![terminal("completed",vec![],usage())])]).await;
    let make_client = || {
        let auth = Auth::chatgpt_with_issuer(path.clone(), &base).unwrap();
        let route = Route::from_overrides(&auth, Transport::Http, Some(&base), None).unwrap();
        ResponsesClient::new(auth, route, limits()).unwrap()
    };
    let first = make_client();
    let second = make_client();
    let request = request();
    let cancel = CancellationToken::new();
    let (first, second) = tokio::join!(
        first.respond(&request, &cancel, |_| {}),
        second.respond(&request, &cancel, |_| {})
    );
    assert!(first.failure.is_none());
    assert!(second.failure.is_none());
    let captures = served.await.unwrap();
    assert_eq!(
        captures.iter().filter(|r| r.path == "/oauth/token").count(),
        1
    );
}

#[tokio::test]
async fn cancelled_refresh_cannot_reuse_a_potentially_rotated_token() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    fs::write(&path, credentials("fixture-account", true).to_string()).unwrap();
    let mut reply = Reply::json(
        json!({"access_token":jwt(json!({"exp":9_999_999_999u64})),"refresh_token":"fixture-rotated"}),
    );
    reply.stall = Duration::from_millis(200);
    let (base, served) = server(vec![reply]).await;
    let auth = Auth::chatgpt_with_issuer(path, &base).unwrap();
    let route = Route::from_overrides(&auth, Transport::Http, Some(&base), None).unwrap();
    let client = ResponsesClient::new(
        auth,
        route,
        Limits {
            total_timeout: Duration::from_millis(40),
            ..limits()
        },
    )
    .unwrap();
    let first = client
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(first.failure.unwrap().kind, FailureKind::Timeout);
    let second = client
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert_eq!(second.failure.unwrap().kind, FailureKind::Authentication);
    assert!(second.attempts.is_empty());
    assert_eq!(served.await.unwrap().len(), 1);
}

#[tokio::test]
async fn logout_and_account_switch_are_observed_before_the_next_request() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    for logout in [false, true] {
        fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
        let auth = Auth::chatgpt(path.clone()).unwrap();
        let route = Route::new(Transport::Http, "http://127.0.0.1:1/responses").unwrap();
        let client = ResponsesClient::new(auth, route, limits()).unwrap();
        if logout {
            logout_chatgpt(&path).unwrap();
        } else {
            fs::write(&path, credentials("another-account", false).to_string()).unwrap();
        }
        let outcome = client
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert_eq!(
            outcome.failure.as_ref().unwrap().kind,
            FailureKind::Authentication
        );
        let persisted = persisted_outcome(&outcome);
        assert_eq!(persisted["request"]["status"], "prepared");
        assert_eq!(persisted["request"]["dialect"], "chat_gpt");
        assert!(persisted["request"]["body"].is_string());
        assert!(outcome.attempts.is_empty());
    }
}

#[tokio::test]
async fn browser_login_exchanges_pkce_code_and_writes_a_shared_credential_file() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    let initial = credentials("fixture-account", false);
    let (base, served) = server(vec![Reply::json(initial["tokens"].clone())]).await;
    let login = ChatGptLogin::start_with_issuer(path.clone(), &base, &[0])
        .await
        .unwrap();
    assert_eq!(format!("{login:?}"), "ChatGptLogin([REDACTED])");
    let url = reqwest::Url::parse(login.authorization_url()).unwrap();
    let query: BTreeMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(query["code_challenge_method"], "S256");
    let callback = format!(
        "{}?state={}&code=fixture-code",
        query["redirect_uri"], query["state"]
    );
    let callback_task = tokio::spawn(async move { reqwest::get(callback).await.unwrap().status() });
    let status = login.complete().await.unwrap();
    assert_eq!(status.account_id, "fixture-account");
    assert_eq!(callback_task.await.unwrap(), 200);
    assert_eq!(chatgpt_auth_status(&path).unwrap(), status);
    let captured = served.await.unwrap();
    let form: BTreeMap<_, _> = url::form_urlencoded::parse(&captured[0].body)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(form["code"], "fixture-code");
    assert_eq!(form["grant_type"], "authorization_code");
    use sha2::Digest;
    assert_eq!(
        URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(form["code_verifier"].as_bytes())),
        query["code_challenge"]
    );
}

#[tokio::test]
async fn cancelled_login_never_writes_credentials() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    let login = ChatGptLogin::start_with_issuer(path.clone(), "http://127.0.0.1:1", &[0])
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        login.complete_with_cancellation(&cancel).await.unwrap_err(),
        AuthError::Cancelled
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn websocket_disconnect_and_cancellation_keep_usage_unknown() {
    for should_cancel in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/responses", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let Message::Text(body) = socket.next().await.unwrap().unwrap() else {
                panic!("request must be text");
            };
            for event in [created(), text_delta("partial ws")] {
                socket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .unwrap();
            }
            socket.close(None).await.unwrap();
            body.as_bytes().to_vec()
        });
        let auth = Auth::api_key(SecretString::new("fixture-key".into())).unwrap();
        let client = ResponsesClient::new(
            auth,
            Route::new(Transport::WebSocket, &endpoint).unwrap(),
            limits(),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let outcome = client
            .respond(&request(), &cancel, |delta| {
                if should_cancel && matches!(delta, Delta::Text { .. }) {
                    cancel.cancel();
                }
            })
            .await;
        assert_eq!(outcome.partial_text, "partial ws");
        assert!(outcome.response.is_none());
        assert!(outcome.billing_uncertain());
        assert_eq!(outcome.attempts.len(), 1);
        assert_eq!(
            outcome.failure.as_ref().unwrap().kind,
            if should_cancel {
                FailureKind::Cancelled
            } else {
                FailureKind::Interrupted
            }
        );
        let body = served.await.unwrap();
        assert_prepared_body(&outcome, &body, "web_socket", "open_ai", "default");
    }
}

#[tokio::test]
async fn a_refresh_returning_another_account_never_overwrites_the_shared_file() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    let original = credentials("fixture-account", true).to_string();
    fs::write(&path, &original).unwrap();
    let changed = credentials("another-account", false);
    let (base, served) = server(vec![Reply::json(changed["tokens"].clone())]).await;
    let auth = Auth::chatgpt_with_issuer(path.clone(), &base).unwrap();
    let route = Route::from_overrides(&auth, Transport::Http, Some(&base), None).unwrap();
    let client = ResponsesClient::new(auth, route, limits()).unwrap();
    let outcome = client
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    let failure = outcome.failure.unwrap();
    assert_eq!(failure.kind, FailureKind::Authentication);
    assert_eq!(failure.auth_error, Some(AuthError::AccountChanged));
    assert!(outcome.attempts.is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    served.await.unwrap();
}

#[tokio::test]
async fn bogus_oauth_state_cannot_exchange_a_code_or_consume_the_login() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    let initial = credentials("fixture-account", false);
    let (base, served) = server(vec![Reply::json(initial["tokens"].clone())]).await;
    let login = ChatGptLogin::start_with_issuer(path.clone(), &base, &[0])
        .await
        .unwrap();
    let url = reqwest::Url::parse(login.authorization_url()).unwrap();
    let query: BTreeMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let callback_task = tokio::spawn(async move {
        let bad = format!(
            "{}?state=attacker-state&code=untrusted-code",
            query["redirect_uri"]
        );
        assert_eq!(reqwest::get(bad).await.unwrap().status(), 400);
        assert!(!path.exists());
        let valid = format!(
            "{}?state={}&code=fixture-code",
            query["redirect_uri"], query["state"]
        );
        assert_eq!(reqwest::get(valid).await.unwrap().status(), 200);
    });
    assert_eq!(
        login.complete().await.unwrap().account_id,
        "fixture-account"
    );
    callback_task.await.unwrap();
    let requests = served.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!String::from_utf8_lossy(&requests[0].body).contains("untrusted-code"));
}

#[test]
fn route_overrides_keep_http_and_websocket_independent_and_preserve_auth_defaults() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("auth.json");
    fs::write(&path, credentials("fixture-account", false).to_string()).unwrap();
    let chatgpt = Auth::chatgpt(path).unwrap();
    assert_eq!(
        Route::from_overrides(&chatgpt, Transport::Http, None, None)
            .unwrap()
            .endpoint(),
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        Route::from_overrides(
            &chatgpt,
            Transport::WebSocket,
            Some("https://custom.invalid/v1"),
            None
        )
        .unwrap()
        .endpoint(),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        Route::from_overrides(
            &chatgpt,
            Transport::Http,
            Some("https://custom.invalid/v1/"),
            Some("wss://other.invalid/responses")
        )
        .unwrap()
        .endpoint(),
        "https://custom.invalid/v1/responses"
    );
    let api_key = Auth::api_key(SecretString::new("fixture-key".into())).unwrap();
    assert_eq!(
        Route::from_overrides(&api_key, Transport::WebSocket, None, None)
            .unwrap()
            .endpoint(),
        "wss://api.openai.com/v1/responses"
    );
    assert_eq!(
        Route::from_overrides(
            &api_key,
            Transport::WebSocket,
            None,
            Some("wss://custom.invalid/responses")
        )
        .unwrap()
        .endpoint(),
        "wss://custom.invalid/responses"
    );
}

#[tokio::test]
async fn websocket_handshake_rejection_keeps_prepared_body_without_dispatch() {
    let (base, served) = server(vec![Reply::reject(403)]).await;
    let endpoint = base.replacen("http://", "ws://", 1);
    let auth = Auth::api_key(SecretString::new("fixture-api-token".into())).unwrap();
    let outcome = ResponsesClient::new(
        auth,
        Route::new(Transport::WebSocket, &format!("{endpoint}/responses")).unwrap(),
        limits(),
    )
    .unwrap()
    .respond(&request(), &CancellationToken::new(), |_| {})
    .await;
    assert_eq!(outcome.failure.as_ref().unwrap().http_status, Some(403));
    assert_eq!(outcome.attempts.len(), 1);
    assert!(!outcome.attempts[0].dispatched);
    let capture = served.await.unwrap();
    assert!(
        capture[0].body.is_empty(),
        "only a handshake reached the fixture"
    );
    let persisted = persisted_outcome(&outcome);
    assert_eq!(persisted["request"]["status"], "prepared");
    assert_eq!(persisted["request"]["transport"], "web_socket");
}

#[test]
fn legacy_outcome_has_explicitly_unavailable_request_provenance() {
    let mut legacy = serde_json::to_value(CallOutcome::default()).unwrap();
    legacy.as_object_mut().unwrap().remove("request");
    let outcome: CallOutcome = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        persisted_outcome(&outcome)["request"],
        json!({"status":"unavailable"})
    );
}
