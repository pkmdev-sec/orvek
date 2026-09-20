"""Validate durable Orvek host evidence and convert it to Harbor ATIF."""

from __future__ import annotations

import json
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from harbor.models.agent.context import AgentContext
from harbor.models.trajectories import (
    Agent,
    FinalMetrics,
    Metrics,
    Observation,
    ObservationResult,
    Step,
    ToolCall,
    Trajectory,
)
from harbor.utils.trajectory_utils import format_trajectory_json


PROTOCOL = "orvek.host"
PROTOCOL_VERSION = 1
USAGE_FIELDS = (
    "input_tokens",
    "output_tokens",
    "total_tokens",
    "cached_input_tokens",
    "reasoning_tokens",
)


@dataclass(frozen=True)
class EvidencePolicy:
    minimum_subagents: int
    fail_on_subagent_error: bool
    require_wait: bool


@dataclass(frozen=True)
class DurableEvidence:
    session_id: str
    request_id: str
    session: dict[str, Any]
    final_receipt: dict[str, Any]
    commands: list[dict[str, Any]]
    journal_sequences: list[int]


@dataclass(frozen=True)
class PairedTool:
    call_id: str
    name: str
    arguments: dict[str, Any]
    output: Any
    output_text: str


def populate_context(
    *,
    logs_dir: Path,
    context: AgentContext,
    agent_name: str,
    agent_version: str,
    policy: EvidencePolicy,
) -> None:
    prompts = _read_jsonl(logs_dir / "input.jsonl")
    if len(prompts) != 1 or not isinstance(prompts[0].get("instruction"), str):
        raise RuntimeError("input.jsonl must contain one prompt")

    evidence = _validate_events(_read_jsonl(logs_dir / "events.jsonl"))
    responses = _commands(evidence, "response")
    usage_records = [_usage(command.get("usage")) for command in _commands(evidence, "provider_usage")]
    usage = _aggregate_available_usage(usage_records)
    tools = _paired_tools(evidence)
    orchestration = _orchestration_facts(tools, policy)
    message, reasoning = _assistant_output(responses)

    model_settings = evidence.session.get("model")
    if not isinstance(model_settings, dict):
        raise RuntimeError("session.data.model must be an object")
    model = model_settings.get("model")
    effort = model_settings.get("thinking")
    if model is not None and not isinstance(model, str):
        raise RuntimeError("session.data.model.model must be a string or null")
    if effort is not None and not isinstance(effort, str):
        raise RuntimeError("session.data.model.thinking must be a string or null")

    tool_calls = [
        ToolCall(
            tool_call_id=tool.call_id,
            function_name=tool.name,
            arguments=tool.arguments,
            extra={"authoritative_output": tool.output},
        )
        for tool in tools
    ]
    observations = [
        ObservationResult(
            source_call_id=tool.call_id,
            content=tool.output_text,
            extra={"decoded_output": tool.output},
        )
        for tool in tools
    ]
    metrics = Metrics(
        prompt_tokens=usage["input_tokens"],
        completion_tokens=usage["output_tokens"],
        cached_tokens=usage["cached_input_tokens"],
        cost_usd=None,
        extra={
            "available_usage": usage,
            "provider_usage_records": len(usage_records),
            "cost_usd": None,
        },
    )
    final_status = evidence.final_receipt["status"]
    evidence_metadata = {
        "protocol": PROTOCOL,
        "protocol_version": PROTOCOL_VERSION,
        "session_id": evidence.session_id,
        "submission_request_id": evidence.request_id,
        "journal_sequences": evidence.journal_sequences,
        "terminal_status": final_status,
        "available_usage": usage,
        "provider_usage_records": len(usage_records),
        "tool_calls": len(tools),
        "cost_usd": None,
        "orchestration": orchestration,
    }
    trajectory = Trajectory(
        session_id=evidence.session_id,
        agent=Agent(
            name=agent_name,
            version=agent_version,
            model_name=model,
            extra={"durable_host": evidence_metadata},
        ),
        steps=[
            Step(step_id=1, source="user", message=prompts[0]["instruction"]),
            Step(
                step_id=2,
                source="agent",
                message=message,
                model_name=model,
                reasoning_effort=effort,
                reasoning_content=reasoning or None,
                tool_calls=tool_calls or None,
                observation=Observation(results=observations) if observations else None,
                metrics=metrics if usage_records else None,
                llm_call_count=len(usage_records),
                extra={"durable_host": evidence_metadata},
            ),
        ],
        notes=None,
        final_metrics=FinalMetrics(
            total_prompt_tokens=usage["input_tokens"],
            total_completion_tokens=usage["output_tokens"],
            total_cached_tokens=usage["cached_input_tokens"],
            total_cost_usd=None,
            total_steps=2,
            extra={
                "available_usage": usage,
                "provider_usage_records": len(usage_records),
                "tool_calls": len(tools),
                "cost_usd": None,
                "orchestration": orchestration,
            },
        ),
    )
    (logs_dir / "trajectory.json").write_text(
        format_trajectory_json(trajectory.to_json_dict()), encoding="utf-8"
    )

    context.n_input_tokens = usage["input_tokens"]
    context.n_cache_tokens = usage["cached_input_tokens"]
    context.n_output_tokens = usage["output_tokens"]
    context.cost_usd = None
    context.metadata = evidence_metadata


def _validate_events(events: list[dict[str, Any]]) -> DurableEvidence:
    if not events:
        raise RuntimeError("events.jsonl must not be empty")
    for index, envelope in enumerate(events, start=1):
        if (
            envelope.get("protocol") != PROTOCOL
            or envelope.get("version") != PROTOCOL_VERSION
            or not isinstance(envelope.get("type"), str)
            or "data" not in envelope
        ):
            raise RuntimeError(f"invalid {PROTOCOL} v1 envelope at line {index}")
        if envelope["type"] == "view_gap" or (
            envelope["type"] == "event"
            and isinstance(envelope["data"], dict)
            and envelope["data"].get("type") == "preview_gap"
        ):
            raise RuntimeError("events.jsonl contains view_gap; durable evidence is incomplete")

    sessions = [event for event in events if event["type"] == "session"]
    pending = [event for event in events if event["type"] == "submission_pending"]
    finals = [event for event in events if event["type"] == "submission_result"]
    if len(sessions) != 1 or sessions[0] is not events[0]:
        raise RuntimeError("events.jsonl must start with exactly one session envelope")
    if len(pending) != 1:
        raise RuntimeError("events.jsonl must contain exactly one submission_pending")
    if len(finals) != 1 or finals[0] is not events[-1]:
        raise RuntimeError("events.jsonl must end with exactly one submission_result")
    session = sessions[0]["data"]
    pending_data = pending[0]["data"]
    final_receipt = finals[0]["data"]
    if not isinstance(session, dict) or not isinstance(session.get("id"), str):
        raise RuntimeError("session envelope has no session identity")
    if (
        not isinstance(pending_data, dict)
        or pending_data.get("session") != session["id"]
        or not isinstance(pending_data.get("request"), str)
    ):
        raise RuntimeError("submission_pending does not identify the session and request")
    request_id = pending_data["request"]
    submissions = [event for event in events if event["type"] == "submission"]
    if not submissions:
        raise RuntimeError("events.jsonl has no submission receipt")
    receipts = [*submissions, finals[0]]
    for receipt in receipts:
        data = receipt["data"]
        if not isinstance(data, dict) or data.get("id") != request_id:
            raise RuntimeError("submission receipt identity does not match submission_pending")
        status = data.get("status")
        if not isinstance(status, dict) or not isinstance(status.get("state"), str):
            raise RuntimeError("submission receipt has no valid status")
    status = final_receipt["status"]
    state = status["state"]
    if state in {"queued", "running", "interrupted"}:
        raise RuntimeError(f"submission_result is not a complete durable result: {state}")
    if state not in {"finished", "cancelled"}:
        raise RuntimeError(f"unknown submission_result state: {state}")
    if state == "finished" and status.get("outcome") is None:
        raise RuntimeError("finished submission_result has no task outcome")

    commands: list[dict[str, Any]] = []
    sequences: list[int] = []
    linked_tasks: set[str] = set()
    for event in events:
        if event["type"] != "event":
            continue
        frame = event["data"]
        if not isinstance(frame, dict) or not isinstance(frame.get("type"), str):
            raise RuntimeError("event envelope contains a malformed watch frame")
        if frame["type"] != "journal":
            continue
        record = frame.get("data")
        if not isinstance(record, dict):
            raise RuntimeError("journal watch frame has no record")
        sequence = record.get("sequence")
        if (
            not isinstance(sequence, int)
            or isinstance(sequence, bool)
            or sequence < 1
            or (sequences and sequence <= sequences[-1])
        ):
            raise RuntimeError("journal frame sequences must be strictly monotonic")
        sequences.append(sequence)
        if not isinstance(record.get("revision"), int) or not isinstance(record.get("event"), dict):
            raise RuntimeError(f"malformed journal record at sequence {sequence}")
        kind = record.get("kind")
        aggregate = record.get("aggregate")
        if kind == "task":
            if aggregate not in linked_tasks:
                raise RuntimeError("task journal record was not linked by the root session")
            continue
        if kind != "session" or aggregate != session["id"]:
            raise RuntimeError("journal record is outside the submitted session view")
        session_event = record["event"]
        if session_event.get("type") != "command":
            if session_event.get("type") != "created":
                raise RuntimeError(f"unknown session event at sequence {sequence}")
            continue
        event_data = session_event.get("data")
        if not isinstance(event_data, dict) or not isinstance(event_data.get("command"), dict):
            raise RuntimeError(f"malformed session command at sequence {sequence}")
        command = event_data["command"]
        command_type = command.get("type")
        command_data = command.get("data")
        if not isinstance(command_type, str) or not isinstance(command_data, dict):
            raise RuntimeError(f"malformed session command at sequence {sequence}")
        command = {
            "type": command_type,
            "data": command_data,
            "operation": event_data.get("operation"),
            "sequence": sequence,
        }
        commands.append(command)
        if command_type == "task_linked" and command_data.get("request") == request_id:
            task = command_data.get("task")
            if not isinstance(task, str):
                raise RuntimeError("task_linked command has no task identity")
            linked_tasks.add(task)

    inputs = [
        command
        for command in commands
        if command["type"] == "input" and command["operation"] == request_id
    ]
    if len(inputs) != 1:
        raise RuntimeError("journal must contain exactly one input command for the submission")
    settled = _commands_from(commands, request_id, "turn_settled")
    if len(settled) != 1:
        raise RuntimeError("journal must contain exactly one turn_settled for the submission")
    settlement = settled[0]
    if state == "finished" and (
        settlement.get("outcome") != status.get("outcome")
        or settlement.get("error") != status.get("error")
    ):
        raise RuntimeError("submission_result disagrees with durable turn_settled")
    return DurableEvidence(
        session_id=session["id"],
        request_id=request_id,
        session=session,
        final_receipt=final_receipt,
        commands=commands,
        journal_sequences=sequences,
    )


def _commands(evidence: DurableEvidence, command_type: str) -> list[dict[str, Any]]:
    return _commands_from(evidence.commands, evidence.request_id, command_type)


def _commands_from(
    commands: list[dict[str, Any]], request_id: str, command_type: str
) -> list[dict[str, Any]]:
    return [
        command["data"]
        for command in commands
        if command["type"] == command_type
        and command["data"].get("request") == request_id
    ]


def _paired_tools(evidence: DurableEvidence) -> list[PairedTool]:
    proposals: dict[str, tuple[str, dict[str, Any]]] = {}
    ordered_ids: list[str] = []
    for response in _commands(evidence, "response"):
        items = response.get("items")
        if not isinstance(items, list):
            raise RuntimeError("response command items must be an array")
        for item in items:
            if not isinstance(item, dict) or item.get("type") != "function_call":
                continue
            call_id = item.get("call_id")
            name = item.get("name")
            arguments_text = item.get("arguments")
            if not all(isinstance(value, str) for value in (call_id, name, arguments_text)):
                raise RuntimeError("function_call is missing call_id, name, or arguments")
            try:
                arguments = json.loads(arguments_text)
            except json.JSONDecodeError as error:
                raise RuntimeError(f"function_call {call_id} has malformed arguments") from error
            if not isinstance(arguments, dict):
                raise RuntimeError(f"function_call {call_id} arguments must decode to an object")
            if call_id in proposals:
                raise RuntimeError(f"duplicate function_call ID: {call_id}")
            proposals[call_id] = (name, arguments)
            ordered_ids.append(call_id)

    results: dict[str, tuple[Any, str]] = {}
    for result in _commands(evidence, "tool_result"):
        call_id = result.get("call_id")
        output_text = result.get("output")
        if not isinstance(call_id, str) or not isinstance(output_text, str):
            raise RuntimeError("tool_result is missing call_id or output")
        if call_id not in proposals:
            raise RuntimeError(f"tool_result has no matching function_call: {call_id}")
        if call_id in results:
            raise RuntimeError(f"duplicate tool_result for function_call: {call_id}")
        try:
            output = json.loads(output_text)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"tool_result {call_id} has malformed JSON output") from error
        results[call_id] = (output, output_text)
    missing = [call_id for call_id in ordered_ids if call_id not in results]
    if missing:
        raise RuntimeError(f"function_call has no durable tool_result: {missing[0]}")
    return [
        PairedTool(call_id, proposals[call_id][0], proposals[call_id][1], *results[call_id])
        for call_id in ordered_ids
    ]


def _assistant_output(responses: list[dict[str, Any]]) -> tuple[str, str]:
    messages: list[str] = []
    reasoning: list[str] = []
    for response in responses:
        items = response.get("items")
        if not isinstance(items, list):
            raise RuntimeError("response command items must be an array")
        for item in items:
            if not isinstance(item, dict):
                raise RuntimeError("response command contains a non-object item")
            if item.get("type") == "message" and item.get("role") == "assistant":
                messages.append(_content_text(item.get("content")))
            elif item.get("type") == "reasoning":
                summary = item.get("summary")
                if not isinstance(summary, list):
                    raise RuntimeError("reasoning item summary must be an array")
                reasoning.append(
                    "\n".join(
                        part["text"]
                        for part in summary
                        if isinstance(part, dict) and isinstance(part.get("text"), str)
                    )
                )
    return (messages[-1] if messages else "", "\n".join(reasoning))


def _content_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        raise RuntimeError("assistant message content must be a string or array")
    text: list[str] = []
    for part in content:
        if not isinstance(part, dict):
            continue
        if part.get("type") in {"input_text", "output_text", "text"} and isinstance(part.get("text"), str):
            text.append(part["text"])
        elif part.get("type") == "refusal" and isinstance(part.get("refusal"), str):
            text.append(part["refusal"])
    return "".join(text)


def _usage(value: Any) -> dict[str, int | None]:
    if not isinstance(value, dict):
        raise RuntimeError("provider_usage command has no usage object")
    usage: dict[str, int | None] = {}
    for field in USAGE_FIELDS:
        field_value = value.get(field)
        if field_value is not None and (
            not isinstance(field_value, int)
            or isinstance(field_value, bool)
            or field_value < 0
        ):
            raise RuntimeError(f"provider_usage.{field} must be null or a non-negative integer")
        usage[field] = field_value
    return usage


def _aggregate_available_usage(
    records: list[dict[str, int | None]],
) -> dict[str, int | None]:
    return {
        field: (
            sum(record[field] for record in records if record[field] is not None)
            if records and all(record[field] is not None for record in records)
            else None
        )
        for field in USAGE_FIELDS
    }


def _orchestration_facts(
    tools: list[PairedTool], policy: EvidencePolicy
) -> dict[str, Any]:
    spawned: set[int] = set()
    states: dict[int, str] = {}
    successful_wait_calls = 0
    unproven_results = 0
    for tool in tools:
        if tool.name == "spawn_agent":
            value = tool.output
            if (
                isinstance(value, dict)
                and isinstance(value.get("agent_id"), int)
                and not isinstance(value.get("agent_id"), bool)
                and value["agent_id"] > 0
                and isinstance(value.get("status"), dict)
                and value["status"].get("state") == "running"
            ):
                spawned.add(value["agent_id"])
                states[value["agent_id"]] = "running"
            else:
                unproven_results += 1
        elif tool.name == "wait_agent":
            value = tool.output
            if not (
                isinstance(value, dict)
                and isinstance(value.get("agents"), list)
                and isinstance(value.get("timed_out"), bool)
            ):
                unproven_results += 1
                continue
            if not value["timed_out"]:
                successful_wait_calls += 1
            for report in value["agents"]:
                if not (
                    isinstance(report, dict)
                    and isinstance(report.get("agent_id"), int)
                    and isinstance(report.get("status"), dict)
                    and isinstance(report["status"].get("state"), str)
                ):
                    unproven_results += 1
                    continue
                agent_id = report["agent_id"]
                if agent_id in spawned:
                    states[agent_id] = report["status"]["state"]

    if len(spawned) < policy.minimum_subagents:
        raise RuntimeError(
            f"expected at least {policy.minimum_subagents} durably proven subagents, observed {len(spawned)}"
        )
    if policy.require_wait and spawned and successful_wait_calls == 0:
        raise RuntimeError("subagent wait policy lacks a successful durable wait_agent result")
    failed = sorted(agent_id for agent_id, state in states.items() if state in {"failed", "interrupted"})
    unresolved = sorted(agent_id for agent_id in spawned if states.get(agent_id) not in {"completed", "closed", "failed", "interrupted"})
    if policy.fail_on_subagent_error:
        if unproven_results or unresolved:
            raise RuntimeError("subagent lifecycle policy cannot be proven from durable tool results")
        if failed:
            raise RuntimeError("one or more durably reported subagents failed or were interrupted")
    return {
        "agents_started": len(spawned),
        "agent_ids": sorted(spawned),
        "latest_states": {str(agent_id): states[agent_id] for agent_id in sorted(states)},
        "successful_wait_calls": successful_wait_calls,
        "failed_agent_ids": failed,
        "unresolved_agent_ids": unresolved,
        "unproven_results": unproven_results,
    }


def _read_jsonl(path: Path) -> list[dict[str, Any]]:
    deadline = time.monotonic() + 30.0
    while True:
        try:
            text = path.read_text(encoding="utf-8")
            values = [json.loads(line) for line in text.splitlines() if line.strip()]
            break
        except OSError as error:
            if time.monotonic() >= deadline:
                raise RuntimeError(f"failed to read JSONL from {path}: {error}") from error
            time.sleep(0.05)
        except json.JSONDecodeError as error:
            if text.endswith(("\n", "\r")) or time.monotonic() >= deadline:
                raise RuntimeError(f"failed to read JSONL from {path}: {error}") from error
            time.sleep(0.05)
    if not all(isinstance(value, dict) for value in values):
        raise RuntimeError(f"all JSONL values in {path} must be objects")
    return values
