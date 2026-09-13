//! Narrow ownership boundaries for archived source payloads.
//!
//! Text retrieval borrows JSON envelopes and decodes only the selected string.
//! Successfully decoded tool results are guarded until explicitly transferred to
//! Nanocodex. SDK-owned item IDs have no mutable access and cannot be cleared here.
//! Serde's string scratch space, intermediate or failed SDK deserialization, and
//! copies retained after `into_item` remain outside these guards' guarantee.

use nanocodex::{
    agent::session::compaction::ContextError,
    oai::{
        ImageDetail,
        responses::{
            FunctionOutputBody, FunctionOutputContent, ItemStatus, ResponseItem, ToolCaller,
        },
    },
};
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeSeed, SeqAccess, Visitor},
};
use serde_json::value::RawValue;
use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ArchivedText {
    #[zeroize(skip)]
    pub(crate) role: &'static str,
    pub(crate) text: Zeroizing<String>,
}

impl fmt::Debug for ArchivedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ArchivedText([REDACTED])")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ItemKind {
    Message,
    FunctionCallOutput,
    CustomToolCallOutput,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type")]
    kind: ItemKind,
    #[serde(borrow)]
    role: Option<&'a RawValue>,
    #[serde(borrow)]
    call_id: Option<&'a RawValue>,
    #[serde(borrow)]
    output: Option<&'a RawValue>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Role {
    User,
    Assistant,
    Developer,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContentKind {
    InputText,
    OutputText,
    InputImage,
    InputAudio,
    EncryptedContent,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Content<'a> {
    #[serde(rename = "type")]
    kind: ContentKind,
    #[serde(borrow)]
    text: Option<&'a RawValue>,
}

pub(crate) fn decode_text(encoded: &[u8], index: usize) -> Result<ArchivedText, ContextError> {
    let envelope: Envelope<'_> =
        serde_json::from_slice(encoded).map_err(|_| invalid("invalid archived text envelope"))?;
    let (role, raw, message) = match envelope.kind {
        ItemKind::Message => {
            let role: Role = serde_json::from_str(
                envelope
                    .role
                    .ok_or_else(|| invalid("missing archived message role"))?
                    .get(),
            )
            .map_err(|_| invalid("invalid archived message role"))?;
            let role = match role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Developer => "developer",
            };
            (
                role,
                envelope
                    .content
                    .ok_or_else(|| invalid("missing archived message content"))?,
                true,
            )
        }
        ItemKind::FunctionCallOutput | ItemKind::CustomToolCallOutput => {
            if !envelope
                .call_id
                .is_some_and(|value| value.get().starts_with('"'))
            {
                return Err(invalid("invalid archived tool call identity"));
            }
            (
                "tool",
                envelope
                    .output
                    .ok_or_else(|| invalid("missing archived tool output"))?,
                false,
            )
        }
        ItemKind::Other => return Err(invalid("archived item has no retrievable text")),
    };
    if !message && raw.get().starts_with('"') {
        if index != 0 {
            return Err(invalid("archived text index is out of range"));
        }
        return Ok(ArchivedText {
            role,
            text: decode_string(raw)?,
        });
    }
    let mut decoder = serde_json::Deserializer::from_str(raw.get());
    let selected = SelectIndex(index)
        .deserialize(&mut decoder)
        .map_err(|_| invalid("invalid archived content array"))?
        .ok_or_else(|| invalid("archived text index is out of range"))?;
    let content: Content<'_> = serde_json::from_str(selected.get())
        .map_err(|_| invalid("invalid archived content block"))?;
    match content.kind {
        ContentKind::InputText => {}
        ContentKind::OutputText if message => {}
        _ => return Err(invalid("the archived content block is not text")),
    }
    let text = content
        .text
        .ok_or_else(|| invalid("missing archived content text"))?;
    Ok(ArchivedText {
        role,
        text: decode_string(text)?,
    })
}

/// Walk the array without allocating an owned vector or decoding unselected text.
struct SelectIndex(usize);

impl<'de> DeserializeSeed<'de> for SelectIndex {
    type Value = Option<&'de RawValue>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for SelectIndex {
    type Value = Option<&'de RawValue>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an archived content array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut selected = None;
        let mut index = 0;
        while let Some(value) = sequence.next_element::<&'de RawValue>()? {
            if index == self.0 {
                selected = Some(value);
            }
            index += 1;
        }
        Ok(selected)
    }
}

fn decode_string(raw: &RawValue) -> Result<Zeroizing<String>, ContextError> {
    deserialize_string(&mut serde_json::Deserializer::from_str(raw.get()))
        .map_err(|_| invalid("invalid archived text string"))
}

/// Guard strings as soon as serde passes them to application-owned storage.
/// Escaped JSON strings may first pass through serde's non-zeroizing scratch buffer.
pub(super) fn deserialize_string<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Zeroizing<String>, D::Error> {
    struct Text;
    impl Visitor<'_> for Text {
        type Value = Zeroizing<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a text string")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(Zeroizing::new(value.to_owned()))
        }

        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(Zeroizing::new(value))
        }
    }
    deserializer.deserialize_string(Text)
}

pub(crate) struct ArchivedToolResult {
    item: Option<ResponseItem>,
}

impl ArchivedToolResult {
    /// Borrows the guarded SDK value. Any clone the caller makes owns separate
    /// storage and is outside this guard's zeroization boundary.
    pub(crate) fn item(&self) -> &ResponseItem {
        self.item
            .as_ref()
            .expect("the archive guard owns its item until transfer")
    }

    /// Transfers the decoded SDK item into Nanocodex's history ownership. This
    /// guard no longer controls that item's lifetime or any copies the SDK makes.
    pub(crate) fn into_item(mut self) -> ResponseItem {
        self.item
            .take()
            .expect("the archive guard owns its item until transfer")
    }
}

impl Zeroize for ArchivedToolResult {
    fn zeroize(&mut self) {
        let Some(item) = self.item.as_mut() else {
            return;
        };
        let (call_id, output, caller, created_by, metadata) = match item {
            ResponseItem::FunctionCallOutput {
                call_id,
                output,
                caller,
                created_by,
                internal_chat_message_metadata_passthrough,
                ..
            } => (
                call_id,
                output,
                caller,
                created_by,
                internal_chat_message_metadata_passthrough,
            ),
            ResponseItem::CustomToolCallOutput {
                call_id,
                name,
                output,
                caller,
                created_by,
                internal_chat_message_metadata_passthrough,
                ..
            } => {
                name.zeroize();
                (
                    call_id,
                    output,
                    caller,
                    created_by,
                    internal_chat_message_metadata_passthrough,
                )
            }
            _ => unreachable!("the archive guard accepts only validated tool outputs"),
        };
        call_id.zeroize();
        match output {
            FunctionOutputBody::Text(text) => text.zeroize(),
            FunctionOutputBody::Content(parts) => {
                for part in parts {
                    match part {
                        FunctionOutputContent::InputText { text } => text.zeroize(),
                        FunctionOutputContent::InputImage { image_url, .. } => image_url.zeroize(),
                        FunctionOutputContent::InputAudio { audio_url } => audio_url.zeroize(),
                        FunctionOutputContent::EncryptedContent { encrypted_content } => {
                            encrypted_content.zeroize()
                        }
                    }
                }
            }
        }
        if let Some(ToolCaller::Program { caller_id }) = caller {
            caller_id.zeroize();
        }
        *caller = None;
        created_by.zeroize();
        if let Some(metadata) = metadata {
            metadata.turn_id.zeroize();
        }
        *metadata = None;
    }
}

impl ZeroizeOnDrop for ArchivedToolResult {}

impl Drop for ArchivedToolResult {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl fmt::Debug for ArchivedToolResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ArchivedToolResult([REDACTED])")
    }
}

pub(crate) fn decode_tool_result(encoded: &[u8]) -> Result<ArchivedToolResult, ContextError> {
    let envelope: Envelope<'_> =
        serde_json::from_slice(encoded).map_err(|_| invalid("invalid archived tool envelope"))?;
    if !matches!(
        envelope.kind,
        ItemKind::FunctionCallOutput | ItemKind::CustomToolCallOutput
    ) {
        return Err(invalid("the archived item is not a tool output"));
    }
    validate_tool_fields(
        encoded,
        matches!(envelope.kind, ItemKind::CustomToolCallOutput),
    )?;
    let item: ResponseItem =
        serde_json::from_slice(encoded).map_err(|_| invalid("invalid archived tool output"))?;
    if !matches!(
        item,
        ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. }
    ) {
        return Err(invalid(
            "archived tool output did not match the SDK contract",
        ));
    }
    Ok(ArchivedToolResult { item: Some(item) })
}

/// Recovery may discard damaged image content, but must not change its tool envelope.
pub(crate) fn tool_output_envelope(
    item: &ResponseItem,
) -> Result<Zeroizing<Vec<u8>>, ContextError> {
    let envelope = match item {
        ResponseItem::FunctionCallOutput {
            id: _,
            output: _,
            call_id,
            caller,
            status,
            created_by,
            internal_chat_message_metadata_passthrough,
        } => (
            "function_call_output",
            None,
            call_id,
            caller,
            status,
            created_by,
            internal_chat_message_metadata_passthrough,
        ),
        ResponseItem::CustomToolCallOutput {
            id: _,
            output: _,
            name,
            call_id,
            caller,
            status,
            created_by,
            internal_chat_message_metadata_passthrough,
        } => (
            "custom_tool_call_output",
            name.as_deref(),
            call_id,
            caller,
            status,
            created_by,
            internal_chat_message_metadata_passthrough,
        ),
        _ => return Err(invalid("saved projection is not a tool output")),
    };
    serde_json::to_vec(&envelope)
        .map(Zeroizing::new)
        .map_err(|_| invalid("could not encode tool output envelope"))
}

// The SDK's untagged Other fallback would otherwise own arbitrary JSON for a
// malformed known variant. Validate the known fields while they are still borrowed.
#[derive(Deserialize)]
struct ToolFields<'a> {
    #[serde(borrow)]
    id: Option<&'a RawValue>,
    #[serde(borrow)]
    call_id: &'a RawValue,
    #[serde(borrow)]
    output: &'a RawValue,
    #[serde(borrow)]
    name: Option<&'a RawValue>,
    #[serde(borrow)]
    caller: Option<&'a RawValue>,
    #[serde(borrow)]
    status: Option<&'a RawValue>,
    #[serde(borrow)]
    created_by: Option<&'a RawValue>,
    #[serde(borrow)]
    internal_chat_message_metadata_passthrough: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct ToolContent<'a> {
    #[serde(rename = "type")]
    kind: ContentKind,
    #[serde(borrow)]
    text: Option<&'a RawValue>,
    #[serde(borrow)]
    image_url: Option<&'a RawValue>,
    #[serde(borrow)]
    detail: Option<&'a RawValue>,
    #[serde(borrow)]
    audio_url: Option<&'a RawValue>,
    #[serde(borrow)]
    encrypted_content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CallerKind {
    Direct,
    Program,
}

#[derive(Deserialize)]
struct Caller<'a> {
    #[serde(rename = "type")]
    kind: CallerKind,
    #[serde(borrow)]
    caller_id: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Metadata<'a> {
    #[serde(borrow)]
    turn_id: Option<&'a RawValue>,
}

fn validate_tool_fields(encoded: &[u8], custom: bool) -> Result<(), ContextError> {
    let fields: ToolFields<'_> =
        serde_json::from_slice(encoded).map_err(|_| invalid("invalid archived tool fields"))?;
    validate_json_string(fields.call_id)?;
    for value in [
        fields.id,
        fields.created_by,
        custom.then_some(fields.name).flatten(),
    ]
    .into_iter()
    .flatten()
    {
        validate_json_string(value)?;
    }
    if let Some(status) = fields.status {
        serde_json::from_str::<ItemStatus>(status.get())
            .map_err(|_| invalid("invalid archived tool status"))?;
    }
    if let Some(raw) = fields.caller {
        let caller: Caller<'_> =
            serde_json::from_str(raw.get()).map_err(|_| invalid("invalid archived tool caller"))?;
        if matches!(caller.kind, CallerKind::Program) {
            validate_json_string(
                caller
                    .caller_id
                    .ok_or_else(|| invalid("missing archived tool caller identity"))?,
            )?;
        }
    }
    if let Some(raw) = fields.internal_chat_message_metadata_passthrough {
        let metadata: Metadata<'_> = serde_json::from_str(raw.get())
            .map_err(|_| invalid("invalid archived tool metadata"))?;
        if let Some(value) = metadata.turn_id {
            validate_json_string(value)?;
        }
    }
    if fields.output.get().starts_with('"') {
        return validate_json_string(fields.output);
    }
    let parts: Vec<&RawValue> = serde_json::from_str(fields.output.get())
        .map_err(|_| invalid("invalid archived tool content array"))?;
    for raw in parts {
        let part: ToolContent<'_> = serde_json::from_str(raw.get())
            .map_err(|_| invalid("invalid archived tool content"))?;
        let payload = match part.kind {
            ContentKind::InputText => part.text,
            ContentKind::InputImage => {
                if let Some(detail) = part.detail {
                    serde_json::from_str::<ImageDetail>(detail.get())
                        .map_err(|_| invalid("invalid archived image detail"))?;
                }
                part.image_url
            }
            ContentKind::InputAudio => part.audio_url,
            ContentKind::EncryptedContent => part.encrypted_content,
            _ => return Err(invalid("unsupported archived tool content")),
        };
        validate_json_string(
            payload.ok_or_else(|| invalid("missing archived tool content payload"))?,
        )?;
    }
    Ok(())
}

fn validate_json_string(raw: &RawValue) -> Result<(), ContextError> {
    struct StringType;
    impl Visitor<'_> for StringType {
        type Value = ();
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a JSON string")
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
    }
    (&mut serde_json::Deserializer::from_str(raw.get()))
        .deserialize_str(StringType)
        .map_err(|_| invalid("invalid archived string field"))
}

fn invalid(reason: &'static str) -> ContextError {
    ContextError::InvalidArchive { reason }
}

#[cfg(test)]
mod tests {
    use super::{ArchivedText, ArchivedToolResult, decode_text, decode_tool_result};
    use nanocodex::oai::responses::{FunctionOutputBody, FunctionOutputContent, ResponseItem};
    use serde_json::json;
    use zeroize::{Zeroize, ZeroizeOnDrop};

    #[test]
    fn selected_text_preserves_roles_utf8_code_and_json_escapes() {
        let text = "  let Exact::Name = \"λ日本語\";\r\n\tvalue +=  17;\\path\n";
        for role in ["user", "assistant", "developer"] {
            let encoded = serde_json::to_vec(&json!({"type":"message","role":role,"content":[
                {"type":"input_image","image_url":"data:image/png;base64,AA=="},
                {"type":"output_text","text":text,"annotations":[]},
            ]}))
            .unwrap();
            let decoded = decode_text(&encoded, 1).unwrap();
            assert_eq!(decoded.role, role);
            assert_eq!(decoded.text.as_str(), text);
            assert!(decode_text(&encoded, 0).is_err());
        }
        for kind in ["function_call_output", "custom_tool_call_output"] {
            let encoded =
                serde_json::to_vec(&json!({"type":kind,"call_id":"fixture","output":text}))
                    .unwrap();
            let decoded = decode_text(&encoded, 0).unwrap();
            assert_eq!(decoded.role, "tool");
            assert_eq!(decoded.text.as_str(), text);
            assert!(decode_text(&encoded, 1).is_err());
        }
        let escaped =
            br#"{"type":"function_call_out\u0070ut","call_id":"fixture","output":"a\t\u03bb\r\n"}"#;
        assert_eq!(decode_text(escaped, 0).unwrap().text.as_str(), "a\tλ\r\n");
    }

    #[test]
    fn mixed_tool_arrays_decode_only_the_selected_text_index() {
        let encoded = serde_json::to_vec(
            &json!({"type":"function_call_output","call_id":"fixture","output":[
                {"type":"input_image","image_url":"data:image/png;base64,AA=="},
                {"type":"input_text","text":"first\tλ"},
                {"type":"encrypted_content","encrypted_content":"opaque fixture bytes"},
                {"type":"input_text","text":"last  line\n"},
            ]}),
        )
        .unwrap();
        assert_eq!(decode_text(&encoded, 1).unwrap().text.as_str(), "first\tλ");
        assert_eq!(
            decode_text(&encoded, 3).unwrap().text.as_str(),
            "last  line\n"
        );
        for index in [0, 2, 4, usize::MAX] {
            assert!(decode_text(&encoded, index).is_err());
        }
    }

    #[test]
    fn malformed_missing_and_nontext_sources_have_content_free_errors() {
        for encoded in [
            br#"{"type":"message","role":"user","content":[{"type":"input_text","text":3}]}"#
                .as_slice(),
            br#"{"type":"message","role":"invalid role fixture","content":[]}"#,
            br#"{"type":"message","content":[]}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":{}}"#,
            br#"{"type":"function_call_output","output":"body"}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":{}}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":[{"type":"input_audio","audio_url":3}]}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":"\ud800"}"#,
            br#"{"type":"function_call_output","call_id":"fixture"}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":"unfinished"#,
            br#"{"type":"reasoning","encrypted_content":"opaque fixture"}"#,
            b"\xff",
        ] {
            let error = decode_text(encoded, 0).unwrap_err();
            assert!(!error.to_string().contains("invalid role fixture"));
            assert!(!format!("{error:?}").contains("unfinished"));
        }
        for encoded in [
            br#"{"type":"message","role":"user","content":[{"type":"input_text","text":"body"}]}"#
                .as_slice(),
            br#"{"type":"reasoning","encrypted_content":"opaque fixture"}"#,
            br#"{"type":"function_call_output","output":"body"}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":{}}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":[{"type":"input_audio","audio_url":3}]}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":"\ud800"}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":"body","caller":{"type":"program"}}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":"body","created_by":3}"#,
            br#"{"type":"function_call_output","call_id":"fixture","output":"body","internal_chat_message_metadata_passthrough":{"turn_id":3}}"#,
        ] {
            assert!(decode_tool_result(encoded).is_err());
        }
    }

    #[test]
    fn guards_redact_debug_and_clear_every_owned_tool_payload_and_metadata() {
        fn secret<T: Zeroize + ZeroizeOnDrop>() {}
        secret::<ArchivedText>();
        secret::<ArchivedToolResult>();
        let encoded = serde_json::to_vec(&json!({
            "type":"custom_tool_call_output","id":"ctco_fixture","call_id":"fixture call","name":"fixture tool",
            "caller":{"type":"program","caller_id":"fixture program"},"created_by":"fixture creator",
            "internal_chat_message_metadata_passthrough":{"turn_id":"fixture turn"},
            "output":[{"type":"input_text","text":"code λ"},
                {"type":"input_image","image_url":"data:image/png;base64,AA=="},
                {"type":"input_audio","audio_url":"data:audio/wav;base64,AA=="},
                {"type":"encrypted_content","encrypted_content":"opaque fixture"}],
        })).unwrap();
        let mut text = decode_text(&encoded, 0).unwrap();
        assert_eq!(format!("{text:?}"), "ArchivedText([REDACTED])");
        text.zeroize();
        assert!(text.text.is_empty());
        let mut guarded = decode_tool_result(&encoded).unwrap();
        assert_eq!(format!("{guarded:?}"), "ArchivedToolResult([REDACTED])");
        guarded.zeroize();
        let ResponseItem::CustomToolCallOutput {
            call_id,
            name,
            output: FunctionOutputBody::Content(parts),
            caller,
            created_by,
            internal_chat_message_metadata_passthrough,
            ..
        } = guarded.item()
        else {
            panic!("tool result shape changed");
        };
        assert!(call_id.bytes().all(|byte| byte == 0));
        assert!(name.is_none());
        assert!(caller.is_none());
        assert!(created_by.is_none());
        assert!(internal_chat_message_metadata_passthrough.is_none());
        for part in parts {
            let bytes = match part {
                FunctionOutputContent::InputText { text } => text.as_bytes(),
                FunctionOutputContent::InputImage { image_url, .. } => image_url.as_bytes(),
                FunctionOutputContent::InputAudio { audio_url } => audio_url.as_bytes(),
                FunctionOutputContent::EncryptedContent { encrypted_content } => {
                    encrypted_content.as_bytes()
                }
            };
            assert!(!bytes.is_empty());
            assert!(bytes.iter().all(|&byte| byte == 0));
        }
        assert_eq!(guarded.item().id().unwrap().as_str(), "ctco_fixture");
    }

    #[test]
    fn function_text_clears_and_explicit_transfer_preserves_the_sdk_item() {
        let encoded =
            br#"{"type":"function_call_output","call_id":"fixture","output":"exact\ttext\n"}"#;
        let guarded = decode_tool_result(encoded).unwrap();
        let expected = serde_json::to_value(guarded.item()).unwrap();
        let transferred = guarded.into_item();
        assert_eq!(serde_json::to_value(transferred).unwrap(), expected);
        let mut guarded = decode_tool_result(encoded).unwrap();
        guarded.zeroize();
        let ResponseItem::FunctionCallOutput {
            output: FunctionOutputBody::Text(text),
            ..
        } = guarded.item()
        else {
            panic!("expected plain tool output");
        };
        assert!(!text.is_empty());
        assert!(text.bytes().all(|byte| byte == 0));
    }
}
