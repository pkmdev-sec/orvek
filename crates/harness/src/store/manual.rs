use super::*;
use crate::{
    manual::{ManualJob, ShellReport},
    session::WorkspaceSeed,
    submission::{SubmissionStatus, WorkIntent},
    workspace::Snapshot,
};

impl Store {
    pub fn begin_shell(
        &mut self,
        session_id: SessionId,
        request: Uuid,
        job: ManualJob,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut session, mut head) = load_session_state(&transaction, session_id, None)?;
        let submission = session
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown shell submission"))?;
        let WorkIntent::Shell { spec } = &submission.intent else {
            return Err(StoreError::Invalid("request is not a user shell command"));
        };
        spec.validate()?;
        if submission.status != SubmissionStatus::Running
            || session.active_request.is_some()
            || session.operations.contains_key(&request)
            || session.branch.pending_task.is_some()
            || session.branch.pending_shell.is_some()
        {
            return Err(StoreError::Invalid(
                "shell source is active, unresolved or already claimed",
            ));
        }
        if job.task != session.current_task
            || spec.expected_task.is_some_and(|id| job.task != Some(id))
        {
            return Err(StoreError::Invalid("shell target task changed"));
        }
        for digest in [job.before, job.origin] {
            Snapshot::load(digest, &self.artifacts)
                .and_then(|snapshot| snapshot.verify_artifacts(&self.artifacts))
                .map_err(|_| StoreError::Integrity("shell input snapshot is unavailable"))?;
        }
        self.artifacts.read(job.environment)?;
        if let Some(id) = job.task {
            let (mut task, task_head) = load_state(&transaction, id)?;
            if spec
                .scope_revision
                .is_some_and(|scope| scope != task.scope_revision)
                || job.scope_revision != Some(task.scope_revision + 1)
            {
                return Err(StoreError::Invalid("shell task scope changed"));
            }
            if task.jobs.values().any(|job| job.status.unresolved())
                || task.effects.values().any(|effect| {
                    matches!(
                        effect.status,
                        EffectStatus::Intended | EffectStatus::Unknown
                    )
                })
            {
                return Err(StoreError::Invalid(
                    "reconcile unfinished task operations before user shell execution",
                ));
            }
            let record = Job {
                id: job.job,
                generation: task.generation + 1,
                status: JobStatus::Running,
                mutates_candidate: true,
                check: None,
                identity: None,
                started_ms: job.started_ms,
                deadline_ms: job.started_ms.saturating_add(spec.timeout_ms),
                invocation: Some(JobInvocation {
                    session: session_id,
                    request,
                    call_id: None,
                    capability: "human_shell".into(),
                    input: submission.input,
                    environment: job.environment,
                }),
                fence_receipt: None,
                execution_receipt: None,
            };
            append_task_event(
                &transaction,
                &mut task,
                task_head,
                TaskEvent::ManualStarted { job: record },
                &self.artifacts,
            )?;
            let task_head = load_state(&transaction, id)?.1;
            append_task_event(
                &transaction,
                &mut task,
                task_head,
                TaskEvent::EffectRecorded(Effect {
                    operation_id: request,
                    description: "User shell source adoption".into(),
                    status: EffectStatus::Intended,
                    idempotent: true,
                }),
                &self.artifacts,
            )?;
        }
        append_session_command(
            &transaction,
            &mut session,
            &mut head,
            request,
            SessionCommand::ShellStarted { job },
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish_shell(
        &mut self,
        session_id: SessionId,
        request: Uuid,
        mut report: ShellReport,
    ) -> Result<Digest, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut session, mut head) = load_session_state(&transaction, session_id, None)?;
        let submission = session
            .submissions
            .get(&request)
            .ok_or(StoreError::Invalid("unknown shell submission"))?;
        if session.active_request != Some(request)
            || submission.manual_job.as_ref() != Some(&report.job)
        {
            return Err(StoreError::Invalid(
                "shell result has no matching active actor",
            ));
        }
        for digest in [report.stdout, report.stderr, report.receipt] {
            self.artifacts.read(digest)?;
        }
        let status = match report.status {
            crate::runtime::ExecutionStatus::Exited(0) => JobStatus::Succeeded,
            crate::runtime::ExecutionStatus::Unknown(_) => JobStatus::Unknown,
            crate::runtime::ExecutionStatus::Cancelled => JobStatus::Cancelled,
            _ => JobStatus::Failed,
        };
        let known = status != JobStatus::Unknown;
        let mut seed = None;
        report.adopted = false;
        if let Some(source) = report.after.filter(|_| known) {
            Snapshot::load(source, &self.artifacts)
                .and_then(|snapshot| snapshot.verify_artifacts(&self.artifacts))
                .map_err(|_| StoreError::Integrity("shell result source is unavailable"))?;
            seed = Some(WorkspaceSeed {
                origin: report.job.origin,
                source,
                task: report.job.task,
            });
        }
        if let Some(id) = report.job.task {
            let (mut task, task_head) = load_state(&transaction, id)?;
            if !task
                .jobs
                .get(&report.job.job)
                .is_some_and(|job| job.status == JobStatus::Running)
            {
                return Err(StoreError::Invalid("shell job is already settled"));
            }
            append_task_event(
                &transaction,
                &mut task,
                task_head,
                TaskEvent::JobSettled {
                    id: report.job.job,
                    status,
                    receipt: Some(report.receipt),
                },
                &self.artifacts,
            )?;
            if report.job.scope_revision != Some(task.scope_revision) {
                seed = None;
                report.error = Some(
                    "User input superseded source adoption; the command result remains archived"
                        .into(),
                );
            }
            if let Some(seed) = &seed {
                let task_head = load_state(&transaction, id)?.1;
                append_task_event(
                    &transaction,
                    &mut task,
                    task_head,
                    TaskEvent::ManualWorkspace {
                        candidate: Candidate {
                            source: seed.source,
                            artifact: seed.source,
                            environment: report.job.environment,
                            frozen: true,
                            provenance: None,
                        },
                        origin: seed.origin,
                    },
                    &self.artifacts,
                )?;
            }
            let task_head = load_state(&transaction, id)?.1;
            append_task_event(
                &transaction,
                &mut task,
                task_head,
                TaskEvent::EffectRecorded(Effect {
                    operation_id: request,
                    description: "User shell source adoption".into(),
                    status: if !known {
                        EffectStatus::Unknown
                    } else if seed.is_some() {
                        EffectStatus::Succeeded
                    } else {
                        EffectStatus::Failed
                    },
                    idempotent: true,
                }),
                &self.artifacts,
            )?;
            let task_head = load_state(&transaction, id)?.1;
            append_task_event(
                &transaction,
                &mut task,
                task_head,
                TaskEvent::Stopped {
                    outcome: Outcome::Blocked,
                    reason: "User shell settled; coding-task verification remains separate".into(),
                },
                &self.artifacts,
            )?;
        }
        report.adopted = seed.is_some();
        let digest = self.artifacts.put(&serde_json::to_vec(&report)?)?;
        append_session_command(
            &transaction,
            &mut session,
            &mut head,
            Uuid::new_v5(&request, b"shell-published"),
            SessionCommand::ShellPublished {
                request,
                report: digest,
                seed,
                settled: known,
            },
        )?;
        append_session_command(
            &transaction,
            &mut session,
            &mut head,
            Uuid::new_v5(&request, b"shell-settled"),
            SessionCommand::TurnSettled {
                request,
                outcome: None,
                error: report.error,
            },
        )?;
        transaction.commit()?;
        Ok(digest)
    }

    pub fn workspace_restored(
        &mut self,
        id: TaskId,
        revision: u64,
        source: Digest,
    ) -> Result<TaskState, StoreError> {
        self.change(id, Some(revision), false, |state, _, _| {
            if state.workspace_override != Some(source)
                || state.jobs.values().any(|job| job.status.unresolved())
            {
                return Err(StoreError::Invalid(
                    "workspace restore has no matching settled source",
                ));
            }
            Ok(TaskEvent::WorkspaceRestored { source })
        })
    }
}
