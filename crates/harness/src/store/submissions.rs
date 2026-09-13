use super::*;
use crate::submission::{Schedule, Submission, SubmissionStatus, WorkIntent};

impl Store {
    pub fn edit_submission(
        &mut self,
        session_id: SessionId,
        operation: Uuid,
        request: Uuid,
        expected_input: Digest,
        input: Option<Digest>,
    ) -> Result<Submission, StoreError> {
        let command = SessionCommand::QueueEdited {
            request,
            expected_input,
            input,
        };
        let fingerprint = Digest::of_value(&command)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut session, mut head) = load_session_state(&transaction, session_id, None)?;
        if let Some(previous) = session.operations.get(&operation) {
            if previous != &fingerprint {
                return Err(StoreError::Invalid(
                    "queue edit operation ID reused with different input",
                ));
            }
            return session
                .submissions
                .get(&request)
                .cloned()
                .ok_or(StoreError::Invalid("unknown submission"));
        }
        let submission = session
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown submission"))?;
        if submission.status != SubmissionStatus::Queued || submission.input != expected_input {
            return Err(StoreError::Invalid(
                "queued input was changed or claimed before the edit",
            ));
        }
        let replacement = input.unwrap_or(expected_input);
        let prepared = crate::input::load(replacement, &self.artifacts)?;
        match &submission.intent {
            WorkIntent::Shell { spec } => {
                let mut spec = spec.clone();
                spec.command = prepared
                    .text
                    .strip_prefix("! ")
                    .ok_or(StoreError::Invalid(
                        "shell edit must keep its command prefix",
                    ))?
                    .to_owned();
                spec.validate()?;
            }
            WorkIntent::NewTask { .. } if prepared.text.trim().is_empty() => {
                return Err(StoreError::Invalid(
                    "new coding tasks require a textual objective",
                ));
            }
            WorkIntent::Continue { task: id, .. } => {
                let id = *id;
                if session.current_task != Some(id) {
                    return Err(StoreError::Invalid(
                        "continuation no longer targets the current task",
                    ));
                }
                let (mut task, task_head) = load_state(&transaction, id)?;
                let registered = task.directive_scopes.get(&request);
                if registered.is_some_and(|scope| *scope <= task.admitted_scope_revision) {
                    return Err(StoreError::Invalid(
                        "this input has already entered the accepted contract; send a new follow-up",
                    ));
                }
                if registered.is_some() || input.is_none() {
                    let event = if registered.is_some() {
                        TaskEvent::DirectiveReplaced {
                            request,
                            input: replacement,
                        }
                    } else {
                        TaskEvent::DirectiveReceived {
                            request,
                            input: replacement,
                        }
                    };
                    append_task_event(&transaction, &mut task, task_head, event, &self.artifacts)?;
                    transaction.execute("DELETE FROM leases WHERE task=?1", [id.to_string()])?;
                }
            }
            _ => {}
        }
        append_session_command(&transaction, &mut session, &mut head, operation, command)?;
        transaction.commit()?;
        Ok(session.submissions[&request].clone())
    }

    pub fn submissions(
        &self,
        session: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<crate::submission::SubmissionPage, StoreError> {
        if limit == 0 || limit > 64 {
            return Err(StoreError::Invalid(
                "submission pages must contain 1..64 records",
            ));
        }
        let state = self.load_session(session)?;
        let submissions = state
            .queue_order
            .iter()
            .filter_map(|id| state.submissions.get(id))
            .filter(|s| s.status.pending())
            .cloned()
            .collect::<Vec<_>>();
        let total = submissions.len();
        if offset > total {
            return Err(StoreError::Invalid(
                "submission offset exceeds pending count",
            ));
        }
        let submissions = submissions
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let end = offset + submissions.len();
        Ok(crate::submission::SubmissionPage {
            submissions,
            total,
            next: (end < total).then_some(end),
            journal_sequence: self.journal_head()?,
        })
    }

    pub fn submit(
        &mut self,
        session_id: SessionId,
        id: Uuid,
        input: Digest,
        intent: WorkIntent,
    ) -> Result<Submission, StoreError> {
        if !self.load_session(session_id)?.submissions.contains_key(&id)
            && self.pending_submissions()? >= 128
        {
            return Err(StoreError::Invalid("host queue capacity reached"));
        }
        let prepared = crate::input::load(input, &self.artifacts)?;
        if let WorkIntent::Shell { spec } = &intent {
            spec.validate()?;
            if prepared.text != format!("! {}", spec.command) {
                return Err(StoreError::Invalid("shell input differs from its command"));
            }
        }
        if let WorkIntent::Auxiliary { spec } = &intent {
            spec.validate()?;
        }
        if let WorkIntent::NewTask { limits, policy } = &intent {
            limits.validate()?;
            let policy: crate::admission::RequestPolicy =
                serde_json::from_slice(&self.artifacts.read(*policy)?)?;
            policy.validate()?;
            if prepared.text.trim().is_empty() {
                return Err(StoreError::Invalid(
                    "new coding tasks require a textual objective",
                ));
            }
        }
        if let WorkIntent::Ordinary { limits, policy, .. } = &intent {
            limits.validate()?;
            let policy: crate::admission::RequestPolicy =
                serde_json::from_slice(&self.artifacts.read(*policy)?)?;
            policy.validate()?;
            if prepared.text.trim().is_empty() {
                return Err(StoreError::Invalid("ordinary input requires text"));
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut session, mut head) = load_session_state(&transaction, session_id, None)?;
        if let Some(previous) = session.submissions.get(&id) {
            return if previous.initial_input == input && previous.intent == intent {
                Ok(previous.clone())
            } else {
                Err(StoreError::Invalid(
                    "submission ID reused with different input or intent",
                ))
            };
        }
        if session.operations.contains_key(&id) {
            return Err(StoreError::Invalid(
                "submission ID already belongs to another operation",
            ));
        }
        if session
            .submissions
            .values()
            .filter(|s| s.status.pending())
            .count()
            >= 16
        {
            return Err(StoreError::Invalid("session queue capacity reached"));
        }
        if let WorkIntent::Continue {
            task: task_id,
            scope_revision,
            schedule,
        } = &intent
        {
            if session.current_task != Some(*task_id) {
                return Err(StoreError::Invalid(
                    "continuation must name the session's current task",
                ));
            }
            let (mut task, task_head) = load_state(&transaction, *task_id)?;
            if task.scope_revision != *scope_revision {
                return Err(StoreError::Revision {
                    expected: *scope_revision,
                    actual: task.scope_revision,
                });
            }
            if task.directives.len() >= 128 {
                return Err(StoreError::Invalid("task follow-up limit reached"));
            }
            if *schedule == Schedule::Steer {
                append_task_event(
                    &transaction,
                    &mut task,
                    task_head,
                    TaskEvent::DirectiveReceived { request: id, input },
                    &self.artifacts,
                )?;
                transaction.execute("DELETE FROM leases WHERE task=?1", [task_id.to_string()])?;
            }
        }
        let submission = Submission {
            manual_job: None,
            id,
            input,
            initial_input: input,
            records: Vec::new(),
            result: None,
            intent,
            status: SubmissionStatus::Queued,
            submitted_revision: session.revision + 1,
            submitted_ms: now_ms(),
        };
        append_session_command(
            &transaction,
            &mut session,
            &mut head,
            Uuid::new_v5(&id, b"submitted"),
            SessionCommand::Submitted(Box::new(submission.clone())),
        )?;
        transaction.commit()?;
        Ok(submission)
    }

    pub fn submission(&self, session: SessionId, request: Uuid) -> Result<Submission, StoreError> {
        self.load_session(session)?
            .submissions
            .get(&request)
            .cloned()
            .ok_or(StoreError::Invalid("submission has not been accepted"))
    }

    pub fn next_submission(&self, session: SessionId) -> Result<Option<Submission>, StoreError> {
        let state = self.load_session(session)?;
        Ok(state
            .queue_order
            .iter()
            .filter_map(|id| state.submissions.get(id))
            .find(|s| s.status == SubmissionStatus::Queued)
            .cloned())
    }

    pub fn move_submission(
        &mut self,
        session: SessionId,
        operation: Uuid,
        request: Uuid,
        expected_input: Digest,
        before: Option<Uuid>,
    ) -> Result<Submission, StoreError> {
        let command = SessionCommand::QueueMoved {
            request,
            expected_input,
            before,
        };
        let state = self.load_session(session)?;
        let current = state
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown submission"))?;
        if let Some(previous) = state.operations.get(&operation) {
            return if *previous == Digest::of_value(&command)? {
                Ok(current.clone())
            } else {
                Err(StoreError::Invalid("queue move operation ID reused"))
            };
        }
        if current.status != SubmissionStatus::Queued || current.input != expected_input {
            return Err(StoreError::Invalid(
                "queued input was changed or claimed before the move",
            ));
        }
        if before.is_some_and(|id| {
            id == request
                || !state
                    .submissions
                    .get(&id)
                    .is_some_and(|s| s.status == SubmissionStatus::Queued)
        }) {
            return Err(StoreError::Invalid("queue neighbor is no longer pending"));
        }
        self.session_command(session, state.revision, operation, command)?;
        self.submission(session, request)
    }

    pub fn set_submission_status(
        &mut self,
        session: SessionId,
        request: Uuid,
        status: SubmissionStatus,
    ) -> Result<Submission, StoreError> {
        let current = self.load_session(session)?;
        let previous = current
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown submission"))?;
        if previous.status == status {
            return Ok(previous.clone());
        }
        let allowed = matches!(
            (&previous.status, &status),
            (
                SubmissionStatus::Queued,
                SubmissionStatus::Running | SubmissionStatus::Cancelled
            ) | (
                SubmissionStatus::Running,
                SubmissionStatus::Queued
                    | SubmissionStatus::Finished { .. }
                    | SubmissionStatus::Interrupted
            )
        ) || (previous.status == SubmissionStatus::Running
            && status == SubmissionStatus::Cancelled
            && !current.operations.contains_key(&request));
        if !allowed {
            return Err(StoreError::Invalid("invalid submission status transition"));
        }
        self.session_command(
            session,
            current.revision,
            Uuid::new_v4(),
            SessionCommand::SubmissionChanged { request, status },
        )?;
        self.submission(session, request)
    }

    pub fn pending_submissions(&self) -> Result<usize, StoreError> {
        let mut count = 0;
        let mut offset = 0;
        loop {
            let sessions = self.sessions(offset, 64)?;
            count += sessions
                .iter()
                .map(|session| {
                    session
                        .submissions
                        .values()
                        .filter(|s| s.status.pending())
                        .count()
                })
                .sum::<usize>();
            if sessions.len() < 64 {
                return Ok(count);
            }
            offset += sessions.len();
        }
    }

    pub fn recover_submissions(&mut self) -> Result<(), StoreError> {
        let mut offset = 0;
        loop {
            let sessions = self.sessions(offset, 64)?;
            for session in &sessions {
                for submission in session
                    .submissions
                    .values()
                    .filter(|s| s.status == SubmissionStatus::Running)
                {
                    // Admission is a transaction. No Input operation means execution
                    // never began; otherwise retain uncertainty instead of replaying it.
                    let status = if session.operations.contains_key(&submission.id) {
                        SubmissionStatus::Interrupted
                    } else {
                        SubmissionStatus::Queued
                    };
                    self.set_submission_status(session.id, submission.id, status)?;
                }
            }
            if sessions.len() < 64 {
                break;
            }
            offset += sessions.len();
        }
        Ok(())
    }

    pub fn continue_submission(
        &mut self,
        session_id: SessionId,
        request: Uuid,
    ) -> Result<(SessionState, TaskState, bool), StoreError> {
        let submission = self.submission(session_id, request)?;
        let id = if let WorkIntent::Continue { task: id, .. } = submission.intent {
            id
        } else if let WorkIntent::Ordinary { .. } = submission.intent {
            let state = self.load_session(session_id)?;
            state.current_task.ok_or(StoreError::Invalid(
                "ordinary action has no current task to continue",
            ))?
        } else {
            return Err(StoreError::Invalid("submission is not a continuation"));
        };
        let prepared = crate::input::load(submission.input, &self.artifacts)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut session, mut head) = load_session_state(&transaction, session_id, None)?;
        if session.branch.pending_shell.is_some() {
            return Err(StoreError::Invalid(
                "reconcile the pending shell before continuing",
            ));
        }
        let (mut task, mut task_head) = load_state(&transaction, id)?;
        if session.tasks_by_request.get(&request) == Some(&id) {
            return Ok((session, task, false));
        }
        if submission.status != SubmissionStatus::Running {
            return Err(StoreError::Invalid(
                "continuation is not claimed for execution",
            ));
        }
        if session.active_request.is_some() {
            return Err(StoreError::Invalid("session already has an active request"));
        }
        if session.current_task != Some(id) {
            return Err(StoreError::Invalid(
                "queued continuation was superseded by another task",
            ));
        }
        if task.jobs.values().any(|job| job.status.unresolved())
            || task.effects.values().any(|effect| {
                matches!(
                    effect.status,
                    EffectStatus::Intended | EffectStatus::Unknown
                )
            })
            || task.model_reservations.iter().any(|call| {
                !task.model_receipts.get(call).is_some_and(|receipt| {
                    receipt.tokens.is_some() && receipt.status != ModelCallStatus::Unknown
                })
            })
        {
            return Err(StoreError::Invalid(
                "reconcile unfinished jobs, effects and provider attempts before continuing",
            ));
        }
        task.cancellation_requested = false;
        check_budget(&task)?;
        if !task.directive_scopes.contains_key(&request) {
            append_task_event(
                &transaction,
                &mut task,
                task_head,
                TaskEvent::DirectiveReceived {
                    request,
                    input: submission.input,
                },
                &self.artifacts,
            )?;
            task_head = load_state(&transaction, id)?.1;
            transaction.execute("DELETE FROM leases WHERE task=?1", [id.to_string()])?;
        }
        append_task_event(
            &transaction,
            &mut task,
            task_head,
            TaskEvent::Reopened {
                reason: format!("user continuation {request}"),
            },
            &self.artifacts,
        )?;
        append_session_command(
            &transaction,
            &mut session,
            &mut head,
            request,
            SessionCommand::Input {
                kind: RequestKind::Task,
                content: prepared.messages,
            },
        )?;
        append_session_command(
            &transaction,
            &mut session,
            &mut head,
            Uuid::new_v5(&request, b"task-link"),
            SessionCommand::TaskLinked { request, task: id },
        )?;
        transaction.commit()?;
        Ok((session, task, true))
    }

    pub fn accept_additive_contract(
        &mut self,
        id: TaskId,
        revision: u64,
        contract: Contract,
        receipt: Digest,
    ) -> Result<TaskState, StoreError> {
        contract.validate()?;
        self.change(id, Some(revision), false, |state, _, artifacts| {
            if !state.amendment_pending {
                return Err(StoreError::Invalid("there is no pending user follow-up"));
            }
            if let Some(old) = &state.contract {
                crate::admission::preserves_obligations(old, &contract)?;
            } else if contract.request != state.request || contract.limits != state.initial_limits {
                return Err(StoreError::Invalid(
                    "follow-up changed original objective or limits",
                ));
            }
            artifacts.read(receipt)?;
            Ok(TaskEvent::AdditiveContractAccepted {
                contract,
                receipt,
                scope_revision: state.scope_revision,
            })
        })
    }
}
