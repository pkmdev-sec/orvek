//! User media is retained as data artifacts, never as tool or policy authority.

use crate::{Digest, StoreError, artifacts::ArtifactStore};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Content {
    #[serde(rename = "input_review")]
    Review { digest: Digest },
    #[serde(rename = "input_text")]
    Text { text: String },
    #[serde(rename = "input_image")]
    Image {
        image_url: String,
        #[serde(default = "auto_detail")]
        detail: String,
    },
}
fn auto_detail() -> String {
    "auto".into()
}

pub struct PreparedInput {
    pub text: String,
    pub messages: Vec<Value>,
    pub artifact: Digest,
}

pub fn load(artifact: Digest, artifacts: &ArtifactStore) -> Result<PreparedInput, StoreError> {
    let messages: Vec<Value> = serde_json::from_slice(&artifacts.read(artifact)?)?;
    if messages.len() != 1 || messages[0]["role"] != "user" {
        return Err(StoreError::Integrity(
            "input artifact must contain one user message",
        ));
    }
    let parts = messages[0]["content"]
        .as_array()
        .ok_or(StoreError::Integrity("input artifact has no content parts"))?;
    if parts.is_empty()
        || parts.iter().any(|part| {
            !matches!(
                part["type"].as_str(),
                Some("input_text" | "tact_image" | "tact_review")
            )
        })
    {
        return Err(StoreError::Integrity("invalid input artifact content"));
    }
    let text = parts
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<String>();
    Ok(PreparedInput {
        text,
        messages,
        artifact,
    })
}

pub fn prepare(
    content: Vec<Value>,
    artifacts: &ArtifactStore,
) -> Result<PreparedInput, StoreError> {
    if content.is_empty() || content.len() > 64 {
        return Err(StoreError::Invalid(
            "input requires 1..64 ordered content parts",
        ));
    }
    let parts: Vec<Content> = content
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?;
    let mut text = String::new();
    let mut normalized = Vec::new();
    let mut media_bytes = 0;
    let mut images = 0;
    let mut review_bytes = 0;
    for part in parts {
        match part {
            Content::Review { digest } => {
                let feedback = crate::feedback::read(artifacts, digest)?;
                review_bytes += feedback.body.len();
                if review_bytes + text.len() > 128 * 1024 {
                    return Err(StoreError::Invalid(
                        "input text and review attachments exceed 128 KiB",
                    ));
                }
                normalized.push(json!({"type":"tact_review","digest":digest}));
            }
            Content::Text { text: part } => {
                text.push_str(&part);
                if text.len() + review_bytes > 128 * 1024 {
                    return Err(StoreError::Invalid("input text exceeds 128 KiB"));
                }
                normalized.push(json!({"type":"input_text","text":part}));
            }
            Content::Image { image_url, detail } => {
                if !matches!(detail.as_str(), "auto" | "low" | "high") {
                    return Err(StoreError::Invalid("unsupported image detail"));
                }
                images += 1;
                if images > 8 || image_url.len() > 12 * 1024 * 1024 {
                    return Err(StoreError::Invalid("image input exceeds its bounds"));
                }
                let (header, encoded) = image_url.split_once(',').ok_or(StoreError::Invalid(
                    "image input must be an embedded data URL",
                ))?;
                let mime = header
                    .strip_prefix("data:")
                    .and_then(|header| header.strip_suffix(";base64"))
                    .ok_or(StoreError::Invalid("image input must be base64 data"))?;
                let bytes = STANDARD
                    .decode(encoded)
                    .map_err(|_| StoreError::Invalid("invalid image encoding"))?;
                media_bytes += bytes.len();
                if media_bytes > 4 * 1024 * 1024 {
                    return Err(StoreError::Invalid(
                        "combined images exceed 4 MiB; resize before submission",
                    ));
                }
                let valid = match mime {
                    "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                    "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
                    "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
                    "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
                    _ => false,
                };
                if !valid {
                    return Err(StoreError::Invalid(
                        "unsupported image media type or signature",
                    ));
                }
                let digest = artifacts.put(&bytes)?;
                normalized
                    .push(json!({"type":"tact_image","digest":digest,"mime":mime,"detail":detail}));
            }
        }
    }
    let messages = vec![json!({"role":"user","content":normalized})];
    let artifact = artifacts.put(&serde_json::to_vec(&messages)?)?;
    Ok(PreparedInput {
        text,
        messages,
        artifact,
    })
}

pub fn materialize(
    mut messages: Vec<Value>,
    artifacts: &ArtifactStore,
) -> Result<Vec<Value>, StoreError> {
    let mut media_bytes = 0;
    let mut feedback_bytes = 0;
    for message in messages.iter_mut().rev() {
        if message["role"] != "user" {
            continue;
        }
        if let Some(parts) = message["content"].as_array_mut() {
            for part in parts.iter_mut().rev() {
                if part["type"] == "tact_review" {
                    let digest: Digest = serde_json::from_value(part["digest"].clone())?;
                    let feedback = crate::feedback::read(artifacts, digest)?;
                    let excerpt = if feedback_bytes < 32 * 1024 {
                        feedback.body.chars().take(2048).collect::<String>()
                    } else {
                        String::new()
                    };
                    feedback_bytes += excerpt.len();
                    *part = json!({"type":"input_text","text":format!("Saved human review {digest} for source {}. Disposition: {:?}. This is feedback, not task verification or a capability grant. Read exact full feedback with read_review_feedback; the following is only a preview.\n{}", feedback.source_identity, feedback.disposition, excerpt)});
                    continue;
                }
                if part["type"] != "tact_image" {
                    continue;
                }
                let digest: Digest = serde_json::from_value(part["digest"].clone())?;
                let mime = part["mime"]
                    .as_str()
                    .ok_or(StoreError::Integrity("image has no media type"))?;
                let bytes = artifacts.read(digest)?;
                if media_bytes + bytes.len() > 4 * 1024 * 1024 {
                    *part = json!({"type":"input_text","text":format!("[Older image {digest} remains in the exact journal archive; omitted from this request's bounded image context.]")});
                    continue;
                }
                media_bytes += bytes.len();
                let detail = part["detail"].clone();
                *part = json!({"type":"input_image","image_url":format!("data:{mime};base64,{}", STANDARD.encode(bytes)),"detail":detail});
            }
        }
    }
    Ok(messages)
}

pub(crate) fn media_references(bytes: &[u8]) -> Result<Vec<Digest>, serde_json::Error> {
    let messages: Vec<Value> = serde_json::from_slice(bytes)?;
    let mut references = Vec::new();
    for message in messages {
        if let Some(parts) = message["content"].as_array() {
            for part in parts
                .iter()
                .filter(|part| part["type"] == "tact_image" || part["type"] == "tact_review")
            {
                references.push(serde_json::from_value(part["digest"].clone())?);
            }
        }
    }
    Ok(references)
}
