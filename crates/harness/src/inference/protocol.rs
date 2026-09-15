use super::transport::{FailureKind, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Model {
    #[default]
    Sol,
    Terra,
    Luna,
    #[serde(alias = "glm-5.3")]
    Glm,
}

impl Model {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sol => "gpt-5.6-sol",
            Self::Terra => "gpt-5.6-terra",
            Self::Luna => "gpt-5.6-luna",
            Self::Glm => "glm-5.3",
        }
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
            "glm" | "glm-5.3" => Ok(Self::Glm),
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
pub struct InferenceRequest {
    settings: ModelSettings,
    input: Vec<Value>,
    tools: Vec<Value>,
    instructions: String,
    session_id: String,
    max_output_tokens: u64,
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
        if input.is_empty()
            || max_output_tokens == 0
            || session_id.is_empty()
            || session_id.len() > 128
            || !session_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(FailureKind::InvalidRequest);
        }
        for item in &input {
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
        Ok(Self {
            settings,
            input,
            tools,
            instructions,
            session_id,
            max_output_tokens,
        })
    }

    pub fn settings(&self) -> ModelSettings {
        self.settings
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn wire(&self, transport: Transport) -> Value {
        let mut request = json!({
            "model": self.settings.model.as_str(), "input": self.input,
            "tools": self.tools, "instructions": self.instructions, "store": false,
            "parallel_tool_calls": false, "tool_choice": "auto", "truncation": "disabled",
            "max_output_tokens": self.max_output_tokens,
            "reasoning": {"effort": self.settings.thinking.as_str(), "summary": "auto", "context": "all_turns"},
            "include": ["reasoning.encrypted_content"], "text": {"verbosity": "low"},
            "prompt_cache_key": self.session_id,
        });
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
                    !parts.is_empty()
                        && parts
                            .iter()
                            .all(|part| match part.get("type").and_then(Value::as_str) {
                                Some("input_text" | "output_text") => {
                                    part.get("text").is_some_and(Value::is_string)
                                }
                                Some("refusal") => {
                                    part.get("refusal").is_some_and(Value::is_string)
                                }
                                Some("input_image") => {
                                    part.get("image_url").is_some_and(Value::is_string)
                                        || part.get("file_id").is_some_and(Value::is_string)
                                }
                                _ => false,
                            })
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
                && item.get("output").is_some_and(Value::is_string)
        }
        "reasoning" => {
            item.get("encrypted_content").is_some_and(Value::is_string)
                && item.get("summary").is_some_and(Value::is_array)
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
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
    /// Exact terminal items allow the controller to construct the next history.
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

#[derive(Default)]
pub(crate) struct Decoder {
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
                let expected = kind
                    .strip_prefix("response.")
                    .ok_or(FailureKind::MalformedResponse)?;
                if string(response, "status")? != expected {
                    return Err(FailureKind::MalformedResponse);
                }
                let history_items = response
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
    use super::{Decoder, Model, terminal_item_confirms};
    use serde_json::json;

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
