use super::*;
use crate::{
    auxiliary::{Accounting, AuxiliaryRecord, AuxiliaryReport, AuxiliaryStatus},
    submission::{SubmissionStatus, WorkIntent},
};

impl Store {
    pub fn begin_auxiliary(
        &mut self,
        session: SessionId,
        request: Uuid,
    ) -> Result<SessionState, StoreError> {
        let state = self.load_session(session)?;
        let submission = state
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown auxiliary submission"))?;
        let WorkIntent::Auxiliary { spec } = &submission.intent else {
            if let WorkIntent::Ordinary { .. } = &submission.intent {
                // Queue-claim classification records usage on the ordinary
                // submission before its effective intent is selected.
                let command = SessionCommand::AuxiliaryStarted;
                return self.session_command(session, state.revision, request, command);
            }
            return Err(StoreError::Invalid("request is not auxiliary"));
        };
        if submission.status != SubmissionStatus::Running
            || state.active_request.is_some()
            || state.operations.contains_key(&request)
        {
            return Err(StoreError::Invalid(
                "auxiliary request is cancelled, active or already admitted",
            ));
        }
        let command = if spec.visible() {
            SessionCommand::Input {
                kind: RequestKind::Auxiliary,
                content: crate::input::load(submission.input, &self.artifacts)?.messages,
            }
        } else {
            SessionCommand::AuxiliaryStarted
        };
        self.session_command(session, state.revision, request, command)
    }

    pub(crate) fn auxiliary_accounting(
        &self,
        session: SessionId,
        request: Uuid,
    ) -> Result<Accounting, StoreError> {
        let submission = self.submission(session, request)?;
        let mut accounting = Accounting::default();
        for record in submission.records {
            accounting.apply(&serde_json::from_slice(&self.artifacts.read(record)?)?)?;
        }
        Ok(accounting)
    }

    pub fn record_auxiliary(
        &mut self,
        session: SessionId,
        request: Uuid,
        operation: Uuid,
        record: AuxiliaryRecord,
    ) -> Result<(), StoreError> {
        let state = self.load_session(session)?;
        let submission = state
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown auxiliary submission"))?;
        let spec = if let WorkIntent::Auxiliary { spec } = &submission.intent {
            spec.clone()
        } else if let WorkIntent::Ordinary { .. } = &submission.intent {
            crate::auxiliary::AuxiliarySpec {
                kind: crate::auxiliary::AuxiliaryKind::Conversation,
                context: crate::auxiliary::AuxiliaryContext::CurrentConversation,
                review: None,
                limits: Default::default(),
            }
        } else {
            return Err(StoreError::Invalid("request is not auxiliary"));
        };
        let classification = matches!(
            record,
            AuxiliaryRecord::ClassificationIntended { .. }
                | AuxiliaryRecord::ClassificationObserved { .. }
        );
        if !classification
            && !matches!(submission.intent, WorkIntent::Ordinary { .. })
            && state.active_request != Some(request)
        {
            return Err(StoreError::Invalid(
                "auxiliary actor no longer owns this request",
            ));
        }
        let digest = self.artifacts.put(&serde_json::to_vec(&record)?)?;
        let command = SessionCommand::AuxiliaryRecorded {
            request,
            record: digest,
        };
        if let Some(previous) = state.operations.get(&operation) {
            return if *previous == Digest::of_value(&command)? {
                Ok(())
            } else {
                Err(StoreError::Invalid("auxiliary operation ID reused"))
            };
        }
        let mut accounting = self.auxiliary_accounting(session, request)?;
        if matches!(
            record,
            AuxiliaryRecord::ModelIntended { .. } | AuxiliaryRecord::ClassificationIntended { .. }
        ) && (accounting.calls.len() >= spec.limits.model_calls as usize
            || accounting
                .tokens()
                .is_none_or(|tokens| tokens >= spec.limits.tokens)
            || accounting
                .started_ms
                .is_none_or(|start| now_ms().saturating_sub(start) >= spec.limits.elapsed_ms))
        {
            return Err(StoreError::Budget);
        }
        if submission.records.len() >= 256 {
            return Err(StoreError::Budget);
        }
        match &record {
            AuxiliaryRecord::ModelIntended { input, .. } => {
                self.artifacts.read(*input)?;
            }
            AuxiliaryRecord::ClassificationIntended { input, .. } => {
                self.artifacts.read(*input)?;
            }
            AuxiliaryRecord::ClassificationObserved { receipt, .. } => {
                self.artifacts.read(receipt.report)?;
            }
            AuxiliaryRecord::ModelObserved { receipt, .. } => {
                self.artifacts.read(receipt.report)?;
            }
            AuxiliaryRecord::ToolObserved { input, output, .. } => {
                self.artifacts.read(*input)?;
                self.artifacts.read(*output)?;
            }
            AuxiliaryRecord::Started { source, review, .. } => {
                for digest in source.iter().chain(review.iter()) {
                    self.artifacts.read(*digest)?;
                }
            }
        }
        accounting.apply(&record)?;
        self.session_command(session, state.revision, operation, command)?;
        Ok(())
    }

    pub fn publish_auxiliary(
        &mut self,
        session: SessionId,
        request: Uuid,
        status: AuxiliaryStatus,
        text: String,
        error: Option<String>,
    ) -> Result<Digest, StoreError> {
        let state = self.load_session(session)?;
        let submission = state
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown auxiliary submission"))?;
        let WorkIntent::Auxiliary { spec } = &submission.intent else {
            return Err(StoreError::Invalid("request is not auxiliary"));
        };
        if state.active_request != Some(request) || text.len() > 256 * 1024 {
            return Err(StoreError::Invalid("invalid auxiliary result or actor"));
        }
        let accounting = self.auxiliary_accounting(session, request)?;
        if status == AuxiliaryStatus::Completed
            && (accounting.tokens().is_none()
                || accounting
                    .tokens()
                    .is_some_and(|tokens| tokens > spec.limits.tokens)
                || accounting.calls.is_empty())
        {
            return Err(StoreError::Invalid(
                "auxiliary answer has unresolved provider accounting",
            ));
        }
        let report = AuxiliaryReport {
            version: 1,
            kind: spec.kind,
            status,
            text: text.clone(),
            model_calls: accounting.calls.len(),
            tokens: accounting.tokens(),
            records: submission.records.clone(),
            error: error.clone(),
        };
        let digest = self.artifacts.put(&serde_json::to_vec(&report)?)?;
        let state = self.session_command(
            session,
            state.revision,
            Uuid::new_v5(&request, b"auxiliary-published"),
            SessionCommand::AuxiliaryPublished {
                request,
                report: digest,
                text: spec.visible().then_some(text),
            },
        )?;
        self.session_command(
            session,
            state.revision,
            Uuid::new_v5(&request, b"auxiliary-settled"),
            SessionCommand::TurnSettled {
                request,
                outcome: None,
                error,
            },
        )?;
        Ok(digest)
    }
}
