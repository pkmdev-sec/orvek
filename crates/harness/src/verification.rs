use crate::{
    Digest, Store, StoreError,
    contract::ControlRequirement,
    runtime::{DockerExecutor, ExecutionRequest, ExecutionStatus},
    state::{CheckStatus, ControlObservation, Observation, TaskId, TaskState},
    store::VerificationLease,
    workspace::Snapshot,
};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Expectations execute in the trusted host; candidate output remains data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckProgram {
    pub version: u32,
    pub probes: Vec<Probe>,
    pub control_failure: Option<ControlFailure>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Probe {
    Command {
        id: String,
        command: String,
        exit_code: i32,
        stdout: Option<Expectation>,
        stderr: Option<Expectation>,
    },
    File {
        id: String,
        path: String,
        content: Digest,
    },
}

impl Probe {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Command { id, .. } | Self::File { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Expectation {
    Equals(String),
    Contains(String),
}

impl Expectation {
    fn matches(&self, bytes: &[u8]) -> bool {
        match self {
            Self::Equals(text) => bytes == text.as_bytes(),
            Self::Contains(text) => {
                !text.is_empty()
                    && bytes
                        .windows(text.len())
                        .any(|window| window == text.as_bytes())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlFailure {
    pub probe: String,
    pub stdout: Option<Expectation>,
    pub stderr: Option<Expectation>,
}

impl CheckProgram {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.version != 1 || self.probes.is_empty() || self.probes.len() > 128 {
            return Err(StoreError::Invalid(
                "invalid verification program version or probe count",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for probe in &self.probes {
            if probe.id().is_empty() || !ids.insert(probe.id()) {
                return Err(StoreError::Invalid("verification probe IDs must be unique"));
            }
            match probe {
                Probe::Command {
                    command,
                    stdout,
                    stderr,
                    ..
                } => {
                    if command.trim().is_empty() || (stdout.is_none() && stderr.is_none()) {
                        return Err(StoreError::Invalid(
                            "command probes require an observable output expectation",
                        ));
                    }
                }
                Probe::File { path, .. } => {
                    if path.is_empty()
                        || !Path::new(path)
                            .components()
                            .all(|part| matches!(part, Component::Normal(_)))
                    {
                        return Err(StoreError::Invalid("file probe escapes candidate"));
                    }
                }
            }
        }
        if let Some(control) = &self.control_failure
            && (!ids.contains(control.probe.as_str())
                || (control.stdout.is_none() && control.stderr.is_none()))
        {
            return Err(StoreError::Invalid(
                "control must name a probe and an expected behavioral failure",
            ));
        }
        Ok(())
    }
}

pub struct VerificationTicket {
    lease: VerificationLease,
    program: CheckProgram,
    candidate: Snapshot,
    environment: Digest,
    timeout_ms: u64,
    control_required: bool,
    control_kind: ControlRequirement,
    control_source: Option<Digest>,
}

#[derive(Debug, Serialize)]
pub struct ProbeReport {
    pub id: String,
    pub passed: bool,
    pub infrastructure_ok: bool,
    pub assertions: u64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub execution: Option<crate::runtime::ExecutionResult>,
}

#[derive(Debug, Serialize)]
pub struct VerificationReport {
    pub job_id: Uuid,
    pub probes: Vec<ProbeReport>,
    pub control: Vec<ProbeReport>,
    pub limitations: Vec<String>,
    pub cancelled: bool,
    pub control_required: bool,
    pub control_matched: bool,
    pub image_id: String,
    pub source: Digest,
    pub control_source: Option<Digest>,
}

pub fn prepare(
    store: &mut Store,
    task: TaskId,
    revision: u64,
    check: &str,
) -> Result<VerificationTicket, StoreError> {
    let state = store.load(task)?;
    if state.revision != revision {
        return Err(StoreError::Revision {
            expected: revision,
            actual: state.revision,
        });
    }
    let definition = state
        .accepted_contract()?
        .checks
        .get(check)
        .ok_or(StoreError::Invalid("unknown check"))?;
    let program: CheckProgram =
        serde_json::from_slice(&store.artifacts().read(definition.verifier)?)?;
    program.validate()?;
    let candidate = state
        .candidate
        .as_ref()
        .ok_or(StoreError::Invalid("missing candidate"))?;
    let snapshot = Snapshot::load(candidate.source, store.artifacts())
        .map_err(|_| StoreError::Invalid("invalid candidate snapshot"))?;
    let control_required = definition.control != ControlRequirement::None;
    let control_kind = definition.control;
    let control_source = match control_kind {
        ControlRequirement::None => None,
        ControlRequirement::BaselineFailure => Some(
            state
                .baseline
                .as_ref()
                .ok_or(StoreError::Invalid(
                    "baseline must be established before verification",
                ))?
                .source,
        ),
        ControlRequirement::NegativeControl => definition.control_source,
    };
    if control_required && program.control_failure.is_none() {
        return Err(StoreError::Invalid(
            "required negative control is unspecified",
        ));
    }
    let timeout_ms = definition.timeout_ms;
    let environment = candidate.environment;
    let lease = store.begin_check(task, revision, check)?;
    Ok(VerificationTicket {
        lease,
        program,
        candidate: snapshot,
        environment,
        timeout_ms,
        control_required,
        control_kind,
        control_source,
    })
}

pub async fn execute(
    ticket: &VerificationTicket,
    candidate_path: &Path,
    control: Option<(&Snapshot, &Path)>,
    executor: &DockerExecutor,
    cancellation: CancellationToken,
) -> VerificationReport {
    let mut report = VerificationReport {
        job_id: ticket.lease.job_id(),
        probes: Vec::new(),
        control: Vec::new(),
        limitations: Vec::new(),
        cancelled: false,
        control_required: ticket.control_required,
        control_matched: false,
        image_id: executor.image_id().to_owned(),
        source: Digest::of_value(&ticket.candidate).expect("snapshot serialization is defined"),
        control_source: None,
    };
    if Digest::of_value(&executor.environment()).ok() != Some(ticket.environment) {
        report
            .limitations
            .push("execution environment differs from the admitted identity".into());
        return report;
    }
    if !ticket
        .candidate
        .matches_exact(candidate_path)
        .unwrap_or(false)
    {
        report
            .limitations
            .push("candidate bytes differ from the frozen manifest".into());
        return report;
    }
    let started = tokio::time::Instant::now();
    for probe in &ticket.program.probes {
        let remaining = std::time::Duration::from_millis(ticket.timeout_ms)
            .saturating_sub(started.elapsed())
            .as_millis() as u64;
        if remaining == 0 || cancellation.is_cancelled() {
            report.cancelled = cancellation.is_cancelled();
            report
                .limitations
                .push("verification interrupted or deadline exhausted".into());
            break;
        }
        report.probes.push(
            run_probe(
                probe_job_id(ticket.lease.job_id(), probe.id(), false),
                probe,
                &ticket.candidate,
                candidate_path,
                executor,
                remaining,
                cancellation.clone(),
            )
            .await,
        );
    }
    if ticket.control_required {
        if let Some((snapshot, path)) = control.filter(|(snapshot, path)| {
            snapshot.matches_exact(path).unwrap_or(false)
                && Digest::of_value(snapshot).ok() == ticket.control_source
        }) {
            report.control_source = Digest::of_value(snapshot).ok();
            for probe in &ticket.program.probes {
                let remaining = std::time::Duration::from_millis(ticket.timeout_ms)
                    .saturating_sub(started.elapsed())
                    .as_millis() as u64;
                if remaining == 0 || cancellation.is_cancelled() {
                    report
                        .limitations
                        .push("negative control was interrupted".into());
                    break;
                }
                report.control.push(
                    run_probe(
                        probe_job_id(ticket.lease.job_id(), probe.id(), true),
                        probe,
                        snapshot,
                        path,
                        executor,
                        remaining,
                        cancellation.clone(),
                    )
                    .await,
                );
            }
            if let Some(expected) = &ticket.program.control_failure {
                report.control_matched = report.control.iter().any(|probe| {
                    probe.id == expected.probe
                        && !probe.passed
                        && probe.infrastructure_ok
                        && expected
                            .stdout
                            .as_ref()
                            .is_none_or(|value| value.matches(&probe.stdout))
                        && expected
                            .stderr
                            .as_ref()
                            .is_none_or(|value| value.matches(&probe.stderr))
                });
            }
            if !snapshot.matches_exact(path).unwrap_or(false) {
                report
                    .limitations
                    .push("control source changed during verification".into());
            }
        } else {
            report
                .limitations
                .push("required baseline/control workspace unavailable or changed".into());
        }
    }
    if !ticket
        .candidate
        .matches_exact(candidate_path)
        .unwrap_or(false)
    {
        report
            .limitations
            .push("candidate changed during verification".into());
    }
    report.cancelled |= cancellation.is_cancelled();
    report
}

pub fn finish(
    store: &mut Store,
    ticket: VerificationTicket,
    report: VerificationReport,
) -> Result<TaskState, StoreError> {
    if report.job_id != ticket.lease.job_id() {
        return Err(StoreError::Lease);
    }
    let raw_report = store.artifacts().put(&serde_json::to_vec(&report)?)?;
    let control_report = if report.control_required {
        Some(
            store
                .artifacts()
                .put(&serde_json::to_vec(&report.control)?)?,
        )
    } else {
        None
    };
    let complete = report.probes.len() == ticket.program.probes.len();
    let all_observed = complete
        && report.probes.iter().all(|probe| probe.infrastructure_ok)
        && report.limitations.is_empty();
    let passed = all_observed
        && report.probes.iter().all(|probe| probe.passed)
        && (!report.control_required || report.control_matched);
    let unreconciled_jobs = report
        .probes
        .iter()
        .chain(&report.control)
        .filter_map(|probe| probe.execution.as_ref())
        .filter(|execution| matches!(execution.status, ExecutionStatus::Unknown(_)))
        .map(|execution| execution.job_id)
        .collect();
    let status = if report.cancelled {
        CheckStatus::Cancelled
    } else if !all_observed {
        CheckStatus::Inconclusive
    } else if passed {
        CheckStatus::Passed
    } else {
        CheckStatus::Failed
    };
    store.finish_check(
        ticket.lease,
        Observation {
            status,
            report: raw_report,
            assertions: report.probes.iter().map(|probe| probe.assertions).sum(),
            discovered: Some(ticket.program.probes.len() as u64),
            skipped: ticket
                .program
                .probes
                .len()
                .saturating_sub(report.probes.len()) as u64,
            exit_code: Some(if passed { 0 } else { 1 }),
            signal: None,
            control: report
                .control_source
                .zip(control_report)
                .map(|(source, report_digest)| ControlObservation {
                    kind: ticket.control_kind,
                    source,
                    rejected: report.control.iter().any(|probe| !probe.passed),
                    intended_reason: report.control_matched,
                    report: report_digest,
                }),
            baseline_unchanged: false,
            limitations: report.limitations,
            unreconciled_jobs,
        },
    )
}

async fn run_probe(
    job_id: Uuid,
    probe: &Probe,
    snapshot: &Snapshot,
    workspace: &Path,
    executor: &DockerExecutor,
    timeout_ms: u64,
    cancellation: CancellationToken,
) -> ProbeReport {
    match probe {
        Probe::File { id, path, content } => {
            let passed = matches!(snapshot.entries.get(path), Some(crate::workspace::Entry::File { content: actual, .. }) if actual == content);
            ProbeReport {
                id: id.clone(),
                passed,
                infrastructure_ok: true,
                assertions: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
                execution: None,
            }
        }
        Probe::Command {
            id,
            command,
            exit_code,
            stdout,
            stderr,
        } => {
            let request = ExecutionRequest {
                job_id,
                workspace: workspace.to_owned(),
                command: command.clone(),
                readonly: true,
                timeout_ms,
                output_bytes: 64 * 1024,
            };
            match executor.run(&request, cancellation).await {
                Ok(execution) => {
                    let infrastructure_ok = matches!(execution.status, ExecutionStatus::Exited(code) if code != 126 && code != 127);
                    let passed = execution.status == ExecutionStatus::Exited(*exit_code)
                        && stdout
                            .as_ref()
                            .is_none_or(|value| value.matches(&execution.stdout))
                        && stderr
                            .as_ref()
                            .is_none_or(|value| value.matches(&execution.stderr));
                    ProbeReport {
                        id: id.clone(),
                        passed,
                        infrastructure_ok,
                        assertions: u64::from(infrastructure_ok)
                            * (1 + u64::from(stdout.is_some()) + u64::from(stderr.is_some())),
                        stdout: execution.stdout.clone(),
                        stderr: execution.stderr.clone(),
                        execution: Some(execution),
                    }
                }
                Err(error) => ProbeReport {
                    id: id.clone(),
                    passed: false,
                    infrastructure_ok: false,
                    assertions: 0,
                    stdout: Vec::new(),
                    stderr: error.to_string().into_bytes(),
                    execution: None,
                },
            }
        }
    }
}

pub fn probe_job_id(verification_job: Uuid, probe: &str, control: bool) -> Uuid {
    Uuid::new_v5(&verification_job, format!("{control}:{probe}").as_bytes())
}
