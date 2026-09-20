use super::*;
use crate::session::compaction::{
    CompactionInput, ContextError, ContextFallback, PreparedCompaction, TextReplacement,
};
use nanocodex_oai_api::{
    ImageDetail,
    responses::{FunctionOutputBody, FunctionOutputContent, ResponseItemId},
};
use std::collections::{BTreeMap, HashSet};

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend,
{
    pub(super) async fn compact_local(
        &mut self,
        call_index: u32,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
        manual: bool,
    ) -> Result<bool> {
        if !self.local_context_enabled() {
            return Ok(false);
        }
        let Some(backend) = self.context_backend.clone() else {
            return Ok(false);
        };
        let history = conversation.flattened_history();
        let prefix = factory.profile().prefix();
        let before = backend.estimate(self.model, prefix, &history)?;
        let policy = backend.policy();
        let threshold = policy.input_tokens.saturating_mul(9) / 10;
        if !manual && !self.force_compaction && before < threshold {
            return Ok(false);
        }
        let checkpoint = conversation
            .archive
            .as_ref()
            .ok_or(ContextError::InvalidArchive {
                reason: "live history has no archive identity",
            })?;
        backend.flush().await?;
        let eligible = backend.successful_results(checkpoint)?;
        let revision = conversation.managed.projection_revision();
        let started = Instant::now();
        self.events.emit(
            AgentEventKind::ModelCompactionStarted,
            serde_json::json!({
                "after_model_call_index": call_index,
                "active_context_tokens": before,
                "auto_compact_token_limit": threshold,
                "strategy": "snapcompact",
                "manual": manual,
            }),
        )?;
        self.stats.compactions += 1;
        let prepared = backend
            .prepare(CompactionInput {
                model: self.model,
                checkpoint,
                revision,
                prefix,
                history: &history,
                eligible: &eligible,
                manual,
            })
            .await;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let fallback = policy.fallback == ContextFallback::Provider
                    && matches!(
                        error,
                        ContextError::NoReduction
                            | ContextError::UnsupportedProfile
                            | ContextError::Budget { .. }
                    );
                self.events.emit(
                    AgentEventKind::ModelCompactionFailed,
                    serde_json::json!({
                        "after_model_call_index": call_index,
                        "duration_ns": elapsed_ns(started),
                        "strategy": "snapcompact",
                        "fallback": fallback.then_some("provider"),
                        "error": error.to_string(),
                    }),
                )?;
                if !fallback {
                    return Err(error.into());
                }
                let text = backend.restore_text(checkpoint, &history)?;
                let input_tokens = backend.estimate(self.model, prefix, &text)?;
                if input_tokens > policy.input_tokens {
                    return Err(ContextError::Budget {
                        reason: "formerly visible text exceeds the provider fallback budget",
                    }
                    .into());
                }
                let (item, _, included) = self
                    .perform_compaction(
                        call_index,
                        nanocodex_oai_api::responses::ResponseHistory::new(text.clone()),
                        0,
                        None,
                        input_tokens,
                        threshold,
                        factory,
                        manual,
                    )
                    .await?;
                // Prepare provider history in an isolated candidate. Failed remote calls
                // leave both the active bitmap projection and archive identity intact.
                let mut candidate = conversation.clone();
                candidate
                    .managed
                    .install_projection(revision, text, prefix, input_tokens)
                    .map_err(|_| ContextError::InvalidArchive {
                        reason: "malformed provider fallback history",
                    })?;
                candidate.install_mid_turn_compaction(
                    item,
                    developer_context(),
                    (*conversation.canonical_context).clone(),
                    prefix,
                );
                candidate.observe_server_reasoning(included);
                if let Some(archive) = &mut candidate.archive {
                    archive.manifest = None;
                }
                *conversation = candidate;
                self.force_compaction = false;
                return Ok(true);
            }
        };
        if prepared.revision != revision || conversation.managed.projection_revision() != revision {
            return Err(ContextError::StaleProjection.into());
        }
        let PreparedCompaction {
            manifest,
            replacements,
            input_tokens,
            page_count,
            ..
        } = prepared;
        let projected = apply_replacements(history, &replacements, &eligible)?;
        let measured = backend.estimate(self.model, prefix, &projected)?;
        if measured > policy.input_tokens || measured > input_tokens {
            return Err(ContextError::Budget {
                reason: "prepared projection exceeds its declared budget",
            }
            .into());
        }
        conversation
            .managed
            .install_projection(revision, projected, prefix, measured)
            .map_err(|_| ContextError::InvalidArchive {
                reason: "malformed projected tool history",
            })?;
        conversation
            .archive
            .as_mut()
            .expect("archive checked above")
            .manifest = Some(manifest.clone());
        self.force_compaction = false;
        let duration_ns = elapsed_ns(started);
        self.stats.compaction_duration_ns += duration_ns;
        self.events.emit(
            AgentEventKind::ModelCompactionCompleted,
            serde_json::json!({
                "after_model_call_index": call_index,
                "strategy": "snapcompact",
                "status": "completed",
                "duration_ns": duration_ns,
                "before_tokens": before,
                "after_tokens": measured,
                "pages": page_count,
                "manifest": manifest,
            }),
        )?;
        Ok(true)
    }
}

pub(crate) fn apply_replacements(
    mut history: Vec<ResponseItem>,
    replacements: &[TextReplacement],
    eligible: &[ResponseItemId],
) -> std::result::Result<Vec<ResponseItem>, ContextError> {
    let eligible = eligible.iter().collect::<HashSet<_>>();
    let mut by_item = BTreeMap::<ResponseItemId, BTreeMap<usize, &TextReplacement>>::new();
    for replacement in replacements {
        if !eligible.contains(&replacement.item_id) || replacement.pages.is_empty() {
            return Err(ContextError::InvalidArchive {
                reason: "projection includes an ineligible result",
            }
            .into());
        }
        if by_item
            .entry(replacement.item_id.clone())
            .or_default()
            .insert(replacement.content_index, replacement)
            .is_some()
        {
            return Err(ContextError::InvalidArchive {
                reason: "duplicate projected text block",
            }
            .into());
        }
    }
    if by_item.is_empty() {
        return Err(ContextError::NoReduction.into());
    }
    for item in &mut history {
        let Some(id) = item.id() else { continue };
        let Some(mut changes) = by_item.remove(id) else {
            continue;
        };
        let projected_id = changes
            .values()
            .next()
            .expect("nonempty changes")
            .projected_item_id
            .clone();
        let (ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. }) = item
        else {
            return Err(ContextError::InvalidArchive {
                reason: "only tool results may be projected",
            }
            .into());
        };
        let content = match output {
            FunctionOutputBody::Text(text) => {
                vec![FunctionOutputContent::InputText { text: text.clone() }]
            }
            FunctionOutputBody::Content(content) => content.clone(),
        };
        let mut projected = Vec::new();
        for (index, block) in content.into_iter().enumerate() {
            let Some(change) = changes.remove(&index) else {
                projected.push(block);
                continue;
            };
            if !matches!(block, FunctionOutputContent::InputText { .. })
                || change.projected_item_id != projected_id
            {
                return Err(ContextError::InvalidArchive {
                    reason: "projection changed nontext content or item identity",
                }
                .into());
            }
            for page in &change.pages {
                if !page.image_url.starts_with("data:image/png;base64,") {
                    return Err(ContextError::InvalidArchive {
                        reason: "archive page is not an inline PNG",
                    }
                    .into());
                }
                projected.push(FunctionOutputContent::InputText {
                    text: page.locator.clone().into_boxed_str(),
                });
                projected.push(FunctionOutputContent::InputImage {
                    image_url: page.image_url.clone().into_boxed_str(),
                    detail: Some(ImageDetail::Original),
                });
            }
        }
        if !changes.is_empty() {
            return Err(ContextError::InvalidArchive {
                reason: "projection refers to a missing content block",
            }
            .into());
        }
        *output = FunctionOutputBody::Content(projected);
        item.set_id(Some(projected_id));
    }
    if !by_item.is_empty() {
        return Err(ContextError::InvalidArchive {
            reason: "projection refers to missing history",
        }
        .into());
    }
    Ok(history)
}
