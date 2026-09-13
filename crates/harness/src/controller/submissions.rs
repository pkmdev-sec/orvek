use super::*;
use crate::submission::{
    OrdinaryKind, Schedule, Submission, SubmissionStatus, SubmitIntent, WorkIntent,
};

impl Host {
    pub async fn move_submission(
        &self,
        session: SessionId,
        operation: Uuid,
        request: Uuid,
        expected_input: crate::Digest,
        before: Option<Uuid>,
    ) -> Result<Submission, HostError> {
        Ok(self.store.lock().await.move_submission(
            session,
            operation,
            request,
            expected_input,
            before,
        )?)
    }
    pub async fn submissions(
        &self,
        session: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<crate::submission::SubmissionPage, HostError> {
        Ok(self
            .store
            .lock()
            .await
            .submissions(session, offset, limit)?)
    }

    pub async fn edit_submission(
        self: &Arc<Self>,
        session: SessionId,
        operation: Uuid,
        request: Uuid,
        expected_input: crate::Digest,
        content: Option<Vec<Value>>,
    ) -> Result<Submission, HostError> {
        let active = self.active.lock().await;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(HostError::ShuttingDown);
        }
        let mut store = self.store.lock().await;
        let input = content
            .map(|content| {
                crate::input::prepare(content, store.artifacts()).map(|input| input.artifact)
            })
            .transpose()?;
        let replay = store
            .load_session(session)?
            .operations
            .contains_key(&operation);
        let previous = store.submission(session, request)?;
        let invalidates = match previous.intent {
            WorkIntent::Continue { task, .. } => {
                input.is_none() || store.load(task)?.directive_scopes.contains_key(&request)
            }
            _ => false,
        };
        let submission =
            store.edit_submission(session, operation, request, expected_input, input)?;
        if !replay
            && invalidates
            && let Some(token) = active.get(&session)
        {
            token.cancel();
        }
        drop(store);
        drop(active);
        self.wake_queue(session).await;
        Ok(submission)
    }

    pub async fn submit(
        self: &Arc<Self>,
        session: SessionId,
        request: Uuid,
        mut content: Vec<Value>,
        intent: SubmitIntent,
    ) -> Result<Submission, HostError> {
        let active = self.active.lock().await;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(HostError::ShuttingDown);
        }
        let mut store = self.store.lock().await;
        if let SubmitIntent::Shell { spec } = &intent {
            spec.validate()?;
            if content.is_empty() {
                content.push(json!({"type":"input_text","text":format!("! {}", spec.command)}));
            }
        }
        if content.is_empty()
            && matches!(&intent, SubmitIntent::Auxiliary { spec } if spec.kind == crate::auxiliary::AuxiliaryKind::Reflection)
        {
            content.push(json!({"type":"input_text","text":"Reflect on this session."}));
        }
        let input = crate::input::prepare(content, store.artifacts())?;
        let intent = match intent {
            SubmitIntent::Shell { spec } => WorkIntent::Shell { spec },
            SubmitIntent::Auxiliary { spec } => {
                spec.validate()?;
                WorkIntent::Auxiliary { spec }
            }
            SubmitIntent::NewTask { limits, policy } => {
                policy.validate()?;
                limits.validate().map_err(StoreError::from)?;
                WorkIntent::NewTask {
                    limits,
                    policy: store
                        .artifacts()
                        .put(&serde_json::to_vec(&policy)?)
                        .map_err(StoreError::from)?,
                }
            }
            SubmitIntent::Ordinary {
                limits,
                policy,
                schedule,
            } => {
                policy.validate()?;
                limits.validate().map_err(StoreError::from)?;
                WorkIntent::Ordinary {
                    limits,
                    policy: store
                        .artifacts()
                        .put(&serde_json::to_vec(&policy)?)
                        .map_err(StoreError::from)?,
                    schedule,
                }
            }
            SubmitIntent::Continue {
                task,
                scope_revision,
                schedule,
            } => WorkIntent::Continue {
                task,
                scope_revision,
                schedule,
            },
        };
        let previous = store
            .load_session(session)?
            .submissions
            .contains_key(&request);
        let submission = store.submit(session, request, input.artifact, intent)?;
        if !previous
            && matches!(
                submission.intent,
                WorkIntent::Continue {
                    schedule: Schedule::Steer,
                    ..
                }
            )
            && let Some(cancellation) = active.get(&session)
        {
            cancellation.cancel();
        }
        drop(store);
        drop(active);
        self.wake_queue(session).await;
        Ok(submission)
    }

    pub async fn submission(
        &self,
        session: SessionId,
        request: Uuid,
    ) -> Result<Submission, HostError> {
        Ok(self.store.lock().await.submission(session, request)?)
    }

    pub async fn cancel_submission(
        &self,
        session: SessionId,
        request: Uuid,
    ) -> Result<Submission, HostError> {
        let mut store = self.store.lock().await;
        let submission = store.submission(session, request)?;
        let state = store.load_session(session)?;
        if submission.status == SubmissionStatus::Queued
            || (submission.status == SubmissionStatus::Running
                && !state.operations.contains_key(&request))
        {
            return Ok(store.set_submission_status(
                session,
                request,
                SubmissionStatus::Cancelled,
            )?);
        }
        let active_request = state.active_request;
        drop(store);
        if submission.status == SubmissionStatus::Running && active_request == Some(request) {
            self.cancel_request(session, request).await?;
        }
        self.submission(session, request).await
    }

    pub async fn cancel_request(
        &self,
        session: SessionId,
        request: Uuid,
    ) -> Result<bool, HostError> {
        let active = self.active.lock().await;
        let mut store = self.store.lock().await;
        let state = store.load_session(session)?;
        if state.active_request != Some(request) {
            return Ok(false);
        }
        if let Some(token) = active.get(&session) {
            token.cancel();
        }
        if state.kind == crate::state::RequestKind::Task
            && let Some(task) = state.tasks_by_request.get(&request)
        {
            let task = store.load(*task)?;
            if task.outcome.is_none() {
                store.request_cancellation(task.id)?;
            }
        }
        Ok(true)
    }

    pub async fn start_queued(self: &Arc<Self>) -> Result<(), HostError> {
        let mut offset = 0;
        loop {
            let sessions = self.store.lock().await.sessions(offset, 64)?;
            for session in &sessions {
                if session
                    .submissions
                    .values()
                    .any(|s| s.status == SubmissionStatus::Queued)
                {
                    self.wake_queue(session.id).await;
                }
            }
            if sessions.len() < 64 {
                break;
            }
            offset += sessions.len();
        }
        Ok(())
    }

    async fn wake_queue(self: &Arc<Self>, session: SessionId) {
        if self.queue_workers.lock().await.insert(session) {
            let host = self.clone();
            tokio::spawn(async move {
                let result = host.run_queue(session).await;
                if let Err(error) = result {
                    let mut workers = host.queue_workers.lock().await;
                    // Persist the coordinator failure on the claimed request.
                    // Pending inputs remain queryable and recoverable after restart.
                    let mut store = host.store.lock().await;
                    if let Ok(state) = store.load_session(session) {
                        for submission in state
                            .submissions
                            .values()
                            .filter(|s| s.status == SubmissionStatus::Running)
                        {
                            let _ = store.set_submission_status(
                                session,
                                submission.id,
                                SubmissionStatus::Finished {
                                    task: state.tasks_by_request.get(&submission.id).copied(),
                                    outcome: None,
                                    error: Some(error.to_string()),
                                },
                            );
                        }
                    }
                    workers.remove(&session);
                }
                host.queue_wake.notify_waiters();
            });
        }
        self.queue_wake.notify_waiters();
    }

    async fn run_queue(&self, session: SessionId) -> Result<(), HostError> {
        loop {
            let available = self.queue_wake.notified();
            tokio::pin!(available);
            available.as_mut().enable();
            let submission = {
                let mut workers = self.queue_workers.lock().await;
                let active = self.active.lock().await;
                if active.contains_key(&session) {
                    drop(active);
                    drop(workers);
                    tokio::select! { () = self.queue_stop.cancelled() => { self.queue_workers.lock().await.remove(&session); return Ok(()); }, () = &mut available => {} }
                    continue;
                }
                let mut store = self.store.lock().await;
                let Some(next) = store.next_submission(session)? else {
                    workers.remove(&session);
                    return Ok(());
                };
                if self.queue_stop.is_cancelled() {
                    workers.remove(&session);
                    return Ok(());
                }
                store.set_submission_status(session, next.id, SubmissionStatus::Running)?
            };
            let result = async {
                let admission = match &submission.intent {
                    WorkIntent::Shell { spec } => {
                        return self
                            .execute_shell(session, submission.id, spec.clone())
                            .await;
                    }
                    WorkIntent::Auxiliary { spec } => {
                        return self
                            .execute_auxiliary(session, submission.id, spec.clone())
                            .await;
                    }
                    WorkIntent::NewTask { limits, policy } => {
                        let store = self.store.lock().await;
                        TaskRequest::DiscoverInput {
                            input: crate::input::load(submission.input, store.artifacts())?,
                            limits: *limits,
                            intake: *policy,
                        }
                    }
                    WorkIntent::Continue { .. } => TaskRequest::Continue {
                        request: submission.id,
                    },
                    WorkIntent::Ordinary { limits, policy, .. } => {
                        let state = self.store.lock().await.load_session(session)?;
                        let kind = match self
                            .classify_ordinary(session, submission.id, &submission, *limits)
                            .await
                        {
                            Ok(kind) => kind,
                            Err(error) => {
                                // A classification that fails closed still has
                                // to settle the turn. Without this the session
                                // keeps `active_request` set and rejects every
                                // later submission until the host restarts.
                                self.store
                                    .lock()
                                    .await
                                    .settle_classification(session, submission.id)?;
                                return Err(error);
                            }
                        };
                        // Only an unfinished task may be continued by a
                        // classifier decision. Reopening a task that already
                        // completed is an explicit user action, not something
                        // an ordinary action should infer.
                        let incomplete_task = match state.current_task {
                            Some(task) => {
                                self.store.lock().await.load(task)?.outcome
                                    != Some(crate::state::Outcome::Complete)
                            }
                            None => false,
                        };
                        match kind {
                            OrdinaryKind::Information => {
                                let spec = crate::auxiliary::ordinary_conversation_spec();
                                self.store
                                    .lock()
                                    .await
                                    .begin_auxiliary(session, submission.id)?;
                                // Keep the request active across classification and answer;
                                // publication is the single settle boundary.
                                let state = self.store.lock().await.load_session(session)?;
                                let cancellation = self.queue_stop.child_token();
                                // Publication is the only settle boundary, so a
                                // failed answer still has to reach it. Returning
                                // early here would leave `active_request` set and
                                // wedge the session until the host restarts.
                                let (status, text, error) = match self
                                    .run_auxiliary(
                                        state,
                                        submission.id,
                                        &spec,
                                        cancellation.clone(),
                                    )
                                    .await
                                {
                                    Ok(result) => result,
                                    Err(error) => (
                                        if cancellation.is_cancelled() {
                                            crate::auxiliary::AuxiliaryStatus::Cancelled
                                        } else if matches!(
                                            error,
                                            HostError::Store(StoreError::Budget)
                                        ) {
                                            crate::auxiliary::AuxiliaryStatus::BudgetExhausted
                                        } else {
                                            crate::auxiliary::AuxiliaryStatus::Failed
                                        },
                                        String::new(),
                                        Some(error.to_string()),
                                    ),
                                };
                                self.store.lock().await.publish_auxiliary(
                                    session,
                                    submission.id,
                                    status,
                                    text,
                                    error.clone(),
                                )?;
                                return Ok(SubmissionStatus::Finished {
                                    task: None,
                                    outcome: None,
                                    error,
                                });
                            }
                            OrdinaryKind::Action if incomplete_task => TaskRequest::Continue {
                                request: submission.id,
                            },
                            OrdinaryKind::Action => {
                                let store = self.store.lock().await;
                                let input =
                                    crate::input::load(submission.input, store.artifacts())?;
                                TaskRequest::DiscoverInput {
                                    input,
                                    limits: *limits,
                                    intake: *policy,
                                }
                            }
                        }
                    }
                };
                let run = self
                    .execute_task_request(
                        session,
                        submission.id,
                        admission,
                        self.queue_stop.child_token(),
                        Arc::new(|_| {}),
                    )
                    .await?;
                Ok(SubmissionStatus::Finished {
                    task: Some(run.task.id),
                    outcome: run.task.outcome,
                    error: run.task.disposition_reason,
                })
            }
            .await;
            let mut store = self.store.lock().await;
            let current = store.submission(session, submission.id)?;
            if current.status == SubmissionStatus::Cancelled {
                continue;
            }
            let status = match result {
                Err(HostError::Busy) => {
                    store.set_submission_status(
                        session,
                        submission.id,
                        SubmissionStatus::Queued,
                    )?;
                    drop(store);
                    tokio::select! { () = self.queue_stop.cancelled() => return Ok(()), () = &mut available => {} }
                    continue;
                }
                Ok(status) => status,
                Err(error) => {
                    let state = store.load_session(session)?;
                    SubmissionStatus::Finished {
                        task: state.tasks_by_request.get(&submission.id).copied(),
                        outcome: None,
                        error: Some(error.to_string()),
                    }
                }
            };
            store.set_submission_status(session, submission.id, status)?;
        }
    }
}
