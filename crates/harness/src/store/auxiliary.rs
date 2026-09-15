use super::*;
use crate::{
    auxiliary::{
        Accounting, AuxiliaryRecord, AuxiliaryReport, AuxiliaryStatus, ordinary_conversation_spec,
    },
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
                if submission.status != SubmissionStatus::Running
                    || state.active_request != Some(request)
                    || self.ordinary_classification(session, request)?.is_none()
                {
                    return Err(StoreError::Invalid(
                        "ordinary request has no completed information classification",
                    ));
                }
                return Ok(state);
            }
            return Err(StoreError::Invalid("request is not auxiliary"));
        };
        if submission.status != SubmissionStatus::Running
            || state.active_request.is_some()
            || state.operations.contains_key(&request)
            || self.ordinary_classification(session, request)?.is_some()
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

    pub fn begin_classification(
        &mut self,
        session: SessionId,
        request: Uuid,
    ) -> Result<(), StoreError> {
        let state = self.load_session(session)?;
        let submission = state
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown ordinary submission"))?;
        if !matches!(submission.intent, WorkIntent::Ordinary { .. })
            || submission.status != SubmissionStatus::Running
            || state.active_request.is_some()
            || state.operations.contains_key(&request)
        {
            return Err(StoreError::Invalid(
                "ordinary classification is cancelled, active or already admitted",
            ));
        }
        self.session_command(
            session,
            state.revision,
            request,
            SessionCommand::AuxiliaryStarted,
        )?;
        Ok(())
    }

    pub fn settle_failed_classification(
        &mut self,
        session: SessionId,
        request: Uuid,
        error: String,
    ) -> Result<(), StoreError> {
        let state = self.load_session(session)?;
        if state.active_request != Some(request) {
            return Ok(());
        }
        self.session_command(
            session,
            state.revision,
            Uuid::new_v5(&request, b"classification-settled"),
            SessionCommand::TurnSettled {
                request,
                outcome: None,
                error: Some(error),
            },
        )?;
        Ok(())
    }

    pub(crate) fn ordinary_classification(
        &self,
        session: SessionId,
        request: Uuid,
    ) -> Result<Option<(OrdinaryKind, Uuid, ModelCallReceipt, u64)>, StoreError> {
        let submission = self.submission(session, request)?;
        // Only an ordinary request can carry a classification; every other
        // intent legitimately has none. Report that rather than failing:
        // `begin_auxiliary` asks this about genuine auxiliary submissions, and
        // the Continue/DiscoverInput task paths ask it about plain follow-ups.
        if !matches!(submission.intent, WorkIntent::Ordinary { .. }) {
            return Ok(None);
        }
        let accounting = self.auxiliary_accounting(session, request)?;
        if accounting.calls.is_empty() {
            return Ok(None);
        }
        let mut observed = None;
        for record in submission.records {
            if let AuxiliaryRecord::ClassificationObserved {
                call,
                receipt,
                kind,
            } = serde_json::from_slice(&self.artifacts.read(record)?)?
                && receipt.tokens.is_some()
                && receipt.status == ModelCallStatus::Completed
            {
                let parsed = match kind.as_str() {
                    "information" => OrdinaryKind::Information,
                    "action" => OrdinaryKind::Action,
                    _ => continue,
                };
                if observed.replace((parsed, call, receipt)).is_some() {
                    return Err(StoreError::Invalid("multiple ordinary classifications"));
                }
            }
        }
        Ok(observed.map(|(kind, call, receipt)| {
            let started_ms = accounting
                .started_ms
                .expect("an observed classification records a start");
            (kind, call, receipt, started_ms)
        }))
    }

    pub fn account_ordinary_classification(
        &mut self,
        task: TaskId,
        request: Uuid,
        kind: OrdinaryKind,
        call: Uuid,
        receipt: ModelCallReceipt,
        started_ms: u64,
    ) -> Result<TaskState, StoreError> {
        if kind != OrdinaryKind::Action || call == request || started_ms == 0 {
            return Err(StoreError::Invalid(
                "ordinary classifier cannot be charged to a task",
            ));
        }
        self.reserve_model_call(task, Uuid::new_v5(&request, b"ordinary-classifier"))?;
        self.record_model_call(
            task,
            Uuid::new_v5(&request, b"ordinary-classifier"),
            receipt,
        )
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
            ordinary_conversation_spec()
        } else {
            return Err(StoreError::Invalid("request is not auxiliary"));
        };
        let classification = matches!(
            record,
            AuxiliaryRecord::ClassificationIntended { .. }
                | AuxiliaryRecord::ClassificationObserved { .. }
        );
        if !classification
            && (state.active_request != Some(request)
                || !matches!(submission.intent, WorkIntent::Ordinary { .. }))
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
        let new_call = matches!(
            record,
            AuxiliaryRecord::ModelIntended { .. } | AuxiliaryRecord::ClassificationIntended { .. }
        );
        let call_is_pending = match &record {
            AuxiliaryRecord::ModelIntended { call, .. }
            | AuxiliaryRecord::ClassificationIntended { call, .. } => {
                !accounting.calls.contains_key(call)
            }
            _ => false,
        };
        let pending_receipt = accounting.has_pending_receipt() && call_is_pending;
        if new_call
            && (accounting.calls.len() >= spec.limits.model_calls as usize
                || pending_receipt
                || accounting
                    .known_tokens()
                    .is_some_and(|tokens| tokens >= spec.limits.tokens)
                || accounting
                    .started_ms
                    .is_some_and(|start| now_ms().saturating_sub(start) >= spec.limits.elapsed_ms))
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
        let ordinary = {
            let state = self.load_session(session)?;
            let submission = state
                .submissions
                .get(&request)
                .ok_or(StoreError::Invalid("unknown auxiliary submission"))?;
            if let WorkIntent::Ordinary { .. } = &submission.intent {
                if state.active_request != Some(request)
                    || self.ordinary_classification(session, request)?.is_none()
                {
                    return Err(StoreError::Invalid("ordinary auxiliary actor mismatch"));
                }
                true
            } else {
                false
            }
        };
        if ordinary {
            let spec = ordinary_conversation_spec();
            let (state, records) = {
                let state = self.load_session(session)?;
                (state.revision, self.submission(session, request)?.records)
            };
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
                records: records.clone(),
                error: error.clone(),
            };
            let digest = self.artifacts.put(&serde_json::to_vec(&report)?)?;
            self.session_command(
                session,
                state,
                Uuid::new_v5(&request, b"auxiliary-published"),
                SessionCommand::AuxiliaryPublished {
                    request,
                    report: digest,
                    text: spec.visible().then_some(text),
                },
            )?;
            let state = self.load_session(session)?;
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
            return Ok(digest);
        }
        let state = self.load_session(session)?;
        let submission = state
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown auxiliary submission"))?;
        let WorkIntent::Auxiliary { spec } = &submission.intent else {
            if let WorkIntent::Ordinary { .. } = &submission.intent {
                if state.active_request != Some(request) {
                    return Err(StoreError::Invalid("ordinary auxiliary actor mismatch"));
                }
            } else {
                return Err(StoreError::Invalid("request is not auxiliary"));
            }
            let _ = ordinary;
            return Err(StoreError::Invalid(
                "ordinary auxiliary publication must use its dedicated path",
            ));
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
