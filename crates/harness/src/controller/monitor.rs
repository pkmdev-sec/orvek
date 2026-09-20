use super::{Host, HostError};
use crate::{
    Digest, Store, StoreError,
    monitor::{
        Cohort, Episode, EpisodeState, Measure, Measurement, MonitorReport, Origin, PinnedBehavior,
        ReadCase, Release, Signature, evaluation,
    },
    session::{JournalRecord, SessionCommand, SessionEvent, SessionId},
    state::{CheckStatus, JobStatus, ModelCallStatus, TaskEvent, TaskId},
};
use serde_json::{Value, json};
use std::{
    fs,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

impl Host {
    pub async fn monitor_report(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<MonitorReport, HostError> {
        Ok(self.store.lock().await.monitor_report(offset, limit)?)
    }
    /// Operator-only release. This is not model/event-payload authority.
    pub async fn install_read_behavior(
        &self,
        expected: Digest,
        bytes: u32,
        note: String,
    ) -> Result<Digest, HostError> {
        let _monitor = self.monitor.lock().await;
        Ok(self.store.lock().await.monitor_activate(
            expected,
            Release {
                parent: Some(expected),
                native_read_output_bytes: bytes,
                note,
            },
            "operator release",
            None,
        )?)
    }
    pub async fn rollback_read_behavior(&self, expected: Digest) -> Result<Digest, HostError> {
        let _monitor = self.monitor.lock().await;
        let mut store = self.store.lock().await;
        let previous = store
            .monitor_status()?
            .previous
            .ok_or(HostError::Invalid("no previous behavior"))?;
        let release = store.monitor_release(previous)?;
        Ok(store.monitor_activate(expected, release, "operator rollback", None)?)
    }
    pub async fn set_monitor_sampling(&self, every: u32) -> Result<(), HostError> {
        if every == 0 || every > 10000 {
            return Err(HostError::Invalid("sampling interval must be 1..10000"));
        }
        let _monitor = self.monitor.lock().await;
        let store = self.store.lock().await;
        let mut status = store.monitor_status()?;
        status.sample_every = every;
        store.monitor_save_status(&status)?;
        Ok(())
    }
    pub(crate) fn pin_read_behavior(&self, store: &Store) -> Result<PinnedBehavior, StoreError> {
        let active = store.monitor_status()?.active;
        let release = store.monitor_release(active)?;
        Ok(PinnedBehavior {
            release: active,
            native_read_output_bytes: release.native_read_output_bytes,
            build: self.monitor_build,
        })
    }
    /// Bounded pure trace intake; expensive checks run after releasing the user-task store lock.
    pub async fn monitor_tick(&self) -> Result<(), HostError> {
        let _monitor = self.monitor.lock().await;
        {
            let mut store = self.store.lock().await;
            let mut status = store.monitor_status()?;
            let records = store.journal_page(status.cursor, 64)?;
            let mut measures = Vec::new();
            let mut episodes = Vec::new();
            for record in records {
                inspect(
                    &store,
                    &record,
                    &mut status,
                    &mut measures,
                    &mut episodes,
                    self.monitor_build,
                )?;
                status.cursor = record.sequence;
            }
            status.last_error = None;
            store.monitor_commit_page(&status, &measures, &episodes)?;
        }
        let pending = self.store.lock().await.monitor_pending()?;
        let Some(mut episode) = pending else {
            return Ok(());
        };
        if matches!(episode.state, EpisodeState::Evaluating) {
            episode.state = EpisodeState::Uncertain {
                reason: "unfinished evaluation; no automatic effect replay".into(),
            };
            let mut store = self.store.lock().await;
            store.monitor_save_episode(&episode)?;
            let status = store.monitor_status()?;
            let measure =
                Measure::new(episode.cohort.clone(), Signature::EvaluatorFailure, 1, 1, 0);
            store.monitor_commit_page(&status, &[Measurement::new(episode.id, measure)], &[])?;
            return Ok(());
        }
        if episode.cohort.build != self.monitor_build {
            episode.state = EpisodeState::Uncertain {
                reason: "host build changed after diagnosis; no matched evaluator".into(),
            };
            self.store.lock().await.monitor_save_episode(&episode)?;
            return Ok(());
        }
        let (artifacts, before, parent) = {
            let mut store = self.store.lock().await;
            if store.monitor_status()?.active != episode.regressed {
                episode.state = EpisodeState::Superseded;
                store.monitor_save_episode(&episode)?;
                return Ok(());
            }
            let before = store.monitor_release(episode.regressed)?;
            let parent = store.monitor_release(episode.parent)?;
            episode.state = EpisodeState::Evaluating;
            store.monitor_save_episode(&episode)?;
            let status = store.monitor_status()?;
            let measure =
                Measure::new(episode.cohort.clone(), Signature::EvaluatorFailure, 1, 0, 0);
            store.monitor_commit_page(&status, &[Measurement::new(episode.id, measure)], &[])?;
            (store.artifacts().clone(), before, parent)
        };
        // This worker can write only a typed configuration candidate, never execute it.
        // Native model tools are not used: they have broad same-user authority.
        let workspace = tempfile::Builder::new()
            .prefix("read-repair-")
            .tempdir_in(&self.root)?;
        let candidate = workspace.path().join("candidate.json");
        let bytes = serde_json::to_vec(&evaluation::CandidateConfig {
            native_read_output_bytes: parent.native_read_output_bytes,
        })?;
        fs::write(&candidate, &bytes)?;
        episode.candidate = Some(artifacts.put(&bytes).map_err(StoreError::from)?);
        self.store.lock().await.monitor_save_episode(&episode)?;
        match evaluation::evaluate(
            &artifacts,
            &episode,
            &candidate,
            before.native_read_output_bytes,
            parent.native_read_output_bytes,
        )
        .await
        {
            Ok(result) => {
                episode.result = Some(
                    artifacts
                        .put(&serde_json::to_vec(&result)?)
                        .map_err(StoreError::from)?,
                );
                let mut store = self.store.lock().await;
                let status = store.monitor_status()?;
                let measure =
                    Measure::new(episode.cohort.clone(), Signature::EvaluatorFailure, 1, 0, 1);
                store.monitor_commit_page(
                    &status,
                    &[Measurement::new(episode.id, measure)],
                    &[],
                )?;
                if result["accepted"] != true {
                    episode.state = EpisodeState::Rejected {
                        reason: "predeclared deterministic outcomes did not pass".into(),
                    };
                    store.monitor_save_episode(&episode)?;
                    return Ok(());
                }
                let release = Release {
                    parent: Some(episode.regressed),
                    native_read_output_bytes: parent.native_read_output_bytes,
                    note: format!("deterministic read capacity recovery {}", episode.id),
                };
                store.monitor_activate(
                    episode.regressed,
                    release,
                    "deterministic native read checks",
                    Some(episode),
                )?;
            }
            Err(reason) => {
                episode.state = EpisodeState::Uncertain {
                    reason: reason.clone(),
                };
                let mut store = self.store.lock().await;
                store.monitor_save_episode(&episode)?;
                let mut status = store.monitor_status()?;
                status.last_error = Some(format!("evaluator: {reason}"));
                let measure =
                    Measure::new(episode.cohort.clone(), Signature::EvaluatorFailure, 1, 1, 1);
                store.monitor_commit_page(
                    &status,
                    &[Measurement::new(episode.id, measure)],
                    &[],
                )?;
            }
        }
        Ok(())
    }
    pub(crate) async fn run_monitor(self: &Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if !self.accepting.load(Ordering::Acquire) {
                return;
            }
            if let Err(error) = self.monitor_tick().await {
                eprintln!("trace monitor: {error}");
                let store = self.store.lock().await;
                if let Ok(mut status) = store.monitor_status() {
                    status.last_error = Some(error.to_string());
                    let _ = store.monitor_save_status(&status);
                }
            }
        }
    }
}

fn inspect(
    store: &Store,
    record: &JournalRecord,
    status: &mut crate::monitor::MonitorStatus,
    measures: &mut Vec<Measurement>,
    episodes: &mut Vec<Episode>,
    current_build: Option<Digest>,
) -> Result<(), StoreError> {
    if record.kind == "session" {
        let session: SessionId = record
            .aggregate
            .parse()
            .map_err(|_| StoreError::Integrity("monitor session"))?;
        let Some(cohort) = store.monitor_cohort(session)? else {
            return Ok(());
        };
        if store.monitor_origin(session)? != Origin::User {
            return Ok(());
        }
        if let SessionEvent::Command { command, .. } = serde_json::from_value(record.event.clone())?
        {
            match command {
                SessionCommand::ProviderCost { call, cost_usd, .. } => {
                    let mut measure =
                        Measure::new(cohort, Signature::Cost, 1, 0, u64::from(cost_usd.is_some()));
                    if let Some(cost) = cost_usd {
                        measure.recorded_usd = cost;
                    }
                    measures.push(Measurement::new(
                        Digest::of_value(&(session, call))?,
                        measure,
                    ));
                }
                SessionCommand::ReviewRecorded { feedback } => {
                    let feedback = crate::feedback::read(store.artifacts(), feedback)?;
                    let changes =
                        feedback.disposition == crate::feedback::Disposition::ChangesRequested;
                    measures.push(Measurement::new(
                        Digest::of_value(&record.sequence)?,
                        Measure::new(cohort, Signature::UserCorrection, 1, u64::from(changes), 1),
                    ));
                }
                _ => {}
            }
        }
        return Ok(());
    }
    if record.kind != "task" {
        return Ok(());
    }
    let task: TaskId = TaskId(
        record
            .aggregate
            .parse()
            .map_err(|_| StoreError::Integrity("monitor task"))?,
    );
    let Some(session) = store.monitor_task_session(task)? else {
        return Ok(());
    };
    let Some(mut cohort) = store.monitor_cohort(session)? else {
        return Ok(());
    };
    if store.monitor_origin(session)? != Origin::User {
        status.sampling.skipped_origin += 1;
        return Ok(());
    }
    let event = serde_json::from_value::<TaskEvent>(record.event.clone())?;
    let job = match &event {
        TaskEvent::JobStarted(job) | TaskEvent::ManualStarted { job } => Some(job.clone()),
        TaskEvent::JobSettled { id, .. } | TaskEvent::JobFenced { id, .. } => {
            store.load(task)?.jobs.get(id).cloned()
        }
        _ => None,
    };
    if let Some(invocation) = job.as_ref().and_then(|job| job.invocation.as_ref()) {
        let environment: Value =
            serde_json::from_slice(&store.artifacts().read(invocation.environment)?)?;
        cohort.build = environment
            .get("host_build")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
    }
    let identity = match &event {
        TaskEvent::ModelCallReserved { operation }
        | TaskEvent::ModelCallRecorded { operation, .. } => {
            Digest::of_value(&(session, operation))?
        }
        TaskEvent::JobStarted(job) | TaskEvent::ManualStarted { job } => {
            Digest::of_value(&(task, job.id))?
        }
        TaskEvent::JobSettled { id, .. } | TaskEvent::JobFenced { id, .. } => {
            Digest::of_value(&(task, id))?
        }
        TaskEvent::EffectRecorded(effect) => Digest::of_value(&(task, effect.operation_id))?,
        _ => Digest::of_value(&record.sequence)?,
    };
    let mut add = |signature, opportunities, failures, measured| {
        measures.push(Measurement::new(
            identity,
            Measure::new(cohort.clone(), signature, opportunities, failures, measured),
        ))
    };
    match event {
        TaskEvent::ModelCallReserved { .. } => {
            add(Signature::ProviderError, 1, 0, 0);
            add(Signature::MissingUsage, 1, 0, 0);
            add(Signature::Cost, 1, 0, 0);
        }
        TaskEvent::ModelCallRecorded { receipt, .. } => {
            add(
                Signature::ProviderError,
                1,
                u64::from(receipt.status == ModelCallStatus::Failed),
                u64::from(!matches!(receipt.status, ModelCallStatus::Unknown)),
            );
            add(
                Signature::MissingUsage,
                1,
                u64::from(receipt.tokens.is_none()),
                u64::from(receipt.tokens.is_some()),
            );
        }
        TaskEvent::EffectRecorded(effect) => {
            add(
                Signature::UnresolvedEffect,
                1,
                u64::from(matches!(
                    effect.status,
                    crate::state::EffectStatus::Unknown | crate::state::EffectStatus::Intended
                )),
                1,
            );
        }
        TaskEvent::Observed(evidence) => {
            add(
                Signature::VerificationFailure,
                1,
                u64::from(evidence.observation.status == CheckStatus::Failed),
                u64::from(matches!(
                    evidence.observation.status,
                    CheckStatus::Passed | CheckStatus::Failed
                )),
            );
            add(
                Signature::EvaluatorFailure,
                1,
                u64::from(evidence.observation.status == CheckStatus::Inconclusive),
                1,
            );
        }
        TaskEvent::JobStarted(_) | TaskEvent::ManualStarted { .. } => {
            add(Signature::JobOutcome, 1, 0, 0);
            add(Signature::UnresolvedJob, 1, 0, 0);
        }
        TaskEvent::JobFenced { .. } => {
            add(Signature::JobOutcome, 1, 0, 1);
            add(Signature::UnresolvedJob, 1, 0, 1);
        }
        TaskEvent::JobSettled {
            id,
            status: job_status,
            receipt,
        } => {
            add(
                Signature::JobOutcome,
                1,
                u64::from(job_status == JobStatus::Failed),
                u64::from(job_status != JobStatus::Unknown),
            );
            add(
                Signature::UnresolvedJob,
                1,
                u64::from(job_status == JobStatus::Unknown),
                1,
            );
            let task = store.load(task)?;
            let Some(job) = task.jobs.get(&id) else {
                return Ok(());
            };
            let Some(invocation) = &job.invocation else {
                return Ok(());
            };
            if invocation.capability != "read_file" || job_status != JobStatus::Succeeded {
                return Ok(());
            }
            let Some(receipt) = receipt else {
                return Ok(());
            };
            let value: Value = serde_json::from_slice(&store.artifacts().read(receipt)?)?;
            if value["backend"] != "host" {
                return Ok(());
            }
            let input: Value = serde_json::from_slice(&store.artifacts().read(invocation.input)?)?;
            let output = &value["tool_result"];
            let received = evaluation::decoded_content(output).map(|b| b.len());
            let Some(size) = output["result"]["size_bytes"].as_u64() else {
                return Ok(());
            };
            let offset = input["arguments"]["offset"].as_u64().unwrap_or(0);
            let requested = input["arguments"]["max_bytes"].as_u64().unwrap_or(65536);
            let wanted = requested.min(size.saturating_sub(offset)).min(4096) as usize;
            let underfill = received.is_some_and(|n| n < wanted);
            add(
                Signature::ReadUnderfill,
                1,
                u64::from(underfill),
                u64::from(received.is_some()),
            );
            if !underfill {
                return Ok(());
            }
            status.sampling.considered += 1;
            if !(status.sampling.considered - 1).is_multiple_of(u64::from(status.sample_every)) {
                status.sampling.skipped_sampling += 1;
                return Ok(());
            }
            status.sampling.selected += 1;
            if cohort.build != current_build {
                status.sampling.skipped_no_hypothesis += 1;
                return Ok(());
            }
            let Some(episode) = diagnose(store, record.sequence, cohort, receipt, wanted)? else {
                status.sampling.skipped_no_hypothesis += 1;
                return Ok(());
            };
            if store.monitor_has_episode(episode.id)? || episodes.iter().any(|e| e.id == episode.id)
            {
                return Ok(());
            }
            if store.monitor_pending()?.is_some() || !episodes.is_empty() {
                status.sampling.skipped_capacity += 1;
                return Ok(());
            }
            episodes.push(episode);
        }
        _ => {}
    }
    Ok(())
}

fn diagnose(
    store: &Store,
    sequence: u64,
    cohort: Cohort,
    receipt: Digest,
    wanted: usize,
) -> Result<Option<Episode>, StoreError> {
    let Some(current) = cohort.release else {
        return Ok(None);
    };
    if cohort.build.is_none() || current != store.monitor_status()?.active {
        return Ok(None);
    }
    let release = store.monitor_release(current)?;
    let Some(parent_id) = release.parent else {
        return Ok(None);
    };
    let parent = store.monitor_release(parent_id)?;
    if release.native_read_output_bytes >= parent.native_read_output_bytes
        || wanted > ((parent.native_read_output_bytes - 2048) / 6) as usize
    {
        return Ok(None);
    }
    let id = Digest::of_value(&("native-read-capacity-v1", current))?;
    if store.monitor_has_episode(id)? {
        return Ok(None);
    }
    let artifacts = store.artifacts();
    // Minimized boundary reproduction, not a copy of private user file contents.
    let regression = ReadCase {
        bytes: vec![b'x'; wanted],
        offset: 0,
        max_bytes: wanted,
    };
    let source_diff = artifacts.put(&serde_json::to_vec(&json!({"path":"behavior.native_read_output_bytes","before_release":parent_id,"after_release":current,"before":parent.native_read_output_bytes,"after":release.native_read_output_bytes}))?)?;
    Ok(Some(Episode { id, origin: Origin::Repair, sequence, cohort, regressed: current, parent: parent_id,
        hypothesis: "The pinned release reduced native read reply capacity. Its actual receipt underfilled a requested page that the parent envelope can fit. Restore only that config field; require failing control and exact held-out byte/metadata checks. This is not a model-quality hypothesis.".into(), source_diff, trace_receipt: receipt,
        regression: artifacts.put(&serde_json::to_vec(&regression)?)?, heldout: artifacts.put(&serde_json::to_vec(&evaluation::heldout())?)?, candidate: None,result:None,state:EpisodeState::Diagnosed }))
}

#[cfg(test)]
mod tests;
