use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, time::Duration};
use orvek_harness::inference::{
    ArgumentValidity, AttemptStatus, Delta, FailureKind, InferenceRequest, Limits, Model,
    ModelSettings, OutputItem, ReasoningMode, ResponseStatus, ResponsesClient, Route, Thinking,
    Transport,
    auth::{
        Auth, AuthError, AuthMode, ChatGptLogin, SecretString, chatgpt_auth_status, logout_chatgpt,
    },
};
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
    mime: &'static str,
    body: Vec<u8>,
    fragment: bool,
    stall: Duration,
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
            mime: "text/event-stream; charset=utf-8",
            body,
            fragment: false,
            stall: Duration::ZERO,
        }
    }
    fn reject(status: u16) -> Self {
        Self {
            status,
            mime: "application/json",
            body: b"{\"error\":\"provider-controlled secret text\"}".to_vec(),
            fragment: false,
            stall: Duration::ZERO,
        }
    }
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            mime: "application/json",
            body: value.to_string().into_bytes(),
            fragment: false,
            stall: Duration::ZERO,
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
            let header = format!(
                "HTTP/1.1 {} Fixture\r\nContent-Type: {}\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n",
                reply.status,
                reply.mime,
                reply.body.len()
            );
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
    let (base, served) = server(vec![reply]).await;
    let mut deltas = Vec::new();
    let settings = ModelSettings {
        model: Model::Terra,
        thinking: Thinking::Max,
        reasoning_mode: ReasoningMode::Pro,
        fast_mode: true,
    };
    let outcome = client(&base, limits())
        .respond(
            &request_with(settings, vec![tool_schema()]),
            &CancellationToken::new(),
            |delta| deltas.push(delta),
        )
        .await;
    assert!(outcome.failure.is_none());
    assert!(!outcome.billing_uncertain());
    let response = outcome.response.unwrap();
    assert_eq!(response.status, ResponseStatus::Completed);
    assert_eq!(response.usage.input_tokens, Some(13));
    assert_eq!(response.usage.cached_input_tokens, Some(0));
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
    assert_eq!(captures[0].path, "/responses");
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
async fn retry_count_and_uncertain_failed_attempts_remain_visible_after_success() {
    let (base, served) = server(vec![
        Reply::reject(503),
        Reply::reject(429),
        Reply::sse(vec![terminal("completed", vec![message("done")], usage())]),
    ])
    .await;
    let outcome = client(&base, limits())
        .respond(&request(), &CancellationToken::new(), |_| {})
        .await;
    assert!(outcome.failure.is_none());
    assert_eq!(outcome.attempts.len(), 3);
    assert!(outcome.attempts[0].billing_uncertain);
    assert!(!outcome.attempts[1].billing_uncertain);
    assert!(outcome.billing_uncertain());
    assert_eq!(served.await.unwrap().len(), 3);
}

#[tokio::test]
async fn exhausted_retries_and_permanent_rejections_are_bounded_and_redacted() {
    for (code, attempts) in [(503, 3), (400, 1), (401, 1), (307, 1)] {
        let (base, served) = server((0..attempts).map(|_| Reply::reject(code)).collect()).await;
        let outcome = client(&base, limits())
            .respond(&request(), &CancellationToken::new(), |_| {})
            .await;
        assert_eq!(outcome.attempts.len(), attempts);
        assert!(outcome.failure.is_some());
        assert!(!format!("{outcome:?}").contains("provider-controlled secret text"));
        assert_eq!(served.await.unwrap().len(), attempts);
    }
}

#[tokio::test]
async fn malformed_events_and_inconsistent_terminal_ids_do_not_finish() {
    let variants = vec![
        vec![
            created(),
            json!({"type":"response.completed","response":{"id":"other","status":"completed","output":[],"usage":usage()}}),
        ],
        vec![
            json!({"type":"response.completed","response":{"id":"response-fixture","status":"incomplete","output":[],"usage":usage()}}),
        ],
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
        let request: Value = serde_json::from_str(&request).unwrap();
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
    assert_eq!(outcome.response.unwrap().status, ResponseStatus::Completed);
    let wire = served.await.unwrap();
    assert_eq!(wire["type"], "response.create");
    assert!(wire.get("stream").is_none());
    assert_eq!(wire["model"], "gpt-5.6-sol");
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
        assert_eq!(outcome.failure.unwrap().kind, FailureKind::Authentication);
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
            socket.next().await.unwrap().unwrap();
            for event in [created(), text_delta("partial ws")] {
                socket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .unwrap();
            }
            socket.close(None).await.unwrap();
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
            outcome.failure.unwrap().kind,
            if should_cancel {
                FailureKind::Cancelled
            } else {
                FailureKind::Interrupted
            }
        );
        served.await.unwrap();
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
