use super::transport::{FailureKind, Transport};
use crate::Digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, str::FromStr};

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Model {
    #[default]
    Sol,
    Terra,
    Luna,
    #[serde(alias = "gpt-6-astra")]
    Astra,
    #[serde(alias = "glm-5.3")]
    Glm,
    #[serde(alias = "gpt-5.3-codex-spark")]
    Spark,
}

impl Model {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sol => "gpt-5.6-sol",
            Self::Terra => "gpt-5.6-terra",
            Self::Luna => "gpt-5.6-luna",
            Self::Astra => "gpt-6-astra",
            Self::Glm => "glm-5.3",
            Self::Spark => "gpt-5.3-codex-spark",
        }
    }

    pub(crate) fn catalog_cost(self, usage: &Usage) -> Option<UsdCost> {
        let input = usage.input_tokens?;
        let cached = usage.cached_input_tokens?;
        let output = usage.output_tokens?;
        let total = usage.total_tokens?;
        if cached > input || input.checked_add(output) != Some(total) {
            return None;
        }
        let (input_rate, cached_rate, output_rate) = match self {
            Self::Sol => (4_000_000_000_000, 400_000_000_000, 20_000_000_000_000),
            Self::Terra => (2_000_000_000_000, 200_000_000_000, 12_000_000_000_000),
            Self::Luna => (200_000_000_000, 20_000_000_000, 1_200_000_000_000),
            Self::Astra => (10_000_000_000_000, 1_000_000_000_000, 50_000_000_000_000),
            Self::Glm | Self::Spark => return None,
        };
        let uncached_cost = u128::from(input - cached).checked_mul(input_rate)?;
        let cached_cost = u128::from(cached).checked_mul(cached_rate)?;
        let output_cost = u128::from(output).checked_mul(output_rate)?;
        Some(UsdCost(
            uncached_cost
                .checked_add(cached_cost)?
                .checked_add(output_cost)?,
        ))
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Model {
    type Err = FailureKind;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "sol" | "gpt-5.6-sol" => Ok(Self::Sol),
            "terra" | "gpt-5.6-terra" => Ok(Self::Terra),
            "luna" | "gpt-5.6-luna" => Ok(Self::Luna),
            "astra" | "gpt-6-astra" => Ok(Self::Astra),
            "glm" | "glm-5.3" => Ok(Self::Glm),
            "spark" | "gpt-5.3-codex-spark" => Ok(Self::Spark),
            _ => Err(FailureKind::InvalidRequest),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Thinking {
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
}

impl Thinking {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningMode {
    #[default]
    Standard,
    Pro,
}

impl ReasoningMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Pro => "pro",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelSettings {
    pub model: Model,
    pub thinking: Thinking,
    pub reasoning_mode: ReasoningMode,
    pub fast_mode: bool,
}

/// Validated protocol shape, not trusted instructions or admitted tool authority.
/// Only caller-owned function tools are accepted; provider-hosted execution needs a
/// separate controller capability and is intentionally excluded here.
#[derive(Clone, Debug)]
pub struct PromptInput {
    items: Vec<Value>,
    stable_items: usize,
    stable_segments: Vec<Digest>,
}

impl PromptInput {
    pub fn all_live(items: Vec<Value>) -> Self {
        Self {
            items,
            stable_items: 0,
            stable_segments: Vec::new(),
        }
    }

    pub fn segmented(
        stable: Vec<Value>,
        live: Vec<Value>,
        stable_segments: Vec<Digest>,
    ) -> Result<Self, FailureKind> {
        if !stable.is_empty() && stable_segments.is_empty() {
            return Err(FailureKind::InvalidRequest);
        }
        let stable_items = stable.len();
        let mut items = stable;
        items.extend(live);
        Ok(Self {
            items,
            stable_items,
            stable_segments,
        })
    }

    pub fn stable(&self) -> &[Value] {
        &self.items[..self.stable_items]
    }

    pub fn live(&self) -> &[Value] {
        &self.items[self.stable_items..]
    }
}

/// Host-generated context media. This type is separate from user input so
/// controller-owned pages never pass through user attachment admission limits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InternalContextMedia {
    pub digest: Digest,
    pub mime: String,
    pub locator: String,
    /// A reusable provider file identity, when the active transport has one.
    /// Without it, serialization deterministically falls back to a data URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_file_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromptCacheIdentity {
    pub version: u32,
    pub routing: Digest,
    pub instructions: Digest,
    pub tools: Digest,
    pub stable_segments: Vec<Digest>,
    pub lineage: Digest,
}

#[derive(Clone, Debug)]
pub struct InferenceRequest {
    settings: ModelSettings,
    input: PromptInput,
    tools: Vec<Value>,
    instructions: String,
    session_id: String,
    prompt_cache_key: String,
    cache_identity: PromptCacheIdentity,
    max_output_tokens: u64,
    output_format: Option<Value>,
}

impl InferenceRequest {
    pub fn new(
        settings: ModelSettings,
        input: Vec<Value>,
        tools: Vec<Value>,
        instructions: String,
        session_id: String,
        max_output_tokens: u64,
    ) -> Result<Self, FailureKind> {
        Self::new_segmented(
            settings,
            PromptInput::all_live(input),
            tools,
            instructions,
            session_id,
            max_output_tokens,
        )
    }

    pub fn new_segmented(
        settings: ModelSettings,
        input: PromptInput,
        tools: Vec<Value>,
        instructions: String,
        session_id: String,
        max_output_tokens: u64,
    ) -> Result<Self, FailureKind> {
        if input.items.is_empty() || max_output_tokens == 0 || !valid_routing_key(&session_id) {
            return Err(FailureKind::InvalidRequest);
        }
        for item in &input.items {
            validate_input(item)?;
        }
        let mut names = std::collections::BTreeSet::new();
        for tool in &tools {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                return Err(FailureKind::InvalidRequest);
            };
            if tool.get("type").and_then(Value::as_str) != Some("function")
                || name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                || !names.insert(name)
                || !tool.get("parameters").is_some_and(Value::is_object)
                || tool
                    .get("parameters")
                    .and_then(|v| v.get("type"))
                    .and_then(Value::as_str)
                    != Some("object")
                || tool.get("async").is_some_and(|v| v != false)
            {
                return Err(FailureKind::InvalidRequest);
            }
        }
        let prompt_cache_key = session_id.clone();
        let cache_identity = cache_identity(
            settings,
            &prompt_cache_key,
            &instructions,
            &tools,
            &input.stable_segments,
            None,
        )?;
        Ok(Self {
            settings,
            input,
            tools,
            instructions,
            prompt_cache_key,
            cache_identity,
            session_id,
            max_output_tokens,
            output_format: None,
        })
    }

    pub fn settings(&self) -> ModelSettings {
        self.settings
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn cache_lineage(&self) -> Digest {
        self.cache_identity.lineage
    }

    pub fn cache_identity(&self) -> &PromptCacheIdentity {
        &self.cache_identity
    }

    pub fn prompt_input(&self) -> &PromptInput {
        &self.input
    }

    /// Routes requests with a shared, exact prefix to the same provider cache
    /// without changing this request's session or thread identity.
    pub fn with_prompt_cache_key(
        mut self,
        prompt_cache_key: impl Into<String>,
    ) -> Result<Self, FailureKind> {
        let prompt_cache_key = prompt_cache_key.into();
        if !valid_routing_key(&prompt_cache_key) {
            return Err(FailureKind::InvalidRequest);
        }
        self.cache_identity = cache_identity(
            self.settings,
            &prompt_cache_key,
            &self.instructions,
            &self.tools,
            &self.input.stable_segments,
            self.output_format.as_ref(),
        )?;
        self.prompt_cache_key = prompt_cache_key;
        Ok(self)
    }

    pub(crate) fn with_json_schema(
        mut self,
        name: &str,
        schema: Value,
    ) -> Result<Self, FailureKind> {
        self.output_format = Some(json!({
            "type": "json_schema", "name": name, "strict": true, "schema": schema,
        }));
        self.cache_identity = cache_identity(
            self.settings,
            &self.prompt_cache_key,
            &self.instructions,
            &self.tools,
            &self.input.stable_segments,
            self.output_format.as_ref(),
        )?;
        Ok(self)
    }

    pub(crate) fn wire(&self, transport: Transport) -> Value {
        let mut request = json!({
            "model": self.settings.model.as_str(), "input": self.input.items,
            "tools": self.tools, "instructions": self.instructions, "store": false,
            "parallel_tool_calls": false, "tool_choice": "auto", "truncation": "disabled",
            "max_output_tokens": self.max_output_tokens,
            "reasoning": {"effort": self.settings.thinking.as_str(), "summary": "auto", "context": "all_turns"},
            "include": ["reasoning.encrypted_content"], "text": {"verbosity": "low"},
            "prompt_cache_key": self.prompt_cache_key,
        });
        if let Some(format) = &self.output_format {
            request["text"]["format"] = format.clone();
        }
        match transport {
            Transport::Http => request["stream"] = true.into(),
            Transport::WebSocket => request["type"] = "response.create".into(),
        }
        if self.settings.reasoning_mode == ReasoningMode::Pro {
            request["reasoning"]["mode"] = "pro".into();
        }
        if self.settings.fast_mode {
            request["service_tier"] = "priority".into();
        }
        request
    }
}

fn cache_identity(
    settings: ModelSettings,
    routing_key: &str,
    instructions: &str,
    tools: &[Value],
    stable_segments: &[Digest],
    output_format: Option<&Value>,
) -> Result<PromptCacheIdentity, FailureKind> {
    let routing = Digest::of(routing_key.as_bytes());
    let instructions = Digest::of(instructions.as_bytes());
    let tools = Digest::of_value(tools).map_err(|_| FailureKind::InvalidRequest)?;
    let lineage = Digest::of_value(&(
        "orvek-prompt-cache-lineage-v1",
        settings,
        routing,
        instructions,
        tools,
        stable_segments,
    ))
    .map_err(|_| FailureKind::InvalidRequest)?;
    let lineage = match output_format {
        Some(format) => Digest::of_value(&("orvek-structured-output-lineage-v1", lineage, format))
            .map_err(|_| FailureKind::InvalidRequest)?,
        None => lineage,
    };
    Ok(PromptCacheIdentity {
        version: 1,
        routing,
        instructions,
        tools,
        stable_segments: stable_segments.to_vec(),
        lineage,
    })
}

fn valid_routing_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"-_.".contains(&character))
}

fn valid_input_part(part: &Value) -> bool {
    match part.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text") => part.get("text").is_some_and(Value::is_string),
        Some("refusal") => part.get("refusal").is_some_and(Value::is_string),
        Some("input_image") => {
            part.get("image_url").is_some_and(Value::is_string)
                || part.get("file_id").is_some_and(Value::is_string)
        }
        _ => false,
    }
}

fn validate_input(item: &Value) -> Result<(), FailureKind> {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let valid = match kind {
        "message" => {
            matches!(
                item.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "developer" | "system")
            ) && match item.get("content") {
                Some(Value::String(_)) => true,
                Some(Value::Array(parts)) => {
                    !parts.is_empty() && parts.iter().all(valid_input_part)
                }
                _ => false,
            }
        }
        "function_call" => ["call_id", "name", "arguments"]
            .iter()
            .all(|key| item.get(key).is_some_and(Value::is_string)),
        "function_call_output" => {
            item.get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
                && match item.get("output") {
                    Some(Value::String(_)) => true,
                    Some(Value::Array(parts)) => {
                        !parts.is_empty() && parts.iter().all(valid_input_part)
                    }
                    _ => false,
                }
        }
        "reasoning" => {
            item.get("summary").is_some_and(Value::is_array)
                && item.get("encrypted_content").is_none_or(Value::is_string)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(FailureKind::InvalidRequest)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Completed,
    Failed,
    Incomplete,
}

const ATTO_USD_PER_USD: u128 = 1_000_000_000_000_000_000;

/// An exact non-negative USD amount with eighteen decimal places of precision.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct UsdCost(u128);

impl UsdCost {
    pub const ZERO: Self = Self(0);

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    fn decimal_string(self) -> String {
        let dollars = self.0 / ATTO_USD_PER_USD;
        let remainder = self.0 % ATTO_USD_PER_USD;
        if remainder == 0 {
            return dollars.to_string();
        }
        let fraction = format!("{remainder:018}");
        format!("{dollars}.{}", fraction.trim_end_matches('0'))
    }
}

impl Serialize for UsdCost {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.decimal_string())
    }
}

impl<'de> Deserialize<'de> for UsdCost {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CostVisitor;

        impl serde::de::Visitor<'_> for CostVisitor {
            type Value = UsdCost;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an exact non-negative USD decimal")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                value
                    .parse()
                    .map_err(|()| E::custom("invalid exact USD cost"))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(UsdCost(u128::from(value)))
            }

            fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(UsdCost(value))
            }
        }

        deserializer.deserialize_any(CostVisitor)
    }
}

impl FromStr for UsdCost {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let (mantissa, exponent) =
            value
                .split_once(['e', 'E'])
                .map_or((value, 0_i32), |(mantissa, exponent)| {
                    exponent
                        .parse::<i32>()
                        .map(|exponent| (mantissa, exponent))
                        .unwrap_or(("", 0))
                });
        if mantissa.is_empty() || mantissa.starts_with('-') {
            return Err(());
        }
        let mantissa = mantissa.strip_prefix('+').unwrap_or(mantissa);
        let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if integer.is_empty() && fraction.is_empty()
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(());
        }
        let digits = format!("{integer}{fraction}");
        let coefficient = digits.parse::<u128>().map_err(|_| ())?;
        let power = 18_i64 + i64::from(exponent) - i64::try_from(fraction.len()).map_err(|_| ())?;
        let scaled = if power >= 0 {
            coefficient
                .checked_mul(
                    10_u128
                        .checked_pow(u32::try_from(power).map_err(|_| ())?)
                        .ok_or(())?,
                )
                .ok_or(())?
        } else {
            let divisor = 10_u128
                .checked_pow(u32::try_from(-power).map_err(|_| ())?)
                .ok_or(())?;
            if coefficient % divisor != 0 {
                return Err(());
            }
            coefficient / divisor
        };
        Ok(Self(scaled))
    }
}

impl fmt::Display for UsdCost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "${}", self.decimal_string())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<UsdCost>,
}

impl Usage {
    pub fn recorded(&self) -> bool {
        self.input_tokens.is_some() && self.output_tokens.is_some() && self.total_tokens.is_some()
    }
    fn parse(value: &Value) -> Self {
        let mut usage = Self {
            input_tokens: value.get("input_tokens").and_then(Value::as_u64),
            output_tokens: value.get("output_tokens").and_then(Value::as_u64),
            total_tokens: value.get("total_tokens").and_then(Value::as_u64),
            cached_input_tokens: value
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64),
            reasoning_tokens: value
                .pointer("/output_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64),
            cost_usd: value
                .get("cost")
                .or_else(|| value.get("response_cost"))
                .and_then(|cost| {
                    cost.as_str()
                        .map(str::to_owned)
                        .or_else(|| Some(cost.to_string()))
                })
                .and_then(|cost| cost.parse().ok()),
        };
        if let (Some(input), Some(output), Some(total)) =
            (usage.input_tokens, usage.output_tokens, usage.total_tokens)
            && input.checked_add(output) != Some(total)
        {
            usage.total_tokens = None;
        }
        if usage
            .cached_input_tokens
            .zip(usage.input_tokens)
            .is_some_and(|(cached, input)| cached > input)
        {
            usage.cached_input_tokens = None;
        }
        if usage
            .reasoning_tokens
            .zip(usage.output_tokens)
            .is_some_and(|(reasoning, output)| reasoning > output)
        {
            usage.reasoning_tokens = None;
        }
        usage
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentValidity {
    JsonObject,
    MalformedJson,
    NotObject,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolProposal {
    pub item_id: String,
    pub call_id: String,
    pub name: String,
    /// Exact provider bytes: malformed JSON is never repaired or guessed.
    pub arguments: String,
    pub validity: ArgumentValidity,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputItem {
    Message {
        id: String,
        text: String,
        refusals: Vec<String>,
    },
    ToolProposal(ToolProposal),
    /// Opaque reasoning and future output types remain data; they are never executed.
    Opaque {
        item: Value,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderResponse {
    pub id: String,
    pub status: ResponseStatus,
    pub output: Vec<OutputItem>,
    /// Confirmed output items allow the controller to construct the next history.
    pub history_items: Vec<Value>,
    pub usage: Usage,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Delta {
    Created { response_id: String },
    Text { item_id: String, text: String },
    ReasoningSummary { item_id: String, text: String },
    ToolArguments { item_id: String, arguments: String },
    ItemDone { item: OutputItem },
}

#[derive(Default, Eq, PartialEq)]
pub(crate) enum ResponseDialect {
    #[default]
    OpenAi,
    ChatGpt,
}

#[derive(Default)]
pub(crate) struct Decoder {
    pub dialect: ResponseDialect,
    pub response_id: Option<String>,
    pub items: BTreeMap<u64, Value>,
    pub text: String,
    pub terminal: Option<ProviderResponse>,
    pub observed: bool,
}

impl Decoder {
    pub fn event(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(Delta),
    ) -> Result<bool, FailureKind> {
        let event: Value =
            serde_json::from_slice(bytes).map_err(|_| FailureKind::MalformedResponse)?;
        self.observed = true;
        let kind = string(&event, "type")?;
        match kind {
            "response.created" | "response.in_progress" => {
                let id = string(
                    event
                        .get("response")
                        .ok_or(FailureKind::MalformedResponse)?,
                    "id",
                )?;
                self.bind_id(id)?;
                if kind == "response.created" {
                    emit(Delta::Created {
                        response_id: id.into(),
                    });
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let text = string(&event, "delta")?;
                self.text.push_str(text);
                emit(Delta::Text {
                    item_id: string(&event, "item_id")?.into(),
                    text: text.into(),
                });
            }
            "response.reasoning_summary_text.delta" => emit(Delta::ReasoningSummary {
                item_id: string(&event, "item_id")?.into(),
                text: string(&event, "delta")?.into(),
            }),
            "response.function_call_arguments.delta" => emit(Delta::ToolArguments {
                item_id: string(&event, "item_id")?.into(),
                arguments: string(&event, "delta")?.into(),
            }),
            "response.output_item.done" => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .ok_or(FailureKind::MalformedResponse)?;
                let item = event.get("item").ok_or(FailureKind::MalformedResponse)?;
                let normalized = normalize(item)?;
                if self.items.insert(index, item.clone()).is_some() {
                    return Err(FailureKind::MalformedResponse);
                }
                emit(Delta::ItemDone { item: normalized });
            }
            "response.completed" | "response.failed" | "response.incomplete" => {
                let response = event
                    .get("response")
                    .ok_or(FailureKind::MalformedResponse)?;
                let id = string(response, "id")?;
                self.bind_id(id)?;
                // Some OpenAI-compatible bridges always name the terminal
                // event `response.completed` and carry the real outcome in
                // `status`. Trust the status whenever it names a terminal
                // outcome instead of requiring it to match the event name.
                let expected = match string(response, "status")? {
                    status @ ("completed" | "failed" | "incomplete") => status,
                    _ => return Err(FailureKind::MalformedResponse),
                };
                let mut history_items = response
                    .get("output")
                    .and_then(Value::as_array)
                    .ok_or(FailureKind::MalformedResponse)?
                    .clone();
                if expected == "completed"
                    && (response.get("error").is_some_and(|value| !value.is_null())
                        || response
                            .get("incomplete_details")
                            .is_some_and(|value| !value.is_null()))
                {
                    return Err(FailureKind::MalformedResponse);
                }
                if expected == "completed"
                    && history_items.is_empty()
                    && self.dialect == ResponseDialect::ChatGpt
                {
                    history_items = self.items.values().cloned().collect();
                }
                for (index, completed_item) in &self.items {
                    let index =
                        usize::try_from(*index).map_err(|_| FailureKind::MalformedResponse)?;
                    if !terminal_item_confirms(completed_item, history_items.get(index)) {
                        return Err(FailureKind::MalformedResponse);
                    }
                }
                let output = history_items
                    .iter()
                    .map(normalize)
                    .collect::<Result<Vec<_>, _>>()?;
                let mut calls = std::collections::BTreeSet::new();
                for item in &output {
                    if let OutputItem::ToolProposal(proposal) = item
                        && !calls.insert(&proposal.call_id)
                    {
                        return Err(FailureKind::MalformedResponse);
                    }
                }
                let status = match expected {
                    "completed" => ResponseStatus::Completed,
                    "failed" => ResponseStatus::Failed,
                    _ => ResponseStatus::Incomplete,
                };
                self.terminal = Some(ProviderResponse {
                    id: id.into(),
                    status,
                    output,
                    history_items,
                    usage: Usage::parse(&response["usage"]),
                });
                return Ok(true);
            }
            "error" => return Err(FailureKind::ProviderError),
            _ => {} // Unknown progress events have no authority; a known terminal is mandatory.
        }
        Ok(false)
    }

    fn bind_id(&mut self, id: &str) -> Result<(), FailureKind> {
        if id.is_empty()
            || self
                .response_id
                .as_deref()
                .is_some_and(|previous| previous != id)
        {
            return Err(FailureKind::MalformedResponse);
        }
        self.response_id = Some(id.into());
        Ok(())
    }
}

fn terminal_item_confirms(completed: &Value, terminal: Option<&Value>) -> bool {
    let Some(terminal) = terminal else {
        return false;
    };
    if completed == terminal {
        return true;
    }
    if completed.get("type").and_then(Value::as_str) != Some("reasoning")
        || terminal.get("type").and_then(Value::as_str) != Some("reasoning")
    {
        return false;
    }
    let (Some(completed), Some(terminal)) = (completed.as_object(), terminal.as_object()) else {
        return false;
    };
    completed.len() == terminal.len()
        && completed.iter().all(|(key, value)| {
            let Some(terminal_value) = terminal.get(key) else {
                return false;
            };
            if key == "encrypted_content" {
                return value.as_str().is_some_and(|value| !value.is_empty())
                    && terminal_value
                        .as_str()
                        .is_some_and(|value| !value.is_empty());
            }
            value == terminal_value
        })
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, FailureKind> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(FailureKind::MalformedResponse)
}

fn normalize(item: &Value) -> Result<OutputItem, FailureKind> {
    match string(item, "type")? {
        "message" => {
            if string(item, "role")? != "assistant" {
                return Err(FailureKind::MalformedResponse);
            }
            let mut text = String::new();
            let mut refusals = Vec::new();
            for part in item
                .get("content")
                .and_then(Value::as_array)
                .ok_or(FailureKind::MalformedResponse)?
            {
                match string(part, "type")? {
                    "output_text" => text.push_str(string(part, "text")?),
                    "refusal" => refusals.push(string(part, "refusal")?.into()),
                    _ => return Ok(OutputItem::Opaque { item: item.clone() }),
                }
            }
            Ok(OutputItem::Message {
                id: string(item, "id")?.into(),
                text,
                refusals,
            })
        }
        "function_call" => {
            let arguments = string(item, "arguments")?.to_owned();
            let validity = match serde_json::from_str::<Value>(&arguments) {
                Ok(Value::Object(_)) => ArgumentValidity::JsonObject,
                Ok(_) => ArgumentValidity::NotObject,
                Err(_) => ArgumentValidity::MalformedJson,
            };
            let call_id = string(item, "call_id")?;
            let name = string(item, "name")?;
            if call_id.is_empty() || name.is_empty() {
                return Err(FailureKind::MalformedResponse);
            }
            Ok(OutputItem::ToolProposal(ToolProposal {
                item_id: string(item, "id")?.into(),
                call_id: call_id.into(),
                name: name.into(),
                arguments,
                validity,
            }))
        }
        _ => Ok(OutputItem::Opaque { item: item.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Decoder, InferenceRequest, Model, ModelSettings, Thinking, Transport, Usage, UsdCost,
        terminal_item_confirms,
    };
    use serde_json::json;

    #[test]
    fn usd_cost_keeps_the_provider_decimal_exact() {
        for (input, expected) in [
            ("0", "$0"),
            ("0.000214", "$0.000214"),
            ("1.230000000000000000", "$1.23"),
            ("2.5e-7", "$0.00000025"),
            ("250e-2", "$2.5"),
        ] {
            assert_eq!(input.parse::<UsdCost>().unwrap().to_string(), expected);
        }
        assert!("0.0000000000000000001".parse::<UsdCost>().is_err());
        assert!("-1".parse::<UsdCost>().is_err());
        assert!("NaN".parse::<UsdCost>().is_err());
    }

    #[test]
    fn usd_cost_round_trips_through_journal_json_values() {
        let cost = "0.000000250000000001".parse::<UsdCost>().unwrap();
        let value = serde_json::to_value(cost).unwrap();

        assert_eq!(value, serde_json::json!("0.000000250000000001"));
        assert_eq!(serde_json::from_value::<UsdCost>(value).unwrap(), cost);
    }

    #[test]
    fn usd_cost_addition_does_not_round_fractional_cents() {
        let first = "0.00000025".parse::<UsdCost>().unwrap();
        let second = "0.00000075".parse::<UsdCost>().unwrap();
        assert_eq!(first.checked_add(second).unwrap().to_string(), "$0.000001");
    }

    #[test]
    fn astra_model_round_trips_without_changing_existing_defaults() {
        for name in ["astra", "gpt-6-astra"] {
            assert_eq!(name.parse::<Model>().unwrap(), Model::Astra);
            assert_eq!(
                serde_json::from_value::<Model>(json!(name)).unwrap(),
                Model::Astra
            );
        }
        assert_eq!(Model::Astra.to_string(), "gpt-6-astra");
        assert_eq!(serde_json::to_value(Model::Astra).unwrap(), json!("astra"));
        assert_eq!(Model::default(), Model::Sol);
        assert_eq!(serde_json::to_value(Model::Sol).unwrap(), json!("sol"));
        assert!("gpt-6".parse::<Model>().is_err());
    }

    #[test]
    fn astra_uses_its_wire_id_and_supported_efforts_on_both_transports() {
        for thinking in [
            Thinking::Low,
            Thinking::Medium,
            Thinking::High,
            Thinking::Xhigh,
            Thinking::Max,
        ] {
            let request = InferenceRequest::new(
                ModelSettings {
                    model: Model::Astra,
                    thinking,
                    ..ModelSettings::default()
                },
                vec![json!({"role": "user", "content": "hello"})],
                vec![],
                "test".into(),
                "astra-session".into(),
                128,
            )
            .unwrap();
            for transport in [Transport::Http, Transport::WebSocket] {
                let wire = request.wire(transport);
                assert_eq!(wire["model"], "gpt-6-astra");
                assert_eq!(wire["reasoning"]["effort"], thinking.as_str());
                assert!(wire["reasoning"].get("mode").is_none());
                assert!(wire.get("service_tier").is_none());
            }
        }
    }

    #[test]
    fn astra_catalog_cost_uses_installed_prime_catalog_rates() {
        // Prime Agent's OpenAI catalog: $10 input, $1 cached, $50 output per million.
        let mut usage = Usage {
            input_tokens: Some(1_000_000),
            cached_input_tokens: Some(250_000),
            output_tokens: Some(100_000),
            total_tokens: Some(1_100_000),
            ..Usage::default()
        };
        assert_eq!(
            Model::Astra.catalog_cost(&usage).unwrap().to_string(),
            "$12.75"
        );
        usage.cached_input_tokens = None;
        assert!(Model::Astra.catalog_cost(&usage).is_none());
        usage.cached_input_tokens = Some(1_000_001);
        assert!(Model::Astra.catalog_cost(&usage).is_none());
        usage.cached_input_tokens = Some(0);
        usage.total_tokens = Some(1);
        assert!(Model::Astra.catalog_cost(&usage).is_none());
    }

    #[test]
    fn glm_model_round_trips_through_its_wire_name() {
        use std::str::FromStr;

        for name in ["glm", "glm-5.3"] {
            let model = Model::from_str(name).unwrap();
            assert_eq!(model, Model::Glm);
            assert_eq!(model.as_str(), "glm-5.3");
        }
        let parsed: Result<Model, _> = "glm-5.3".parse();
        assert_eq!(parsed.unwrap().as_str(), "glm-5.3");
        assert!("glm-4".parse::<Model>().is_err());
    }

    #[test]
    fn bridge_shaped_reasoning_items_are_valid_input() {
        use serde_json::json;

        let bridge = json!({
            "type": "reasoning",
            "id": "rs-1",
            "summary": [{"type": "summary_text", "text": "thinking"}],
            "content": []
        });
        assert!(super::validate_input(&bridge).is_ok());

        let encrypted = json!({
            "type": "reasoning",
            "id": "rs-2",
            "encrypted_content": "ciphertext",
            "summary": [],
            "content": []
        });
        assert!(super::validate_input(&encrypted).is_ok());

        let malformed = json!({"type": "reasoning", "id": "rs-3"});
        assert!(super::validate_input(&malformed).is_err());
    }

    #[test]
    fn terminal_reencrypted_reasoning_confirms_the_completed_item() {
        let mut decoder = Decoder::default();
        let completed_reasoning = json!({
            "type": "reasoning",
            "id": "reasoning-1",
            "encrypted_content": "first-ciphertext",
            "summary": [{"type": "summary_text", "text": "same summary"}],
            "content": [],
        });
        decoder
            .event(
                &serde_json::to_vec(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": completed_reasoning,
                }))
                .unwrap(),
                &mut |_| {},
            )
            .unwrap();

        let terminal = json!({
            "type": "response.completed",
            "response": {
                "id": "response-1",
                "status": "completed",
                "error": null,
                "incomplete_details": null,
                "output": [{
                    "type": "reasoning",
                    "id": "reasoning-1",
                    "encrypted_content": "terminal-ciphertext",
                    "summary": [{"type": "summary_text", "text": "same summary"}],
                    "content": [],
                }],
                "usage": {"input_tokens": 5, "output_tokens": 1, "total_tokens": 6},
            },
        });

        assert!(
            decoder
                .event(&serde_json::to_vec(&terminal).unwrap(), &mut |_| {})
                .unwrap()
        );
    }

    #[test]
    fn bridge_reasoning_item_keeps_its_output_index_in_the_terminal() {
        let mut decoder = Decoder::default();
        let reasoning = json!({
            "id": "rs-1", "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "thinking"}], "content": [],
        });
        let message = json!({
            "id": "msg-1", "type": "message", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": "{\"kind\":\"information\"}"}],
        });
        decoder
            .event(
                &serde_json::to_vec(&json!({
                    "type": "response.output_item.done", "output_index": 0, "item": reasoning,
                }))
                .unwrap(),
                &mut |_| {},
            )
            .unwrap();
        decoder
            .event(
                &serde_json::to_vec(&json!({
                    "type": "response.output_item.done", "output_index": 1, "item": message,
                }))
                .unwrap(),
                &mut |_| {},
            )
            .unwrap();

        let terminal = json!({
            "type": "response.completed",
            "response": {
                "id": "response-1", "status": "completed",
                "output": [
                    {"id": "rs-1", "type": "reasoning",
                     "summary": [{"type": "summary_text", "text": "thinking"}], "content": []},
                    {"id": "msg-1", "type": "message", "role": "assistant", "status": "completed",
                     "content": [{"type": "output_text", "text": "{\"kind\":\"information\"}"}]},
                ],
                "usage": {"input_tokens": 41, "output_tokens": 200, "total_tokens": 241},
            },
        });
        assert!(
            decoder
                .event(&serde_json::to_vec(&terminal).unwrap(), &mut |_| {})
                .unwrap()
        );
        let provider = decoder.terminal.expect("terminal response");
        assert_eq!(provider.status, crate::inference::ResponseStatus::Completed);
    }

    #[test]
    fn bridge_terminal_event_name_yields_to_the_status() {
        let mut decoder = Decoder::default();
        let done = json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id": "msg-1",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "{\"kind\":\"infor"}],
            },
        });
        decoder
            .event(&serde_json::to_vec(&done).unwrap(), &mut |_| {})
            .unwrap();

        // The z.ai bridge names every terminal event `response.completed`
        // and reports a truncation through `status` and `incomplete_details`.
        let terminal = json!({
            "type": "response.completed",
            "response": {
                "id": "response-1",
                "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
                "output": [{
                    "id": "msg-1",
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "{\"kind\":\"infor"}],
                }],
                "usage": {"input_tokens": 41, "output_tokens": 200, "total_tokens": 241},
            },
        });

        assert!(
            decoder
                .event(&serde_json::to_vec(&terminal).unwrap(), &mut |_| {})
                .unwrap()
        );
        let provider = decoder.terminal.expect("terminal response");
        assert_eq!(
            provider.status,
            crate::inference::ResponseStatus::Incomplete
        );
    }

    #[test]
    fn terminal_reencrypted_reasoning_rejects_semantic_changes() {
        let completed = json!({
            "type": "reasoning",
            "id": "reasoning-1",
            "encrypted_content": "first-ciphertext",
            "summary": [{"type": "summary_text", "text": "original summary"}],
            "content": [],
        });
        let changed = json!({
            "type": "reasoning",
            "id": "reasoning-1",
            "encrypted_content": "terminal-ciphertext",
            "summary": [{"type": "summary_text", "text": "changed summary"}],
            "content": [],
        });

        assert!(!terminal_item_confirms(&completed, Some(&changed)));
    }
}
