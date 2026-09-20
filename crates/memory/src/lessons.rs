//! Candidate lessons are reference data, never policy promotion or test execution.
use crate::{
    LessonQuery, MemoryError, MemoryKind, MemoryMetadata, MemoryRecord, MemoryStore, ProposalState,
    TraceReference, normalize_identity,
};

/// Merge within writer ownership. Current evidence replaces active citations; history survives.
pub async fn propose_lesson<S: MemoryStore>(
    store: &S,
    content: &str,
    mut metadata: MemoryMetadata,
) -> Result<MemoryRecord, MemoryError> {
    if !matches!(
        metadata.kind,
        MemoryKind::LessonProposal {
            state: ProposalState::Pending,
            ..
        }
    ) || metadata.producing_traces.len() != 1
    {
        return Err(MemoryError::InvalidMetadata);
    }
    metadata.pending_run = metadata.producing_traces.first().cloned();
    let query = LessonQuery::Matching {
        scope: metadata.scope.clone(),
        content_identity: normalize_identity(content),
    };
    let records = store.lesson_page(&query, 0).await?;
    validate_page(&query, 0, &records)?;
    let key = if let Some(record) = records.into_iter().next() {
        metadata.historical_evidence = record.metadata.historical_evidence;
        for evidence in record
            .metadata
            .evidence
            .into_iter()
            .chain(match record.metadata.kind {
                MemoryKind::LessonProposal { behavior_test, .. } => Some(behavior_test),
                _ => None,
            })
        {
            if !metadata.historical_evidence.contains(&evidence) {
                metadata.historical_evidence.push(evidence);
            }
        }
        for trace in record.metadata.producing_traces {
            if !metadata.producing_traces.contains(&trace) {
                metadata.producing_traces.push(trace);
            }
        }
        metadata.imported_from = record.metadata.imported_from;
        metadata.transferred_from = record.metadata.transferred_from;
        metadata.ownership_id = record.metadata.ownership_id;
        Some(record.key)
    } else {
        None
    };
    store.put_with_metadata(content, &metadata, key).await
}

/// Finalize only the pending run and the exact version observed. A newer nomination wins CAS.
pub async fn finalize_lessons<S: MemoryStore>(
    store: &S,
    trace: &TraceReference,
) -> Result<usize, MemoryError> {
    let query = LessonQuery::Pending {
        trace: trace.clone(),
    };
    let mut count = 0;
    let mut after = 0;
    loop {
        let records = store.lesson_page(&query, after).await?;
        validate_page(&query, after, &records)?;
        if records.is_empty() {
            return Ok(count);
        }
        for record in records {
            after = record.key.id;
            let mut metadata = record.metadata;
            if let MemoryKind::LessonProposal { state, .. } = &mut metadata.kind {
                *state = ProposalState::Proposed;
                metadata.pending_run = None;
                match store
                    .put_with_metadata(&record.content, &metadata, Some(record.key))
                    .await
                {
                    Ok(_) => count += 1,
                    Err(MemoryError::Conflict | MemoryError::NotFound) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

fn validate_page(
    query: &LessonQuery,
    mut after: i64,
    records: &[MemoryRecord],
) -> Result<(), MemoryError> {
    if records.len() > crate::server::protocol::MAX_EXPORT_PAGE_RECORDS {
        return Err(MemoryError::InvalidPagination);
    }
    for record in records {
        if record.key.id <= after || !query.matches(record) {
            return Err(MemoryError::InvalidPagination);
        }
        after = record.key.id;
    }
    Ok(())
}
