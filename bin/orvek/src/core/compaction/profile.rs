use super::config::CompactionConfig;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageReader;
use nanocodex::{
    Model,
    agent::session::compaction::{ContextError, estimate_retained_item_tokens},
    oai::{
        ImageDetail,
        responses::{ContentItem, FunctionOutputBody, FunctionOutputContent, ResponseItem},
    },
};
use std::io::{self, Cursor, Write};
use zeroize::Zeroizing;

pub(crate) const PROFILE_ID: &str = "openai-8x16-experimental-v1";
pub(crate) const RENDERER_VERSION: u32 = 1;
const REQUEST_OVERHEAD_BYTES: usize = 65_536;
const TOKEN_RESERVE: u64 = 8_192;
const MAX_IMAGES: usize = 1_500;

pub(crate) fn estimate(
    config: &CompactionConfig,
    model: Model,
    prefix: &[ResponseItem],
    history: &[ResponseItem],
) -> Result<u64, ContextError> {
    match model {
        Model::Sol | Model::Terra | Model::Luna => {}
        _ => return Err(ContextError::UnsupportedProfile),
    }
    let mut bytes = ByteCount(REQUEST_OVERHEAD_BYTES);
    let mut tokens = TOKEN_RESERVE;
    let mut image_count = 0;
    for item in prefix.iter().chain(history) {
        serde_json::to_writer(&mut bytes, item).map_err(|_| ContextError::Budget {
            reason: "request size could not be measured",
        })?;
        if bytes.0 > config.max_request_bytes {
            return Err(ContextError::Budget {
                reason: "serialized request bytes",
            });
        }
        let mut item_tokens = estimate_retained_item_tokens(item);
        for (url, detail) in images(item) {
            image_count += 1;
            if image_count > MAX_IMAGES {
                return Err(ContextError::Budget {
                    reason: "total image count",
                });
            }
            let (width, height) = dimensions(url)?;
            let patches = u64::from(width.div_ceil(32)) * u64::from(height.div_ceil(32));
            let old_estimate = if detail == Some(ImageDetail::Original) {
                patches.min(10_000)
            } else {
                7_373_u64.div_ceil(4)
            };
            let (width, height) = match detail {
                Some(ImageDetail::Low) => resize(width, height, 512, None),
                Some(ImageDetail::High) => resize(width, height, 2048, Some(2_500)),
                _ => resize(width, height, 65_535, None),
            };
            let patches = u64::from(width.div_ceil(32)) * u64::from(height.div_ceil(32));
            if patches > 30_000 {
                return Err(ContextError::Budget {
                    reason: "image patch limit",
                });
            }
            // A rounding token keeps this estimate conservative when adjusting the
            // older whole-item byte estimate. Reasoning remains in the native estimate.
            item_tokens = item_tokens
                .saturating_sub(old_estimate)
                .saturating_add((patches * 6).div_ceil(5) + 1);
        }
        tokens = tokens.saturating_add(item_tokens);
    }
    Ok(tokens)
}

fn images(item: &ResponseItem) -> Vec<(&str, Option<ImageDetail>)> {
    match item {
        ResponseItem::Message { content, .. } => content
            .iter()
            .filter_map(|part| match part {
                ContentItem::InputImage { image_url, detail } => {
                    Some((image_url.as_ref(), *detail))
                }
                _ => None,
            })
            .collect(),
        ResponseItem::FunctionCallOutput {
            output: FunctionOutputBody::Content(content),
            ..
        }
        | ResponseItem::CustomToolCallOutput {
            output: FunctionOutputBody::Content(content),
            ..
        } => content
            .iter()
            .filter_map(|part| match part {
                FunctionOutputContent::InputImage { image_url, detail } => {
                    Some((image_url.as_ref(), *detail))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn dimensions(url: &str) -> Result<(u32, u32), ContextError> {
    let (metadata, encoded) = url
        .split_once(',')
        .ok_or(ContextError::UnsupportedProfile)?;
    if !metadata.starts_with("data:image/") || !metadata.ends_with(";base64") {
        return Err(ContextError::UnsupportedProfile);
    }
    if encoded.len() > 32 * 1024 * 1024 {
        return Err(ContextError::Budget {
            reason: "encoded image bytes",
        });
    }
    let bytes =
        Zeroizing::new(
            STANDARD
                .decode(encoded)
                .map_err(|_| ContextError::InvalidArchive {
                    reason: "invalid inline image encoding",
                })?,
        );
    ImageReader::new(Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .map_err(|_| ContextError::InvalidArchive {
            reason: "unknown inline image format",
        })?
        .into_dimensions()
        .map_err(|_| ContextError::InvalidArchive {
            reason: "invalid inline image dimensions",
        })
}

fn resize(width: u32, height: u32, edge: u32, budget: Option<u64>) -> (u32, u32) {
    let scale = (f64::from(edge) / f64::from(width.max(height))).min(1.0);
    let mut width = (f64::from(width) * scale).floor().max(1.0) as u32;
    let mut height = (f64::from(height) * scale).floor().max(1.0) as u32;
    if let Some(budget) = budget {
        let patches = u64::from(width.div_ceil(32)) * u64::from(height.div_ceil(32));
        if patches > budget {
            let scale = (1024.0 * budget as f64 / f64::from(width) / f64::from(height)).sqrt();
            let patch_width = f64::from(width) * scale / 32.0;
            let patch_height = f64::from(height) * scale / 32.0;
            let adjusted = scale
                * (patch_width.floor() / patch_width).min(patch_height.floor() / patch_height);
            width = (f64::from(width) * adjusted).floor().max(1.0) as u32;
            height = (f64::from(height) * adjusted).floor().max(1.0) as u32;
        }
    }
    (width, height)
}

struct ByteCount(usize);

impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("request size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_detail_matches_documented_patch_resizing() {
        assert_eq!(resize(2048, 2048, 2048, Some(2500)), (1600, 1600));
        assert_eq!(resize(1568, 1568, 2048, Some(2500)), (1568, 1568));
        assert_eq!(resize(1024, 512, 512, None), (512, 256));
    }
}
