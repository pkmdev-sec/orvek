//! Displayable prompt text paired with model-only image content.

use orvek_harness::Digest;
use serde_json::{Value, json};
use std::{fmt, ops::Range};

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Submission {
    text: String,
    images: Vec<SubmissionImage>,
    reviews: Vec<Digest>,
}

#[derive(Clone, Eq, PartialEq)]
struct SubmissionImage {
    range: Range<usize>,
    data_url: String,
}

impl Submission {
    pub(crate) fn text(text: String) -> Self {
        Self {
            text,
            images: Vec::new(),
            reviews: Vec::new(),
        }
    }

    pub(crate) fn multimodal(
        text: String,
        images: impl IntoIterator<Item = (Range<usize>, String)>,
    ) -> Self {
        let images = images
            .into_iter()
            .map(|(range, data_url)| SubmissionImage { range, data_url })
            .collect();
        Self {
            text,
            images,
            reviews: Vec::new(),
        }
    }

    pub(crate) fn attach_review(mut self, digest: Digest) -> Self {
        if !self.reviews.contains(&digest) {
            self.reviews.push(digest);
        }
        self
    }

    pub(crate) fn reviews(&self) -> &[Digest] {
        &self.reviews
    }

    pub(crate) fn images(&self) -> impl Iterator<Item = (Range<usize>, String)> + '_ {
        self.images
            .iter()
            .map(|image| (image.range.clone(), image.data_url.clone()))
    }

    pub(crate) fn display_text(&self) -> &str {
        &self.text
    }

    pub(crate) fn from_host_content(parts: Vec<Value>) -> Result<Self, &'static str> {
        let mut text = String::new();
        let mut images = Vec::new();
        for part in parts {
            match part["type"].as_str() {
                Some("input_text") => {
                    text.push_str(part["text"].as_str().ok_or("missing input text")?)
                }
                Some("input_image") => {
                    let start = text.len();
                    text.push_str(&format!("[Image #{}]", images.len() + 1));
                    images.push((
                        start..text.len(),
                        part["image_url"]
                            .as_str()
                            .ok_or("missing image bytes")?
                            .to_owned(),
                    ));
                }
                _ => return Err("unsupported input part"),
            }
        }
        Ok(Self::multimodal(text, images))
    }

    pub(crate) fn host_content(&self) -> Vec<Value> {
        let mut content = Vec::new();
        for digest in &self.reviews {
            content.push(json!({"type":"input_review","digest":digest}));
        }
        let mut cursor = 0;
        for image in &self.images {
            if cursor < image.range.start {
                content.push(
                    json!({"type":"input_text", "text":self.text[cursor..image.range.start]}),
                );
            }
            content
                .push(json!({"type":"input_image", "image_url":image.data_url, "detail":"auto"}));
            cursor = image.range.end;
        }
        if cursor < self.text.len() {
            content.push(json!({"type":"input_text", "text":self.text[cursor..]}));
        }
        content
    }
}

impl fmt::Debug for Submission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Submission")
            .field("text", &self.text)
            .field("images", &self.images.len())
            .finish()
    }
}

impl From<String> for Submission {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

#[cfg(test)]
mod tests {
    use super::Submission;
    use serde_json::json;

    #[test]
    fn multimodal_prompt_replaces_markers_with_ordered_images() {
        let submission = Submission::multimodal(
            "before [Image #1] after".to_owned(),
            [(7..17, "data:image/png;base64,a".to_owned())],
        );
        assert_eq!(
            submission.host_content(),
            vec![
                json!({"type":"input_text","text":"before "}),
                json!({"type":"input_image","image_url":"data:image/png;base64,a","detail":"auto"}),
                json!({"type":"input_text","text":" after"}),
            ]
        );
    }

    #[test]
    fn review_attachment_is_sent_as_a_separate_input_part() {
        let digest = "0".repeat(64).parse().unwrap();
        let submission =
            Submission::text("Please address this review.".to_owned()).attach_review(digest);
        assert_eq!(submission.host_content()[0]["type"], "input_review");
        assert_eq!(submission.host_content()[0]["digest"], json!(digest));
        assert_eq!(submission.display_text(), "Please address this review.");
    }
}
