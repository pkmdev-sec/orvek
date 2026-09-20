#!/usr/bin/env python3
"""Paired real-CLI stress/wiring evidence, NOT live-model quality or cost evidence.

Every model call, including transition proposals and exact retrieval, is included.
Uses T03 parse_log; unknown cache/reasoning/billing measurements stay unknown.
Only the isolated fixture forces frequent transitions. No network outside loopback.
"""
import argparse
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "examples/host-docs"))
sys.path.insert(0, str(ROOT / "evals/snapcompact"))
from fixture import HostFixture, message, tool, wait_until
spec = importlib.util.spec_from_file_location("snap_measurements", ROOT / "evals/snapcompact/run_paired.py")
measurements = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measurements)


def restart(host, enabled):
    host.stop()
    if host.socket.exists():
        host.socket.unlink()
    host.env["ORVEK_EXPERIMENTAL_CONTEXT_TRANSITIONS"] = "1" if enabled else "0"
    host.host = subprocess.Popen(host.command + ["host"], cwd=host.workspace, env=host.env,
                                 stdin=subprocess.DEVNULL, stdout=host.log, stderr=host.log)
    wait_until(lambda: host.socket.exists() or host.host.poll() is not None)
    assert host.host.poll() is None, "host did not restart"
    host.query("info")


def session_state(host, session):
    view = host.query("session",id=session)
    cursor = {"version":1,"session":session,"revision":view["revision"]}
    history = []
    while len(history) < view["history_items"]:
        page = host.query("history",cursor=cursor,start=len(history),limit=64)
        assert page["items"], "history retrieval made no progress"
        history.extend(page["items"])
    transitions = [row["event"]["data"]["command"]["data"]["transition"]
                   for row in host.journal() if row["aggregate"] == session
                   and row["event"].get("data",{}).get("command",{}).get("type") == "context_transition"]
    return {**view,"history":history,"context_transitions":transitions}


def total(values):
    return sum(values) if values and all(value is not None for value in values) else None


def summarize_attempt(host, path, captured, elapsed_ms):
    observed = measurements.parse_log(path)
    commands = [measurements.journal_command(json.loads(line)) for line in path.read_text().splitlines()]
    commands = [command for command in commands if command and command["data"].get("request") == observed["request"]]
    response_tools = {item["call_id"]:item["name"] for command in commands if command["type"] == "response"
                      for item in command["data"]["items"] if item.get("type") == "function_call"}
    receipts = {}
    task = (observed["receipt"] or {}).get("task")
    for line in path.read_text().splitlines():
        event = json.loads(line)
        if event.get("type") != "event" or event["data"].get("type") != "journal":
            continue
        row = event["data"]["data"]
        recorded = row["event"]
        if row["aggregate"] == task and recorded["type"] == "model_call_recorded":
            digest = recorded["data"]["receipt"]["report"]
            raw = host.artifact(digest)
            assert hashlib.sha256(raw).hexdigest() == digest
            artifacts = path.parent / "receipts"
            artifacts.mkdir(exist_ok=True)
            (artifacts / (digest + ".json")).write_bytes(raw)
            receipts[recorded["data"]["operation"]] = json.loads(raw)
    wire_sizes = [len(receipt["outcome"]["request"]["body"].encode())
                  if receipt["outcome"]["request"]["status"] == "prepared" else None
                  for receipt in receipts.values()]
    usage = observed["usage"]
    cached, inputs = usage["cached_input_tokens"], usage["input_tokens"]
    return {"issues": observed["issues"], "log_complete": observed["log_complete"],
            "receipt": observed["receipt"], "usage": usage,
            "provider_receipt_usd": float(observed["cost"]) if observed["cost"] is not None else None,
            "catalog_estimate_usd": None, "usage_origin": "scripted_fixture_not_provider_measurement",
            "uncached_input_tokens": inputs - cached if inputs is not None and cached is not None else None,
            "model_calls": len(captured), "recorded_model_calls":len(receipts), "summary_proposal_calls": sum(name == "transition_context" for name in response_tools.values()),
            "retrieval_calls": observed["retrievals"], "retrieval_failures": None,
            "request_bytes": total(wire_sizes) if len(receipts) == len(captured) else None,
            "request_bytes_method": "exact_prepared_UTF8_bodies_from_recorded_model_call_receipts",
            "cache_identities": {call:receipt["cache"] for call,receipt in receipts.items()},
            "latency_ms": elapsed_ms, "render_ms": None, "peak_memory_bytes": None,
            "retries": observed["retries"], "child_outcomes": observed["child_outcomes"]}


def run_condition(binary, directory, condition, phases, records):
    plan = []
    def provider(index, request):
        assert plan, (index, "unexpected model call")
        response = plan.pop(0)
        names = {tool["name"] for tool in request["tools"]}
        assert ("transition_context" in names) == (condition == "transition")
        return response(request) if callable(response) else response

    with HostFixture(binary, provider) as host:
        restart(host, condition == "transition")
        session = None
        def run(prompt, phase, verify):
            nonlocal session
            attempt = {"schema_version":"orvek-context-transition-eval-v1", "attempt_id":str(uuid.uuid4()),
                       "condition":condition,"phase":phase,"status":"running","acceptance":"pending_live_quality_and_full_cost",
                       "task_digest":hashlib.sha256(prompt.encode()).hexdigest(),
                       "harness_build":hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                       "provider":"scripted_loopback", "quality_claim":False,
                       "dataset":"t07-long-refactor-unicode-v1",
                       "settings":{"model":"sol","thinking":"medium","execution":"host"},
                       "environment":"isolated_HOME_workspace_loopback_scripted_provider",
                       "tool_access":"same_native_host_tools_plus_transition_only_in_treatment",
                       "provider_receipt_usd":None,"uncached_input_tokens":None,"request_bytes":None,
                       "latency_ms":None,"model_calls":None,"summary_proposal_calls":None,"retrieval_calls":None}
            records.write(json.dumps(attempt) + "\n"); records.flush()
            path = directory / f"{condition}-{phase}.jsonl"
            count = len(host.requests)
            start = time.monotonic()
            try:
                command = host.command + (["--resume",session] if session else []) + ["run",prompt]
                with path.open("w") as output:
                    process = subprocess.run(command,cwd=host.workspace,env=host.env,stdout=output,
                                             stderr=subprocess.PIPE,text=True,timeout=120)
                measured = summarize_attempt(host,path,host.requests[count:],round(1000*(time.monotonic()-start)))
                attempt.update(measured,exit_code=process.returncode)
                parsed = measurements.parse_log(path)
                assert process.returncode == 0, (path, process.stderr)
                assert measured["log_complete"],measured
                assert parsed["receipt"]["outcome"] == "finished_unverified", parsed
                session = parsed["session"]
                verify(session_state(host,session))
                assert not plan, "provider script was not consumed"
                assert not host.provider_errors,host.provider_errors
                attempt.update(status="completed",deterministic_checks=True)
            except BaseException as error:
                attempt.update(status="interrupted" if isinstance(error,KeyboardInterrupt) else "failed",
                               deterministic_checks=False,error=str(error))
                raise
            finally:
                records.write(json.dumps(attempt) + "\n"); records.flush()

        expected = {}
        for generation in range(phases):
            needle = f"phase-{generation}:雪🦀é punctuation=\"'[]{{}}\""
            (host.workspace / "research.txt").write_text(needle + "\n" + "research evidence line\n" * 450)
            plan.extend([tool("read_file",{"path":"research.txt"}),message()])
            run("Research the historical evidence before the refactor. Preserve exact Unicode and the original goal.",
                f"{generation}-research",lambda state: None)
            state = session_state(host,session)
            source_item = next(index for index in range(len(state["history"])-1,-1,-1)
                               if state["history"][index].get("type") == "function_call_output")
            source = state["history"][source_item]["output"].encode()
            offset = source.index("雪".encode())+1
            source_range = {"start":source_item-1,"end":source_item+1}
            expected[str(generation)] = needle
            if condition == "transition":
                plan.append(tool("transition_context",{"range":source_range,"purpose":"research complete; implement the refactor",
                            "summary":"Evidence index: use the source retrieval link for exact Unicode. This summary does not prove completion.",
                            "pending_obligations":["Preserve every earlier mapping","Check exact Unicode and public API"]}))
            plan.append(tool("read_context",{"item":source_item,"offset":offset,"byte_limit":5,"search":"🦀"}))
            def check_retrieval(request, expected_bytes=source[offset:offset+5]):
                outputs = [item for item in request["input"] if item.get("type") == "function_call_output"]
                page = json.loads(outputs[-1]["output"])["page"]
                assert base64.b64decode(page["bytes_base64"],validate=True) == expected_bytes
                assert "text" not in page, "fixture must exercise split UTF-8"
                return tool("read_context",{"item":source_item,"offset":0,"byte_limit":24576})
            plan.append(check_retrieval)
            def implement(request, exact=source, mapping=dict(expected)):
                outputs = [item for item in request["input"] if item.get("type") == "function_call_output"]
                assert base64.b64decode(json.loads(outputs[-1]["output"])["page"]["bytes_base64"]) == exact
                assert "Preserve all historical mappings" in request["instructions"]
                code = "def recover():\n    return " + repr(mapping) + "\n"
                program = "from pathlib import Path; Path('candidate.py').write_text(" + repr(code) + "); from candidate import recover; assert recover() == " + repr(mapping)
                import shlex
                return tool("exec_command",{"command":"python3 -c " + shlex.quote(program)})
            plan.extend([implement,message()])
            def verify(state):
                import ast
                tree = ast.parse((host.workspace / "candidate.py").read_text())
                assert ast.literal_eval(tree.body[0].body[0].value) == expected
                transitions = state.get("context_transitions",[])
                assert len(transitions) == (generation+1 if condition == "transition" else 0)
                if transitions:
                    assert transitions[-1]["source_digest"] == hashlib.sha256(
                        json.dumps(state["history"][source_item-1:source_item+1],separators=(",", ":"),ensure_ascii=False,sort_keys=True).encode()).hexdigest()
            run("Refactor candidate.py. Preserve all historical mappings and exact Unicode. Keep recover() as the public API and test it.",
                f"{generation}-implementation",verify)
            # Restart with accepted transitions. No summary call or effect is replayed.
            before = session_state(host,session)
            call_count = len(host.requests)
            restart(host,condition == "transition")
            assert session_state(host,session) == before
            assert len(host.requests) == call_count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--phases",type=int,default=3,help="isolated stress fixture only; never a production trigger")
    args = parser.parse_args()
    if args.phases < 1: parser.error("phases must be positive")
    args.output.mkdir(parents=True,exist_ok=False)
    with (args.output / "attempts.jsonl").open("w") as records:
        for condition in ("native","transition"):
            run_condition(args.binary,args.output,condition,args.phases,records)
    rows = [json.loads(line) for line in (args.output / "attempts.jsonl").read_text().splitlines()]
    final = [row for row in rows if row["status"] != "running"]
    summary = {"acceptance":"pending_live_quality_and_full_cost","quality_evidence":"deterministic_wiring_only","conditions":{}}
    for condition in ("native","transition"):
        selected = [row for row in final if row["condition"] == condition]
        summary["conditions"][condition] = {"attempts":len(selected),"completed":sum(row["status"] == "completed" for row in selected),
            **{field:total([row.get(field) for row in selected]) for field in ("model_calls","summary_proposal_calls","retrieval_calls","request_bytes","latency_ms","provider_receipt_usd","uncached_input_tokens")}}
    (args.output / "summary.json").write_text(json.dumps(summary,indent=2)+"\n")
    print(json.dumps(summary,indent=2))


if __name__ == "__main__":
    main()
