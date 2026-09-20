//! Offline adapters over recorded data only. No execution or provider handles enter here.
use super::{
    MaterializationBudget, Payload, ReplayReport, TraceBundle, TraceError, decode, decoded_len,
    invalid,
};
use crate::{
    Digest,
    context::{ContextRepresentation, ContextSegmentRole, ContextView, Manifest},
    inference::{
        CallOutcome, InferenceRequest, ModelSettings, OutputItem, PromptCacheIdentity,
        RequestProvenance, ToolProposal,
    },
    session::{SessionCommand, SessionCreation, SessionEvent, SessionId, SessionState},
    state::{Job, JobStatus, ModelCallReceipt, ModelCallStatus, TaskId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalGap {
    DispatchMissing,
    DispatchPayloadUnavailable,
    OutcomeMissing,
    OutcomeUnavailable,
    WireUnavailable,
    ContextMissing,
    ReportMetadataUnavailable,
    TaskLinkUnavailable,
    ContextSourceUnavailable,
    ContextMaterializationUnavailable,
    ChildOriginUnavailable,
    ChildCallsUnavailable,
    ChildContextUnavailable,
    ProposalUnavailable,
    InvocationUnavailable,
    ToolLinkUnavailable,
    ToolResultUnavailable,
    UnattributedUsage,
    ResponseLinkUnavailable,
    UnavailableTraceReceipt,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordedStatus {
    #[default]
    Unavailable,
    /// The stub returned recorded data. This does not mean the call succeeded.
    Served,
}

#[derive(Debug, Default, Serialize)]
pub struct CallReplay {
    pub session: Option<SessionId>,
    pub request: Option<Uuid>,
    pub task: Option<TaskId>,
    pub child: Option<Uuid>,
    pub dispatch_sequence: Option<u64>,
    pub outcome: RecordedStatus,
    /// Prepared bytes are not proof of delivery, even if an attempt was dispatched.
    pub prepared_body_checked: bool,
    /// Recorded outputs served as data, not evidence that a tool would run again.
    pub tool_outputs: BTreeMap<String, Digest>,
    pub gaps: BTreeSet<CausalGap>,
}

#[derive(Debug, Serialize)]
pub struct ToolReplay {
    pub task: TaskId,
    pub call: Option<Uuid>,
    pub child: Option<Uuid>,
    pub tool_call: Option<String>,
    pub status: JobStatus,
    pub settlement_status: Option<JobStatus>,
    pub result: RecordedStatus,
    pub gaps: BTreeSet<CausalGap>,
}

#[derive(Debug, Default, Serialize)]
pub struct CausalReplay {
    /// Completeness of supported recorded relationships, not controller decisions.
    pub complete: bool,
    pub calls: BTreeMap<Uuid, CallReplay>,
    pub tools: BTreeMap<Uuid, ToolReplay>,
    pub gaps: BTreeSet<CausalGap>,
}

#[derive(Deserialize)]
struct Link {
    session: SessionId,
    request: Uuid,
    task: TaskId,
    child: Option<Uuid>,
    call: Uuid,
}

#[derive(Deserialize)]
struct Dispatch {
    model: ModelSettings,
    input: Digest,
    tools: Digest,
    instructions: Digest,
    payload: Digest,
    cache: PromptCacheIdentity,
}

#[derive(Default)]
struct CallData<'a> {
    dispatch: Option<&'a Value>,
    report: Option<&'a Value>,
    outcome: Option<&'a Value>,
    receipt: Option<&'a ModelCallReceipt>,
    outcome_sequence: Option<u64>,
}

struct RecordedProvider<'a> {
    bundle: &'a TraceBundle,
}

impl RecordedProvider<'_> {
    fn respond(
        &self,
        data: &CallData<'_>,
        replay: &mut CallReplay,
        budget: &mut MaterializationBudget,
    ) -> Result<Option<CallOutcome>, TraceError> {
        let request = match data.dispatch {
            None => {
                replay.gaps.insert(CausalGap::DispatchMissing);
                None
            }
            Some(span) => {
                let dispatch: Dispatch = Deserialize::deserialize(span)?;
                let template = artifact_json(self.bundle, dispatch.payload, budget)?;
                let input = artifact_json(self.bundle, dispatch.input, budget)?;
                let tools = artifact_json(self.bundle, dispatch.tools, budget)?;
                let instructions = artifact_bytes(self.bundle, dispatch.instructions, budget)?;
                if let Some(template) = &template
                    && (input.as_ref().is_some_and(|v| v != &template["input"])
                        || tools.as_ref().is_some_and(|v| v != &template["tools"])
                        || instructions.as_ref().is_some_and(|v| {
                            Some(v.as_slice())
                                != template["instructions"].as_str().map(str::as_bytes)
                        }))
                {
                    return Err(invalid(
                        "dispatch input/tools/instructions disagree with logical template",
                    ));
                }
                if input.is_none()
                    || tools.is_none()
                    || instructions.is_none()
                    || template.is_none()
                {
                    replay.gaps.insert(CausalGap::DispatchPayloadUnavailable);
                }
                if let Some(report) = data.report {
                    if ["model", "cache", "sent_input"]
                        .iter()
                        .any(|key| report.get(*key).is_none())
                    {
                        replay.gaps.insert(CausalGap::ReportMetadataUnavailable);
                    }
                    equal_if_present(report, "model", &serde_json::to_value(dispatch.model)?)?;
                    equal_if_present(report, "cache", &serde_json::to_value(&dispatch.cache)?)?;
                    if let Some(input) = &input {
                        equal_if_present(
                            report,
                            "sent_input",
                            &serde_json::to_value(Digest::of_value(input)?)?,
                        )?;
                    }
                }
                template
                    .map(|template| {
                        // Covers the reconstructed request plus protocol transform temporaries.
                        budget.charge(&template)?;
                        budget.charge(&template)?;
                        InferenceRequest::from_recorded_template(
                            dispatch.model,
                            &template,
                            &dispatch.cache,
                        )
                        .map_err(|_| invalid("dispatch logical template/cache is inconsistent"))
                    })
                    .transpose()?
            }
        };
        let Some(value) = data.outcome else {
            replay.gaps.insert(if data.receipt.is_some() {
                CausalGap::OutcomeUnavailable
            } else {
                CausalGap::OutcomeMissing
            });
            replay.gaps.insert(CausalGap::WireUnavailable);
            return Ok(None);
        };
        budget.charge(value)?;
        let outcome: CallOutcome = Deserialize::deserialize(value)?;
        if let Some(response) = &outcome.response {
            budget.charge(&response.history_items)?;
            response.validate_recorded().map_err(|_| {
                invalid("provider output disagrees with recorded history/proposals")
            })?;
            if outcome
                .response_id
                .as_ref()
                .is_some_and(|id| id != &response.id)
            {
                return Err(invalid("provider response identity disagrees with outcome"));
            }
        }
        if outcome
            .attempts
            .iter()
            .enumerate()
            .any(|(index, attempt)| attempt.number as usize != index + 1)
        {
            return Err(invalid(
                "provider attempt sequence disagrees with recorded call",
            ));
        }
        if let Some(receipt) = data.receipt {
            let completed = outcome.failure.is_none()
                && outcome.response.as_ref().is_some_and(|response| {
                    response.status == crate::inference::ResponseStatus::Completed
                });
            if matches!(receipt.status, ModelCallStatus::Completed) && !completed
                || matches!(receipt.status, ModelCallStatus::Failed) && completed
            {
                return Err(invalid(
                    "model receipt status disagrees with recorded outcome",
                ));
            }
        }
        if let Some(receipt) = data.receipt
            && receipt.tokens != outcome.accounted_tokens()
        {
            return Err(invalid(
                "model receipt tokens disagree with recorded outcome",
            ));
        }
        if let Some(dispatch) = replay.dispatch_sequence
            && data
                .outcome_sequence
                .is_some_and(|sequence| sequence <= dispatch)
        {
            return Err(invalid("model outcome precedes dispatch"));
        }
        match (&outcome.request, request) {
            (
                RequestProvenance::Prepared {
                    transport,
                    dialect,
                    body,
                    ..
                },
                Some(request),
            ) => {
                budget.consume(body.len() as u64)?;
                let expected = request.effective_wire(*transport, *dialect);
                budget.charge(&expected)?;
                if serde_json::to_string(&expected)? != *body {
                    return Err(invalid(
                        "prepared provider body disagrees with logical dispatch",
                    ));
                }
                replay.prepared_body_checked = true;
            }
            _ => {
                replay.gaps.insert(CausalGap::WireUnavailable);
            }
        }
        replay.outcome = RecordedStatus::Served;
        Ok(Some(outcome))
    }
}

#[derive(Deserialize)]
struct ToolReceipt {
    task: TaskId,
    job: Uuid,
    session: SessionId,
    request: Uuid,
    status: JobStatus,
    generation: Option<u64>,
    environment: Option<Digest>,
    subagent: Option<Uuid>,
    call: Option<Uuid>,
    tool_call: Option<String>,
    call_id: Option<String>,
    tool_result: Value,
}

struct RecordedTools<'a> {
    bundle: &'a TraceBundle,
}

impl RecordedTools<'_> {
    fn result(
        &self,
        task: TaskId,
        job: &Job,
        replay: &mut ToolReplay,
        budget: &mut MaterializationBudget,
    ) -> Result<Option<Value>, TraceError> {
        let Some(digest) = job.execution_receipt else {
            replay.gaps.insert(CausalGap::ToolResultUnavailable);
            return Ok(None);
        };
        let Some(value) = artifact_json(self.bundle, digest, budget)? else {
            replay.gaps.insert(CausalGap::ToolResultUnavailable);
            return Ok(None);
        };
        // Verification/fence receipts and older opaque formats are not tool outputs.
        if value.get("tool_result").is_none() {
            replay.gaps.insert(CausalGap::ToolResultUnavailable);
            return Ok(None);
        }
        let receipt: ToolReceipt = serde_json::from_value(value)?;
        if (receipt.task, receipt.job, receipt.status)
            != (task, job.id, replay.settlement_status.unwrap_or(job.status))
            || receipt
                .generation
                .is_some_and(|generation| generation != job.generation)
        {
            return Err(invalid(
                "tool receipt task/job/status/generation disagrees with journal",
            ));
        }
        if let Some(invocation) = &job.invocation
            && (receipt.session != invocation.session
                || receipt.request != invocation.request
                || receipt
                    .environment
                    .is_some_and(|environment| environment != invocation.environment)
                || (invocation.call_id.is_some() && receipt.call_id != invocation.call_id))
        {
            return Err(invalid(
                "tool receipt session/request/environment/call disagrees with invocation",
            ));
        }
        if replay.child.is_some() {
            if receipt.subagent != replay.child
                || receipt.call != replay.call
                || receipt.tool_call != replay.tool_call
            {
                return Err(invalid(
                    "tool receipt child/call/tool identity disagrees with dispatch",
                ));
            }
        } else if receipt.subagent.is_some() {
            replay.gaps.insert(CausalGap::ToolLinkUnavailable);
        }
        replay.result = RecordedStatus::Served;
        Ok(Some(receipt.tool_result))
    }
}

pub(super) fn replay(
    bundle: &TraceBundle,
    report: &ReplayReport,
    budget: &mut MaterializationBudget,
) -> Result<CausalReplay, TraceError> {
    let mut audit = CausalReplay::default();
    let mut data = BTreeMap::<Uuid, CallData<'_>>::new();
    let mut tool_links = BTreeMap::<Uuid, (u64, &Value)>::new();
    let mut job_starts = BTreeMap::new();
    let mut job_settlements = BTreeMap::new();
    let mut child_terminals = Vec::new();
    for (task, state) in &report.tasks {
        for call in state
            .model_reservations
            .iter()
            .chain(state.model_receipts.keys())
        {
            bind_task(audit.calls.entry(*call).or_default(), *task)?;
            data.entry(*call).or_default().receipt = state.model_receipts.get(call);
        }
    }
    for span in &report.spans {
        let value = &span["span"];
        let sequence = span["sequence"]
            .as_u64()
            .ok_or_else(|| invalid("span sequence"))?;
        if span["event"]["type"] == "job_started" {
            let job: Uuid = Deserialize::deserialize(&span["event"]["data"]["id"])?;
            job_starts.insert(job, sequence);
        } else if span["event"]["type"] == "job_settled" {
            let job: Uuid = Deserialize::deserialize(&span["event"]["data"]["id"])?;
            let status: JobStatus = Deserialize::deserialize(&span["event"]["data"]["status"])?;
            job_settlements.insert(job, (sequence, status));
        }
        match value["kind"].as_str() {
            Some("model_dispatch" | "model_response") if span.get("session").is_some() => {
                let link: Link = Deserialize::deserialize(value)?;
                let session: SessionId = Deserialize::deserialize(&span["session"])?;
                validate_journal_link(bundle, sequence, session, value, budget)?;
                let state = &report.sessions[&session];
                if state
                    .tasks_by_request
                    .get(&link.request)
                    .is_some_and(|task| *task != link.task)
                {
                    return Err(invalid("receipt task disagrees with session request"));
                }
                let call = audit.calls.entry(link.call).or_default();
                if !state.tasks_by_request.contains_key(&link.request)
                    || !report.tasks.contains_key(&link.task)
                {
                    call.gaps.insert(CausalGap::TaskLinkUnavailable);
                }
                bind_task(call, link.task)?;
                if call.session.is_some_and(|id| id != link.session)
                    || call.request.is_some_and(|id| id != link.request)
                    || (data
                        .get(&link.call)
                        .is_some_and(|v| v.dispatch.is_some() || v.outcome.is_some())
                        && call.child != link.child)
                {
                    return Err(invalid("call/child identity disagrees across receipts"));
                }
                call.session = Some(session);
                call.request = Some(link.request);
                call.child = link.child;
                let entry = data.entry(link.call).or_default();
                if value["kind"] == "model_dispatch" {
                    if entry.dispatch.replace(value).is_some() {
                        return Err(invalid("duplicate model dispatch"));
                    }
                    call.dispatch_sequence = Some(sequence);
                } else {
                    if entry.outcome.replace(&value["outcome"]).is_some() {
                        return Err(invalid("duplicate model outcome"));
                    }
                    entry.outcome_sequence = Some(sequence);
                }
            }
            Some("model_response") => {
                let call: Uuid = Deserialize::deserialize(&value["call"])?;
                let entry = data.entry(call).or_default();
                if entry.outcome_sequence.is_some() {
                    return Err(invalid("duplicate model outcome"));
                }
                let body = &value["report"];
                entry.report = (!body.is_null()).then_some(body);
                entry.outcome = body.get("outcome");
                entry.outcome_sequence = Some(sequence);
            }
            Some("child_terminal") => {
                let session: SessionId = Deserialize::deserialize(&span["session"])?;
                validate_journal_link(bundle, sequence, session, value, budget)?;
                child_terminals.push(value);
            }
            Some("tool_dispatch") => {
                let session: SessionId = Deserialize::deserialize(&span["session"])?;
                validate_journal_link(bundle, sequence, session, value, budget)?;
                let job: Uuid = Deserialize::deserialize(&value["job"])?;
                if tool_links.insert(job, (sequence, value)).is_some() {
                    return Err(invalid("duplicate tool dispatch"));
                }
            }
            _ => {}
        }
    }
    for terminal in child_terminals {
        let child: Uuid = Deserialize::deserialize(&terminal["child"])?;
        let mut linked = false;
        for call in audit
            .calls
            .values()
            .filter(|call| call.child == Some(child))
        {
            linked = true;
            equal_required(terminal, "session", &serde_json::to_value(call.session)?)?;
            equal_required(terminal, "request", &serde_json::to_value(call.request)?)?;
            equal_required(terminal, "task", &serde_json::to_value(call.task)?)?;
        }
        if !linked {
            audit.gaps.insert(CausalGap::ChildCallsUnavailable);
        }
    }
    let mut proposals = BTreeMap::<(SessionId, Option<Uuid>, String), (Uuid, ToolProposal)>::new();
    let mut contexts = BTreeMap::new();
    let provider = RecordedProvider { bundle };
    for (id, entry) in &data {
        let call = audit.calls.entry(*id).or_default();
        if call.child.is_some() {
            call.gaps.insert(CausalGap::ChildOriginUnavailable);
        }
        if let Some(outcome) = provider.respond(entry, call, budget)?
            && let (Some(session), Some(response)) = (call.session, outcome.response)
        {
            for item in response.output {
                if let OutputItem::ToolProposal(proposal) = item {
                    let key = (session, call.child, proposal.call_id.clone());
                    if proposals.insert(key, (*id, proposal)).is_some() {
                        return Err(invalid("ambiguous provider tool call identity"));
                    }
                }
            }
        }
        if call.child.is_none() {
            if let Some(context) = entry.report.and_then(|r| r.get("context")) {
                budget.charge(context)?;
                let manifest: Manifest = Deserialize::deserialize(context)?;
                if call.session.is_some_and(|s| s != manifest.source.session) {
                    return Err(invalid("context source session disagrees with dispatch"));
                }
                contexts
                    .entry((manifest.source.session, manifest.source.revision))
                    .or_insert_with(Vec::new)
                    .push((*id, manifest));
            } else {
                call.gaps.insert(CausalGap::ContextMissing);
            }
        }
    }
    validate_events(
        bundle,
        report,
        &data,
        &proposals,
        &mut audit,
        &mut contexts,
        budget,
    )?;
    for (task, state) in &report.tasks {
        for job in state.jobs.values() {
            // Check jobs use their evidence/certificate reducers, not tool stubs.
            if job.check.is_some() {
                continue;
            }
            let mut replay = ToolReplay {
                task: *task,
                call: None,
                child: None,
                tool_call: job.invocation.as_ref().and_then(|v| v.call_id.clone()),
                status: job.status,
                settlement_status: job_settlements.get(&job.id).map(|(_, status)| *status),
                result: RecordedStatus::Unavailable,
                gaps: BTreeSet::new(),
            };
            if let Some((sequence, link)) = tool_links.remove(&job.id) {
                if job_starts
                    .get(&job.id)
                    .is_some_and(|start| *start >= sequence)
                    || job_settlements
                        .get(&job.id)
                        .is_some_and(|(settled, _)| *settled <= sequence)
                {
                    return Err(invalid("tool dispatch disagrees with job event order"));
                }
                equal_required(link, "task", &serde_json::to_value(task)?)?;
                equal_required(link, "generation", &serde_json::to_value(job.generation)?)?;
                replay.call = Deserialize::deserialize(&link["call"])?;
                replay.child = Deserialize::deserialize(&link["child"])?;
                replay.tool_call = Deserialize::deserialize(&link["tool_call"])?;
                if let Some(invocation) = &job.invocation {
                    equal_required(link, "session", &serde_json::to_value(invocation.session)?)?;
                    equal_required(link, "request", &serde_json::to_value(invocation.request)?)?;
                    equal_required(link, "name", &Value::String(invocation.capability.clone()))?;
                }
            }
            if let Some(invocation) = &job.invocation {
                if let Some(owner) = replay.call.and_then(|call| audit.calls.get(&call))
                    && (owner.session.is_some_and(|s| s != invocation.session)
                        || owner.request.is_some_and(|r| r != invocation.request)
                        || owner.task.is_some_and(|t| t != *task)
                        || (owner.dispatch_sequence.is_some() && owner.child != replay.child))
                {
                    return Err(invalid(
                        "tool link disagrees with model call/child identity",
                    ));
                }
                let proposal = replay
                    .tool_call
                    .as_ref()
                    .and_then(|id| proposals.get(&(invocation.session, replay.child, id.clone())));
                if let Some((call, proposal)) = proposal {
                    if replay.call.is_some_and(|id| id != *call) {
                        return Err(invalid("tool dispatch names a different model call"));
                    }
                    replay.call = Some(*call);
                    if data[call]
                        .outcome_sequence
                        .zip(job_starts.get(&job.id))
                        .is_some_and(|(outcome, start)| outcome >= *start)
                    {
                        return Err(invalid("tool job precedes recorded provider proposal"));
                    }
                    let owner = &audit.calls[call];
                    if owner.task != Some(*task) || owner.request != Some(invocation.request) {
                        return Err(invalid(
                            "tool invocation disagrees with model call task/request",
                        ));
                    }
                    if proposal.name != invocation.capability {
                        return Err(invalid(
                            "tool invocation capability disagrees with proposal",
                        ));
                    }
                    if let Some(input) = artifact_json(bundle, invocation.input, budget)? {
                        equal_required(&input, "name", &Value::String(proposal.name.clone()))?;
                        let arguments: Value =
                            serde_json::from_str(&proposal.arguments).map_err(|_| {
                                invalid("admitted tool proposal arguments are malformed")
                            })?;
                        equal_required(&input, "arguments", &arguments)?;
                    } else {
                        replay.gaps.insert(CausalGap::InvocationUnavailable);
                    }
                } else {
                    if replay
                        .call
                        .and_then(|id| data.get(&id))
                        .is_some_and(|entry| entry.outcome.is_some())
                    {
                        return Err(invalid(
                            "tool call is absent from recorded provider proposals",
                        ));
                    }
                    replay.gaps.insert(CausalGap::ProposalUnavailable);
                }
            } else {
                replay.gaps.insert(CausalGap::InvocationUnavailable);
            }
            if replay.call.is_none() || replay.tool_call.is_none() {
                replay.gaps.insert(CausalGap::ToolLinkUnavailable);
            }
            let tools = RecordedTools { bundle };
            if let Some(result) = tools.result(*task, job, &mut replay, budget)? {
                budget.charge(&result)?;
                let digest = Digest::of(serde_json::to_string(&result)?.as_bytes());
                if replay.child.is_none()
                    && let Some(invocation) = &job.invocation
                    && let Some(call_id) = &invocation.call_id
                    && let Some(recorded) = report
                        .sessions
                        .get(&invocation.session)
                        .and_then(|s| s.tool_calls.get(call_id))
                        .and_then(|t| t.output)
                    && digest != recorded
                {
                    return Err(invalid(
                        "recorded tool output disagrees with settled job result",
                    ));
                }
                if let (Some(call), Some(tool_call)) = (replay.call, &replay.tool_call) {
                    audit
                        .calls
                        .entry(call)
                        .or_default()
                        .tool_outputs
                        .insert(tool_call.clone(), digest);
                }
            }
            if let Some(call) = replay.call {
                audit
                    .calls
                    .entry(call)
                    .or_default()
                    .gaps
                    .extend(replay.gaps.iter().copied());
            }
            audit.tools.insert(job.id, replay);
        }
    }
    if !tool_links.is_empty() {
        return Err(invalid("tool dispatch references an absent job"));
    }
    validate_child_inputs(bundle, &data, &mut audit, budget)?;
    for (id, call) in &mut audit.calls {
        if call.dispatch_sequence.is_none() {
            call.gaps.insert(CausalGap::DispatchMissing);
        }
        if matches!(call.outcome, RecordedStatus::Unavailable) {
            call.gaps
                .insert(if data.get(id).is_some_and(|d| d.receipt.is_some()) {
                    CausalGap::OutcomeUnavailable
                } else {
                    CausalGap::OutcomeMissing
                });
        }
    }
    audit.complete = audit.gaps.is_empty()
        && audit.calls.values().all(|c| c.gaps.is_empty())
        && audit.tools.values().all(|t| t.gaps.is_empty());
    budget.charge(&audit)?;
    Ok(audit)
}

fn validate_child_inputs(
    bundle: &TraceBundle,
    data: &BTreeMap<Uuid, CallData<'_>>,
    audit: &mut CausalReplay,
    budget: &mut MaterializationBudget,
) -> Result<(), TraceError> {
    let mut children = BTreeMap::<Uuid, Vec<(u64, Uuid)>>::new();
    for (id, call) in &audit.calls {
        if let (Some(child), Some(sequence)) = (call.child, call.dispatch_sequence) {
            children.entry(child).or_default().push((sequence, *id));
        }
    }
    for calls in children.values_mut() {
        calls.sort_unstable();
        for pair in calls.windows(2) {
            let previous = pair[0].1;
            let next = pair[1].1;
            let prior: Dispatch = Deserialize::deserialize(data[&previous].dispatch.unwrap())?;
            let following: Dispatch = Deserialize::deserialize(data[&next].dispatch.unwrap())?;
            let Some(prior_input) = artifact_json(bundle, prior.input, budget)? else {
                audit
                    .calls
                    .get_mut(&next)
                    .unwrap()
                    .gaps
                    .insert(CausalGap::ChildContextUnavailable);
                continue;
            };
            let Some(next_input) = artifact_json(bundle, following.input, budget)? else {
                continue;
            };
            let prior_input = prior_input
                .as_array()
                .ok_or_else(|| invalid("child input is not an array"))?;
            let next_input = next_input
                .as_array()
                .ok_or_else(|| invalid("child input is not an array"))?;
            if !next_input.starts_with(prior_input) {
                return Err(invalid(
                    "child dispatch dropped or changed its recorded input prefix",
                ));
            }
            let Some(outcome) = data[&previous].outcome else {
                audit
                    .calls
                    .get_mut(&next)
                    .unwrap()
                    .gaps
                    .insert(CausalGap::ChildContextUnavailable);
                continue;
            };
            if let Some(history) = outcome
                .pointer("/response/history_items")
                .and_then(Value::as_array)
            {
                if !next_input[prior_input.len()..].starts_with(history) {
                    return Err(invalid(
                        "child dispatch disagrees with recorded provider output",
                    ));
                }
                for (tool_call, digest) in &audit.calls[&previous].tool_outputs {
                    let result = next_input[prior_input.len() + history.len()..]
                        .iter()
                        .find(|item| {
                            item["type"] == "function_call_output" && item["call_id"] == *tool_call
                        })
                        .and_then(|item| item["output"].as_str());
                    if result.map(|value| Digest::of(value.as_bytes())) != Some(*digest) {
                        return Err(invalid(
                            "child dispatch disagrees with recorded tool output",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_journal_link(
    bundle: &TraceBundle,
    sequence: u64,
    session: SessionId,
    value: &Value,
    budget: &mut MaterializationBudget,
) -> Result<(), TraceError> {
    equal_required(value, "session", &serde_json::to_value(session)?)?;
    let record = &bundle.records[(sequence - 1) as usize];
    budget.consume(decoded_len(&record.event_base64)?)?;
    let event: SessionEvent = serde_json::from_slice(&decode(&record.event_base64)?)?;
    let SessionEvent::Command {
        command: SessionCommand::TraceRecorded { request, .. },
        ..
    } = event
    else {
        return Err(invalid("receipt has no trace journal command"));
    };
    equal_required(value, "request", &serde_json::to_value(request)?)
}

fn bind_task(call: &mut CallReplay, task: TaskId) -> Result<(), TraceError> {
    if call.task.is_some_and(|id| id != task) {
        return Err(invalid("model call belongs to different tasks"));
    }
    call.task = Some(task);
    Ok(())
}
fn equal_required(value: &Value, key: &str, expected: &Value) -> Result<(), TraceError> {
    if value.get(key) != Some(expected) {
        return Err(invalid(format!(
            "recorded {key} disagrees with causal source"
        )));
    }
    Ok(())
}
fn equal_if_present(value: &Value, key: &str, expected: &Value) -> Result<(), TraceError> {
    if value.get(key).is_some() {
        equal_required(value, key, expected)?;
    }
    Ok(())
}
fn artifact_bytes(
    bundle: &TraceBundle,
    digest: Digest,
    budget: &mut MaterializationBudget,
) -> Result<Option<Vec<u8>>, TraceError> {
    match bundle.artifacts.get(&digest) {
        Some(Payload::Present(encoded)) => {
            budget.consume(decoded_len(encoded)?)?;
            Ok(Some(decode(encoded)?))
        }
        _ => Ok(None),
    }
}
fn artifact_json(
    bundle: &TraceBundle,
    digest: Digest,
    budget: &mut MaterializationBudget,
) -> Result<Option<Value>, TraceError> {
    artifact_bytes(bundle, digest, budget)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(TraceError::from))
        .transpose()
}

fn validate_events(
    bundle: &TraceBundle,
    report: &ReplayReport,
    data: &BTreeMap<Uuid, CallData<'_>>,
    proposals: &BTreeMap<(SessionId, Option<Uuid>, String), (Uuid, ToolProposal)>,
    audit: &mut CausalReplay,
    contexts: &mut BTreeMap<(SessionId, u64), Vec<(Uuid, Manifest)>>,
    budget: &mut MaterializationBudget,
) -> Result<(), TraceError> {
    let mut sessions = BTreeMap::<SessionId, SessionState>::new();
    let responses = data
        .keys()
        .map(|call| (Uuid::new_v5(call, b"response"), *call))
        .collect::<BTreeMap<_, _>>();
    let need_contexts = !contexts.is_empty();
    for record in &bundle.records {
        if record.kind != "session" {
            continue;
        }
        budget.consume(decoded_len(&record.event_base64)?)?;
        let event: SessionEvent = serde_json::from_slice(&decode(&record.event_base64)?)?;
        let id = SessionId(record.aggregate);
        if let SessionEvent::Command {
            operation, command, ..
        } = &event
        {
            match command {
                SessionCommand::ProviderUsage { call: None, .. } => {
                    audit.gaps.insert(CausalGap::UnattributedUsage);
                }
                SessionCommand::ProviderUsage {
                    call: Some(call),
                    request,
                    ..
                }
                | SessionCommand::ProviderCost { call, request, .. }
                | SessionCommand::ContextPrepared { call, request, .. } => {
                    let replay = audit.calls.entry(*call).or_default();
                    if replay.session.is_some_and(|s| s != id)
                        || replay.request.is_some_and(|r| r != *request)
                    {
                        return Err(invalid(
                            "call accounting/context session or request disagrees with dispatch",
                        ));
                    }
                    replay.session = Some(id);
                    replay.request = Some(*request);
                    if let Some(task) = report.sessions[&id].tasks_by_request.get(request) {
                        bind_task(replay, *task)?;
                    }
                    if !data.contains_key(call) {
                        replay.gaps.extend([
                            CausalGap::DispatchMissing,
                            CausalGap::OutcomeMissing,
                            CausalGap::WireUnavailable,
                            CausalGap::ContextMissing,
                        ]);
                    }
                    if let SessionCommand::ProviderUsage { usage, .. } = command
                        && let Some(recorded) = data
                            .get(call)
                            .and_then(|d| d.outcome)
                            .and_then(|o| o.pointer("/response/usage"))
                        && serde_json::to_value(usage)? != *recorded
                    {
                        return Err(invalid("provider usage disagrees with recorded outcome"));
                    }
                }
                SessionCommand::TraceRecorded { record, .. } => {
                    if !matches!(bundle.artifacts.get(record), Some(Payload::Present(_))) {
                        audit.gaps.insert(CausalGap::UnavailableTraceReceipt);
                    }
                }
                SessionCommand::ToolResult {
                    request,
                    call_id,
                    output,
                } => {
                    if let Some((call, _)) = proposals.get(&(id, None, call_id.clone())) {
                        let owner = audit.calls.get_mut(call).unwrap();
                        if owner.request != Some(*request) {
                            return Err(invalid(
                                "tool output request disagrees with provider proposal",
                            ));
                        }
                        let digest = Digest::of(output.as_bytes());
                        if owner
                            .tool_outputs
                            .insert(call_id.clone(), digest)
                            .is_some_and(|old| old != digest)
                        {
                            return Err(invalid("conflicting recorded tool outputs"));
                        }
                    } else {
                        audit.gaps.insert(CausalGap::ToolLinkUnavailable);
                    }
                }
                SessionCommand::Response { items, .. } => {
                    if !responses.contains_key(operation) {
                        audit.gaps.insert(CausalGap::ResponseLinkUnavailable);
                    }
                    if let Some(call) = responses.get(operation)
                        && let Some(recorded) = data[call]
                            .outcome
                            .and_then(|o| o.pointer("/response/history_items"))
                        && serde_json::to_value(items)? != *recorded
                    {
                        return Err(invalid(
                            "session response disagrees with recorded provider output",
                        ));
                    }
                }
                _ => {}
            }
        }
        if !need_contexts {
            continue;
        }
        match event {
            SessionEvent::Created {
                branch,
                config,
                admission,
                parent,
                history,
                at_ms,
                imported,
            } => {
                sessions.insert(
                    id,
                    SessionState::create(
                        id,
                        SessionCreation {
                            branch,
                            config,
                            admission: admission.map(|p| *p),
                            parent,
                            history,
                            started_ms: at_ms,
                            imported: imported.map(|p| *p),
                        },
                    ),
                );
            }
            SessionEvent::Command {
                operation, command, ..
            } => {
                sessions
                    .get_mut(&id)
                    .ok_or_else(|| invalid("context source session absent"))?
                    .apply(operation, &command)?;
            }
        }
        let state = &sessions[&id];
        let Some(views) = contexts.remove(&(id, state.revision)) else {
            continue;
        };
        for (call, manifest) in views {
            if audit.calls[&call]
                .dispatch_sequence
                .is_some_and(|dispatch| record.sequence >= dispatch)
            {
                return Err(invalid("recorded context source does not precede dispatch"));
            }
            budget.charge(&state.history)?;
            let projected = crate::context::project(state, manifest.byte_limit)
                .map_err(|_| invalid("recorded context cannot be projected from source"))?;
            let view = ContextView {
                manifest,
                input: projected.input,
            };
            budget.charge(&view)?;
            if !view.valid_for(state) {
                return Err(invalid("report context disagrees with recorded source"));
            }
            let Some(dispatch) = data[&call].dispatch else {
                continue;
            };
            let dispatch: Dispatch = Deserialize::deserialize(dispatch)?;
            let Some(input) = artifact_json(bundle, dispatch.input, budget)? else {
                continue;
            };
            let input = input
                .as_array()
                .ok_or_else(|| invalid("dispatch input is not an array"))?;
            if input.len() != view.input.len() {
                return Err(invalid("dispatch input length disagrees with context"));
            }
            for (index, (expected, actual)) in view.input.iter().zip(input).enumerate() {
                let bitmap = view.manifest.segments.iter().any(|segment| {
                    matches!(segment.representation, ContextRepresentation::Bitmap(_))
                        && segment.input_range.start <= index as u64
                        && (index as u64) < segment.input_range.end
                });
                if bitmap || contains_materialized_reference(expected) {
                    audit
                        .calls
                        .get_mut(&call)
                        .unwrap()
                        .gaps
                        .insert(CausalGap::ContextMaterializationUnavailable);
                } else if expected != actual {
                    return Err(invalid("dispatch input disagrees with selected context"));
                }
            }
            let stable = view
                .manifest
                .segments
                .iter()
                .filter(|s| s.role == ContextSegmentRole::StableHistory)
                .map(|segment| {
                    let range = usize::try_from(segment.input_range.start)
                        .ok()
                        .zip(usize::try_from(segment.input_range.end).ok())
                        .and_then(|(start, end)| input.get(start..end))
                        .ok_or_else(|| invalid("context stable input range"))?;
                    Ok(Digest::of_value(range)?)
                })
                .collect::<Result<Vec<_>, TraceError>>()?;
            if stable != dispatch.cache.stable_segments {
                return Err(invalid(
                    "cache stable segments disagree with selected context",
                ));
            }
        }
    }
    for views in contexts.values() {
        for (call, _) in views {
            audit
                .calls
                .get_mut(call)
                .unwrap()
                .gaps
                .insert(CausalGap::ContextSourceUnavailable);
        }
    }
    if report.tasks.values().any(|t| !t.usage_receipts.is_empty()) {
        audit.gaps.insert(CausalGap::UnattributedUsage);
    }
    Ok(())
}

fn contains_materialized_reference(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            matches!(
                fields.get("type").and_then(Value::as_str),
                Some("tact_image" | "tact_review")
            ) || fields.values().any(contains_materialized_reference)
        }
        Value::Array(items) => items.iter().any(contains_materialized_reference),
        _ => false,
    }
}
