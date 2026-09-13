use super::*;
use crate::{
    manual::{ManualJob, ShellReport, ShellSpec},
    runtime::{ExecutionRequest, ExecutionStatus},
    submission::SubmissionStatus,
};

impl Host {
    pub(super) async fn execute_shell(
        &self,
        session: SessionId,
        request: Uuid,
        mut spec: ShellSpec,
    ) -> Result<SubmissionStatus, HostError> {
        let permit = self
            .runs
            .clone()
            .try_acquire_owned()
            .map_err(|_| HostError::Busy)?;
        let cancellation = self.queue_stop.child_token();
        {
            let mut active = self.active.lock().await;
            if !self.accepting.load(Ordering::Acquire) {
                return Err(HostError::ShuttingDown);
            }
            if active.contains_key(&session) {
                return Err(HostError::Busy);
            }
            active.insert(session, cancellation.clone());
        }
        let result = async {
            let (state, artifacts) = {
                let store = self.store.lock().await;
                let input = crate::input::load(
                    store.submission(session, request)?.input,
                    store.artifacts(),
                )?;
                spec.command = input
                    .text
                    .strip_prefix("! ")
                    .ok_or(HostError::Invalid("shell input lost its command prefix"))?
                    .to_owned();
                spec.validate()?;
                (store.load_session(session)?, store.artifacts().clone())
            };
            if let Some(task) = state.current_task {
                self.reconcile_unresolved(task, cancellation.clone())
                    .await?;
            }
            let (origin, snapshot) = self.prepare_workspace(&state, &artifacts)?;
            let before = snapshot.publish(&artifacts)?;
            let directory = self.root.join("shell").join(request.to_string());
            fs::create_dir_all(&directory)?;
            let working = directory.join("working");
            // A queued retry here has no ShellStarted event and therefore has
            // never dispatched this command. Recover only its unadmitted copy.
            if working.exists() {
                fs::remove_dir_all(&working)?;
            }
            snapshot.materialize(&working, &artifacts, false)?;
            let environment = artifacts
                .put(&serde_json::to_vec(&self.executor.environment())?)
                .map_err(StoreError::from)?;
            let scope_revision = if let Some(task) = state.current_task {
                Some(self.store.lock().await.load(task)?.scope_revision + 1)
            } else {
                None
            };
            let job = ManualJob {
                job: Uuid::new_v5(&request, b"shell-execution"),
                task: state.current_task,
                before,
                origin,
                environment,
                started_ms: crate::store::now_ms(),
                scope_revision,
            };
            self.store
                .lock()
                .await
                .begin_shell(session, request, job.clone())?;
            let run = self
                .executor
                .run(
                    &ExecutionRequest {
                        job_id: job.job,
                        workspace: working.clone(),
                        command: spec.command,
                        readonly: false,
                        timeout_ms: spec.timeout_ms,
                        output_bytes: spec.output_bytes,
                    },
                    cancellation,
                )
                .await;
            let (status, stdout, stderr, elapsed_ms, error, raw) = match run {
                Ok(run) => {
                    let raw = serde_json::to_value(&run)?;
                    (
                        run.status,
                        run.stdout,
                        run.stderr,
                        run.elapsed_ms,
                        None,
                        raw,
                    )
                }
                Err(error) => {
                    let message = error.to_string();
                    (
                        ExecutionStatus::Unknown(message.clone()),
                        Vec::new(),
                        Vec::new(),
                        crate::store::now_ms().saturating_sub(job.started_ms),
                        Some(message.clone()),
                        json!({"error":message}),
                    )
                }
            };
            let after = if matches!(status, ExecutionStatus::Unknown(_)) {
                None
            } else {
                Some(
                    Snapshot::capture(&working, SnapshotPolicy::default(), &artifacts)?
                        .publish(&artifacts)?,
                )
            };
            let receipt = artifacts
                .put(&serde_json::to_vec(
                    &json!({"version":1,"job":job,"request":request,"after":after,"execution":raw}),
                )?)
                .map_err(StoreError::from)?;
            let report = ShellReport {
                version: 1,
                job,
                status,
                stdout: artifacts.put(&stdout).map_err(StoreError::from)?,
                stderr: artifacts.put(&stderr).map_err(StoreError::from)?,
                elapsed_ms,
                receipt,
                after,
                adopted: false,
                error,
            };
            // Publish the observation before committing its state projection.
            // It is recoverable raw data; only finish_shell can adopt source.
            let mut record = tempfile::NamedTempFile::new_in(&directory)?;
            use std::io::Write;
            record.write_all(&serde_json::to_vec(&report)?)?;
            record.as_file().sync_all()?;
            record
                .persist_noclobber(directory.join("observation.json"))
                .map_err(|error| error.error)?;
            fs::File::open(&directory)?.sync_all()?;
            let digest = self
                .store
                .lock()
                .await
                .finish_shell(session, request, report)?;
            let report: ShellReport =
                serde_json::from_slice(&artifacts.read(digest).map_err(StoreError::from)?)?;
            Ok(SubmissionStatus::Finished {
                task: None,
                outcome: None,
                error: report.error,
            })
        }
        .await;
        self.active.lock().await.remove(&session);
        drop(permit);
        self.queue_wake.notify_waiters();
        result
    }
}
