//! Candidate lessons are reference data, never policy promotion or test execution.
use crate::{
    MemoryError, MemoryKind, MemoryMetadata, MemoryRecord, MemoryStore, ProposalState,
    TraceReference, normalize_identity,
};

/// Merge a short repeated candidate, preserving every distinct citation and producing run.
pub async fn propose_lesson<S: MemoryStore>(
    store: &S,
    content: &str,
    mut metadata: MemoryMetadata,
) -> Result<MemoryRecord, MemoryError> {
    let records = store.list().await?;
    let existing = records.into_iter().find(|record| {
        record.metadata.scope == metadata.scope
            && matches!(record.metadata.kind, MemoryKind::LessonProposal { .. })
            && normalize_identity(&record.content) == normalize_identity(content)
    });
    let key = if let Some(record) = existing {
        for evidence in record.metadata.evidence {
            if !metadata.evidence.contains(&evidence) {
                metadata.evidence.push(evidence);
            }
        }
        if let MemoryKind::LessonProposal { behavior_test, .. } = record.metadata.kind
            && !metadata.evidence.contains(&behavior_test)
        {
            metadata.evidence.push(behavior_test);
        }
        for trace in record.metadata.producing_traces {
            if !metadata.producing_traces.contains(&trace) {
                metadata.producing_traces.push(trace);
            }
        }
        metadata.imported_from = record.metadata.imported_from;
        Some(record.key)
    } else {
        None
    };
    store.put_with_metadata(content, &metadata, key).await
}

/// Runs after settlement, without a provider call or changes to task verification.
/// A failed/interrupted transaction leaves the candidate pending, not partly promoted.
pub async fn finalize_lessons<S: MemoryStore>(
    store: &S,
    trace: &TraceReference,
) -> Result<usize, MemoryError> {
    let mut count = 0;
    for record in store.list().await? {
        let mut metadata = record.metadata;
        if !metadata.producing_traces.contains(trace) {
            continue;
        }
        if let MemoryKind::LessonProposal { state, .. } = &mut metadata.kind
            && *state == ProposalState::Pending
        {
            *state = ProposalState::Proposed;
            store
                .put_with_metadata(&record.content, &metadata, Some(record.key))
                .await?;
            count += 1;
        }
    }
    Ok(count)
}
