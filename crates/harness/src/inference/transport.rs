use super::{
    auth::{Auth, AuthError, AuthMode},
    protocol::{
        Decoder, Delta, InferenceRequest, Model, ModelSettings, ProviderResponse, ResponseDialect,
        UsdCost,
    },
};
use futures_util::{SinkExt, StreamExt};
use reqwest::{
    Client, Url,
    header::{CONTENT_TYPE, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, fmt, sync::Arc, time::Duration};
use tokio::time::{Instant, timeout};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Http,
    WebSocket,
}

#[derive(Clone, Debug)]
pub struct Route {
    pub(crate) endpoint: Url,
    pub(crate) transport: Transport,
}

impl Route {
    /// The override is the complete Responses endpoint; `api_base_url` is handled
    /// separately by `from_overrides` so a WebSocket override never changes HTTP.
    pub fn new(transport: Transport, endpoint: &str) -> Result<Self, FailureKind> {
        let endpoint = Url::parse(endpoint).map_err(|_| FailureKind::InvalidEndpoint)?;
        let secure = match transport {
            Transport::Http => "https",
            Transport::WebSocket => "wss",
        };
        let local = match transport {
            Transport::Http => "http",
            Transport::WebSocket => "ws",
        };
        let loopback = matches!(
            endpoint.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if (endpoint.scheme() != secure && !(endpoint.scheme() == local && loopback))
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || endpoint.query().is_some()
        {
            return Err(FailureKind::InvalidEndpoint);
        }
        Ok(Self {
            endpoint,
            transport,
        })
    }

    pub fn from_overrides(
        auth: &Auth,
        transport: Transport,
        api_base_url: Option<&str>,
        websocket_url: Option<&str>,
    ) -> Result<Self, FailureKind> {
        let endpoint = match transport {
            Transport::Http => format!(
                "{}/responses",
                api_base_url
                    .unwrap_or(auth.mode().api_base_url())
                    .trim_end_matches('/')
            ),
            Transport::WebSocket => websocket_url
                .unwrap_or(auth.mode().websocket_url())
                .to_owned(),
        };
        Self::new(transport, &endpoint)
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    /// Every Responses attempt counts, including a pre-generation rejection.
    /// OAuth refresh requests are coordinated separately and do not generate model tokens.
    pub max_attempts: u32,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub total_timeout: Duration,
    pub retry_delay: Duration,
    pub max_retry_delay: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_event_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            connect_timeout: Duration::from_secs(20),
            idle_timeout: Duration::from_secs(90),
            total_timeout: Duration::from_secs(300),
            retry_delay: Duration::from_millis(250),
            max_retry_delay: Duration::from_secs(5),
            max_request_bytes: 2 * 1024 * 1024,
            max_response_bytes: 16 * 1024 * 1024,
            max_event_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    #[error("invalid inference request or unsupported tool/input shape")]
    InvalidRequest,
    #[error("invalid provider endpoint; TLS is required outside loopback")]
    InvalidEndpoint,
    #[error("invalid inference limits")]
    InvalidLimits,
    #[error("provider credentials are unavailable or require login")]
    Authentication,
    #[error("provider request was rejected")]
    Rejected,
    #[error("provider connection failed")]
    Transport,
    #[error("provider stream ended without a terminal response")]
    Interrupted,
    #[error("provider stream contained malformed or inconsistent protocol data")]
    MalformedResponse,
    #[error("provider reported an error")]
    ProviderError,
    #[error("inference was cancelled")]
    Cancelled,
    #[error("inference timed out")]
    Timeout,
    #[error("inference exceeded its byte limit")]
    SizeLimit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub http_status: Option<u16>,
    pub auth_error: Option<AuthError>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Started,
    Rejected,
    ResponseReceived,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AttemptRecord {
    pub number: u32,
    pub status: AttemptStatus,
    pub http_status: Option<u16>,
    /// True once request dispatch could have reached the provider.
    pub dispatched: bool,
    /// A rejection with unknown billing, lost terminal event, or missing usage
    /// must not be normalized to zero spend by the controller.
    pub billing_uncertain: bool,
    pub elapsed_ms: u64,
}

/// Route selection without credential-bearing endpoint or authentication data.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRoute {
    Default,
    ModelOverride,
}

/// Credential-free application-body provenance, not proof of provider delivery.
/// One prepared body is shared by every attempt in this outcome. Consult each
/// attempt's `dispatched` flag for possible transport handoff; even `true` cannot
/// prove that the remote peer received it. Headers and endpoints are never retained.
/// Prompt/source data is retained verbatim; this is not a content secret scrubber.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RequestProvenance {
    /// Preparation did not finish, or a historical outcome did not capture it.
    #[default]
    Unavailable,
    Prepared {
        transport: Transport,
        dialect: ResponseDialect,
        route: RequestRoute,
        /// Exact serialized UTF-8 HTTP body or WebSocket text payload. No framing.
        /// May be prepared but never dispatched (for example, auth failure).
        body: String,
    },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CallOutcome {
    #[serde(default)]
    pub request: RequestProvenance,
    pub response: Option<ProviderResponse>,
    pub failure: Option<Failure>,
    pub attempts: Vec<AttemptRecord>,
    pub partial_text: String,
    pub partial_items: Vec<Value>,
    pub response_id: Option<String>,
    #[serde(default)]
    pub cost_usd: Option<UsdCost>,
}

impl CallOutcome {
    pub fn billing_uncertain(&self) -> bool {
        self.attempts.iter().any(|a| a.billing_uncertain)
    }

    /// A rate-limit rejection can be admitted again without duplicating a billed generation.
    pub fn rate_limited(&self) -> bool {
        self.clean_rejection(429)
            && self.failure.as_ref().is_some_and(|failure| {
                failure.kind == FailureKind::Rejected && failure.auth_error.is_none()
            })
    }

    /// A clean 401 has no generated output or uncertain billing. Retrying the
    /// same projected context can therefore resume at the failed model turn.
    pub fn authentication_rejected(&self) -> bool {
        self.clean_rejection(401)
            && self.failure.as_ref().is_some_and(|failure| {
                matches!(
                    failure.kind,
                    FailureKind::Authentication | FailureKind::Rejected
                ) && failure.auth_error.is_none()
            })
    }

    pub fn retryable_pre_generation_rejection(&self) -> bool {
        self.rate_limited() || self.authentication_rejected()
    }

    /// Returns usage only when the receipt establishes an exact amount. Clean
    /// retryable rejections are known pre-generation and therefore cost zero.
    pub fn accounted_tokens(&self) -> Option<u64> {
        if self.retryable_pre_generation_rejection() {
            Some(0)
        } else if self.billing_uncertain() {
            None
        } else {
            self.response
                .as_ref()
                .and_then(|output| output.usage.total_tokens)
                .or_else(|| {
                    self.attempts
                        .iter()
                        .all(|attempt| !attempt.dispatched)
                        .then_some(0)
                })
        }
    }

    /// Returns a gateway-reported or fixed-model catalog cost, or exact zero only when no
    /// generation could have been billed. Other dispatched calls without a receipt are unknown.
    pub fn accounted_cost(&self) -> Option<UsdCost> {
        self.cost_usd.or_else(|| {
            (self.retryable_pre_generation_rejection()
                || self.attempts.iter().all(|attempt| !attempt.dispatched))
            .then_some(UsdCost::ZERO)
        })
    }

    fn clean_rejection(&self, status: u16) -> bool {
        self.failure
            .as_ref()
            .is_some_and(|failure| failure.http_status == Some(status))
            && self.response.is_none()
            && self.response_id.is_none()
            && self.partial_text.is_empty()
            && self.partial_items.is_empty()
            && !self.attempts.is_empty()
            && self.attempts.iter().all(|attempt| {
                attempt.status == AttemptStatus::Rejected
                    && attempt.http_status == Some(status)
                    && !attempt.billing_uncertain
            })
    }
}

/// No conversation, execution, or completion state is retained here. A new socket
/// carries each explicit request; the controller supplies full history on every call.
/// `emit` must return promptly. Slow UI consumers should use a bounded host event queue.
pub struct ResponsesClient {
    client: Client,
    auth: Auth,
    route: Route,
    limits: Limits,
    model_routes: HashMap<Model, ModelRoute>,
}

/// Per-model credentials and endpoint for setups that mix providers, such as
/// GLM through a local bridge alongside a direct OpenAI model.
#[derive(Clone, Debug)]
struct ModelRoute {
    auth: Arc<Auth>,
    route: Route,
}

impl fmt::Debug for ResponsesClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponsesClient")
            .field("route", &self.route)
            .field("limits", &self.limits)
            .field("auth", &"[REDACTED]")
            .finish()
    }
}

impl ResponsesClient {
    pub fn new(auth: Auth, route: Route, limits: Limits) -> Result<Self, FailureKind> {
        if limits.max_attempts == 0
            || limits.max_attempts > 8
            || limits.connect_timeout.is_zero()
            || limits.idle_timeout.is_zero()
            || limits.total_timeout.is_zero()
            || limits.max_request_bytes == 0
            || limits.max_event_bytes == 0
            || limits.max_event_bytes > limits.max_response_bytes
            || limits.max_response_bytes > 128 * 1024 * 1024
            || limits.max_request_bytes > 32 * 1024 * 1024
        {
            return Err(FailureKind::InvalidLimits);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .connect_timeout(limits.connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| FailureKind::Transport)?;
        Ok(Self {
            client,
            auth,
            route,
            limits,
            model_routes: HashMap::new(),
        })
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Routes one model through its own credentials and endpoint instead of
    /// the client-wide defaults.
    pub fn with_model_route(mut self, model: Model, auth: Auth, route: Route) -> Self {
        self.model_routes.insert(
            model,
            ModelRoute {
                auth: Arc::new(auth),
                route,
            },
        );
        self
    }

    fn effective(&self, model: Model) -> Option<&ModelRoute> {
        self.model_routes.get(&model)
    }

    /// Count a concrete Responses input without generating model output.
    /// Callers use this to compare alternate context encodings of the same source.
    pub async fn count_input_tokens(
        &self,
        settings: ModelSettings,
        input: &[Value],
    ) -> Result<u64, FailureKind> {
        let override_route = self.effective(settings.model);
        let route = override_route
            .map(|model_route| &model_route.route)
            .unwrap_or(&self.route);
        let auth = override_route
            .map(|model_route| model_route.auth.as_ref())
            .unwrap_or(&self.auth);
        let mut endpoint = route.endpoint.clone();
        let count_scheme = match endpoint.scheme() {
            "wss" => "https",
            "ws" => "http",
            scheme => scheme,
        }
        .to_owned();
        endpoint
            .set_scheme(&count_scheme)
            .map_err(|_| FailureKind::InvalidEndpoint)?;
        endpoint.set_path(&format!(
            "{}/input_tokens",
            endpoint.path().trim_end_matches('/')
        ));
        let body = serde_json::to_vec(&serde_json::json!({
            "model": settings.model.as_str(),
            "input": input,
        }))
        .map_err(|_| FailureKind::InvalidRequest)?;
        if body.len() > self.limits.max_request_bytes {
            return Err(FailureKind::SizeLimit);
        }
        let (mut headers, _) = auth
            .headers()
            .await
            .map_err(|_| FailureKind::Authentication)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "user-agent",
            HeaderValue::from_static(concat!("tact/", env!("CARGO_PKG_VERSION"))),
        );
        headers.insert(
            "x-openai-internal-codex-responses-lite",
            HeaderValue::from_static("true"),
        );
        let mut response = timeout(
            self.limits.total_timeout,
            self.client
                .post(endpoint)
                .headers(headers)
                .body(body)
                .send(),
        )
        .await
        .map_err(|_| FailureKind::Timeout)?
        .map_err(|_| FailureKind::Transport)?;
        if !response.status().is_success() {
            return Err(if response.status().as_u16() == 401 {
                FailureKind::Authentication
            } else {
                FailureKind::Rejected
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.limits.max_response_bytes as u64)
        {
            return Err(FailureKind::SizeLimit);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| FailureKind::Transport)? {
            if bytes.len().saturating_add(chunk.len()) > self.limits.max_response_bytes {
                return Err(FailureKind::SizeLimit);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| FailureKind::MalformedResponse)?
            .get("input_tokens")
            .and_then(Value::as_u64)
            .ok_or(FailureKind::MalformedResponse)
    }

    pub async fn respond(
        &self,
        request: &InferenceRequest,
        cancel: &CancellationToken,
        mut emit: impl FnMut(Delta),
    ) -> CallOutcome {
        let mut state = CallState::default();
        let started = Instant::now();
        let operation = self.run(request, cancel, &mut state, &mut emit);
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(FailureKind::Cancelled),
            result = timeout(self.limits.total_timeout, operation) => result.unwrap_or(Err(FailureKind::Timeout)),
        };
        if let Err(kind) = result {
            if let Some(last) = state.outcome.attempts.last_mut()
                && last.status == AttemptStatus::Started
            {
                last.status = match kind {
                    FailureKind::Cancelled => AttemptStatus::Cancelled,
                    FailureKind::Timeout => AttemptStatus::TimedOut,
                    _ => AttemptStatus::Failed,
                };
                last.billing_uncertain = last.dispatched;
                last.elapsed_ms = state
                    .attempt_started
                    .unwrap_or(started)
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX);
            }
            state.outcome.failure = Some(Failure {
                kind,
                http_status: state.outcome.attempts.last().and_then(|a| a.http_status),
                auth_error: state.auth_error,
            });
        }
        let terminal_cost = state
            .decoder
            .terminal
            .as_ref()
            .and_then(|response| response.usage.cost_usd);
        state.outcome.cost_usd = match (state.response_cost_usd, terminal_cost) {
            (Some(header), Some(terminal)) if header != terminal => None,
            (Some(header), _) => Some(header),
            (None, Some(terminal)) => Some(terminal),
            (None, None) => state
                .decoder
                .terminal
                .as_ref()
                .and_then(|response| request.settings().model.catalog_cost(&response.usage)),
        };
        state.outcome.response = state.decoder.terminal.map(|mut response| {
            response.usage.cost_usd = state.outcome.cost_usd;
            response
        });
        state.outcome.response_id = state.decoder.response_id;
        state.outcome.partial_text = state.decoder.text;
        state.outcome.partial_items = state.decoder.items.into_values().collect();
        state.outcome
    }

    async fn run(
        &self,
        request: &InferenceRequest,
        cancel: &CancellationToken,
        state: &mut CallState,
        emit: &mut impl FnMut(Delta),
    ) -> Result<(), FailureKind> {
        let override_route = self.effective(request.settings().model);
        let route = override_route
            .map(|model_route| &model_route.route)
            .unwrap_or(&self.route);
        let auth = override_route
            .map(|model_route| model_route.auth.as_ref())
            .unwrap_or(&self.auth);
        let mut wire = request.wire(route.transport);
        if auth.mode() == AuthMode::ChatGpt {
            state.decoder.dialect = ResponseDialect::ChatGpt;
            let fields = wire.as_object_mut().expect("request wire is an object");
            fields.remove("max_output_tokens");
            fields.remove("truncation");
        }
        let body = serde_json::to_string(&wire).map_err(|_| FailureKind::InvalidRequest)?;
        if body.len() > self.limits.max_request_bytes {
            return Err(FailureKind::SizeLimit);
        }
        // Retain before the first await so auth failures, cancellation and timeouts
        // carry the same body as successful calls. No credential/header data enters it.
        state.outcome.request = RequestProvenance::Prepared {
            transport: route.transport,
            dialect: state.decoder.dialect,
            route: if override_route.is_some() {
                RequestRoute::ModelOverride
            } else {
                RequestRoute::Default
            },
            body: body.clone(),
        };
        let mut recovered = false;
        for number in 1..=self.limits.max_attempts {
            state.response_cost_usd = None;
            let (mut headers, generation) = auth.headers().await.map_err(|error| {
                state.auth_error = Some(error);
                FailureKind::Authentication
            })?;
            for name in ["session-id", "thread-id"] {
                headers.insert(
                    name,
                    HeaderValue::from_str(request.session_id())
                        .map_err(|_| FailureKind::InvalidRequest)?,
                );
            }
            headers.insert(
                "x-client-request-id",
                HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
                    .map_err(|_| FailureKind::InvalidRequest)?,
            );
            headers.insert(
                "user-agent",
                HeaderValue::from_static(concat!("tact/", env!("CARGO_PKG_VERSION"))),
            );
            headers.insert(
                "x-openai-internal-codex-responses-lite",
                HeaderValue::from_static("true"),
            );
            state.attempt_started = Some(Instant::now());
            state.outcome.attempts.push(AttemptRecord {
                number,
                status: AttemptStatus::Started,
                http_status: None,
                dispatched: false,
                billing_uncertain: false,
                elapsed_ms: 0,
            });
            let result = match route.transport {
                Transport::Http => {
                    self.http(&route.endpoint, body.as_bytes(), headers, state, emit)
                        .await
                }
                Transport::WebSocket => {
                    self.websocket(&route.endpoint, body.as_bytes(), headers, state, emit)
                        .await
                }
            };
            if cancel.is_cancelled() {
                return Err(FailureKind::Cancelled);
            }
            let record = state
                .outcome
                .attempts
                .last_mut()
                .expect("attempt admitted above");
            record.elapsed_ms = state
                .attempt_started
                .expect("attempt admitted above")
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            match result {
                Ok(()) => {
                    record.status = AttemptStatus::ResponseReceived;
                    record.billing_uncertain = !state
                        .decoder
                        .terminal
                        .as_ref()
                        .is_some_and(|r| r.usage.recorded());
                    return Ok(());
                }
                Err(rejection) => {
                    record.http_status = rejection.status;
                    record.status = if rejection.status.is_some() {
                        AttemptStatus::Rejected
                    } else if rejection.kind == FailureKind::Timeout {
                        AttemptStatus::TimedOut
                    } else {
                        AttemptStatus::Failed
                    };
                    record.billing_uncertain = record.dispatched
                        && !matches!(rejection.status, Some(400..=407 | 409..=499));
                    if rejection.status == Some(401) && !recovered {
                        auth.recover_unauthorized(generation)
                            .await
                            .map_err(|error| {
                                state.auth_error = Some(error);
                                FailureKind::Authentication
                            })?;
                        recovered = true;
                        if number < self.limits.max_attempts {
                            continue;
                        }
                        // The host deliberately configures one transport
                        // attempt per durable model-call receipt. Refresh now,
                        // then let the controller admit the repeated call.
                        return Err(FailureKind::Authentication);
                    }
                    // Only explicit pre-stream rejections are retried. A broken stream may
                    // already have generated/billed output and is returned for host policy.
                    if number == self.limits.max_attempts
                        || record.billing_uncertain
                        || rejection.status != Some(429)
                        || state.decoder.observed
                    {
                        return Err(rejection.kind);
                    }
                    let delay = rejection
                        .retry_after
                        .unwrap_or(self.limits.retry_delay)
                        .min(self.limits.max_retry_delay);
                    tokio::time::sleep(delay).await;
                }
            }
        }
        Err(FailureKind::Rejected)
    }

    async fn http(
        &self,
        endpoint: &Url,
        body: &[u8],
        headers: reqwest::header::HeaderMap,
        state: &mut CallState,
        emit: &mut impl FnMut(Delta),
    ) -> Result<(), AttemptFailure> {
        state.dispatched();
        // The client owns TCP/TLS connection timing. Header latency is an idle
        // wait after dispatch and remains bounded by the overall call deadline.
        let response = timeout(
            self.limits.idle_timeout,
            self.client
                .post(endpoint.clone())
                .headers(headers)
                .header(CONTENT_TYPE, "application/json")
                .header("accept", "text/event-stream")
                .body(body.to_vec())
                .send(),
        )
        .await
        .map_err(|_| FailureKind::Timeout)?
        .map_err(|_| FailureKind::Transport)?;
        let status = response.status().as_u16();
        state.response_cost_usd = response
            .headers()
            .get("x-litellm-response-cost")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<UsdCost>().ok());
        if !response.status().is_success() {
            return Err(AttemptFailure {
                kind: FailureKind::Rejected,
                status: Some(status),
                retry_after: response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs),
            });
        }
        let event_stream = match response.headers().get(CONTENT_TYPE) {
            None => state.decoder.dialect == ResponseDialect::ChatGpt,
            Some(content_type) => content_type.to_str().ok().is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            }),
        };
        if !event_stream {
            return Err(FailureKind::MalformedResponse.into());
        }
        let mut stream = response.bytes_stream();
        let mut sse = Sse::default();
        let mut total = 0usize;
        loop {
            let chunk = timeout(self.limits.idle_timeout, stream.next())
                .await
                .map_err(|_| FailureKind::Timeout)?;
            let Some(chunk) = chunk else {
                return Err(FailureKind::Interrupted.into());
            };
            let chunk = chunk.map_err(|_| FailureKind::Transport)?;
            total = total
                .checked_add(chunk.len())
                .ok_or(FailureKind::SizeLimit)?;
            if total > self.limits.max_response_bytes {
                return Err(FailureKind::SizeLimit.into());
            }
            if sse.push(
                &chunk,
                self.limits.max_event_bytes,
                &mut state.decoder,
                emit,
            )? {
                return Ok(());
            }
        }
    }

    async fn websocket(
        &self,
        endpoint: &Url,
        body: &[u8],
        headers: reqwest::header::HeaderMap,
        state: &mut CallState,
        emit: &mut impl FnMut(Delta),
    ) -> Result<(), AttemptFailure> {
        let mut handshake = endpoint
            .as_str()
            .into_client_request()
            .map_err(|_| FailureKind::InvalidEndpoint)?;
        handshake.headers_mut().extend(headers);
        handshake.headers_mut().insert(
            "openai-beta",
            HeaderValue::from_static("responses_websockets=2026-02-06"),
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(self.limits.max_event_bytes))
            .max_frame_size(Some(self.limits.max_event_bytes));
        let connected = timeout(
            self.limits.connect_timeout,
            connect_async_with_config(handshake, Some(config), true),
        )
        .await
        .map_err(|_| FailureKind::Timeout)?;
        let (mut socket, _) = connected.map_err(|error| match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => AttemptFailure {
                kind: FailureKind::Rejected,
                status: Some(response.status().as_u16()),
                retry_after: None,
            },
            _ => FailureKind::Transport.into(),
        })?;
        state.dispatched();
        let text = String::from_utf8(body.to_vec()).map_err(|_| FailureKind::InvalidRequest)?;
        timeout(
            self.limits.idle_timeout,
            socket.send(Message::Text(text.into())),
        )
        .await
        .map_err(|_| FailureKind::Timeout)?
        .map_err(|_| FailureKind::Transport)?;
        let mut total = 0usize;
        loop {
            let message = timeout(self.limits.idle_timeout, socket.next())
                .await
                .map_err(|_| FailureKind::Timeout)?;
            let Some(message) = message else {
                return Err(FailureKind::Interrupted.into());
            };
            let message = message.map_err(|_| FailureKind::Transport)?;
            total = total
                .checked_add(message.len())
                .ok_or(FailureKind::SizeLimit)?;
            if total > self.limits.max_response_bytes {
                return Err(FailureKind::SizeLimit.into());
            }
            match message {
                Message::Text(text) => {
                    if state.decoder.event(text.as_bytes(), emit)? {
                        return Ok(());
                    }
                }
                Message::Ping(_) => {
                    timeout(self.limits.idle_timeout, socket.flush())
                        .await
                        .map_err(|_| FailureKind::Timeout)?
                        .map_err(|_| FailureKind::Transport)?;
                }
                Message::Pong(_) => {}
                Message::Close(_) => return Err(FailureKind::Interrupted.into()),
                _ => return Err(FailureKind::MalformedResponse.into()),
            }
        }
    }
}

#[derive(Default)]
struct CallState {
    outcome: CallOutcome,
    decoder: Decoder,
    attempt_started: Option<Instant>,
    auth_error: Option<AuthError>,
    response_cost_usd: Option<UsdCost>,
}
impl CallState {
    fn dispatched(&mut self) {
        self.outcome
            .attempts
            .last_mut()
            .expect("attempt admitted before transport")
            .dispatched = true;
    }
}
struct AttemptFailure {
    kind: FailureKind,
    status: Option<u16>,
    retry_after: Option<Duration>,
}
impl From<FailureKind> for AttemptFailure {
    fn from(kind: FailureKind) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
        }
    }
}

#[derive(Default)]
struct Sse {
    line: Vec<u8>,
    data: Vec<u8>,
    event_bytes: usize,
}
impl Sse {
    fn push(
        &mut self,
        bytes: &[u8],
        limit: usize,
        decoder: &mut Decoder,
        emit: &mut impl FnMut(Delta),
    ) -> Result<bool, FailureKind> {
        for &byte in bytes {
            self.event_bytes += 1;
            if self.event_bytes > limit {
                return Err(FailureKind::SizeLimit);
            }
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line.is_empty() {
                self.event_bytes = 0;
                if !self.data.is_empty() {
                    self.data.pop();
                    if self.data == b"[DONE]" {
                        return Err(FailureKind::Interrupted);
                    }
                    let terminal = decoder.event(&self.data, emit)?;
                    self.data.clear();
                    if terminal {
                        return Ok(true);
                    }
                }
            } else if let Some(data) = self.line.strip_prefix(b"data:") {
                self.data
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                self.data.push(b'\n');
            }
            self.line.clear();
        }
        Ok(false)
    }
}
