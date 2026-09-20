# Executable host examples

These examples run the real CLI and same-user operator IPC against a loopback fake
Responses provider. They use Python 3.11+ and its standard library. No live provider
or notification service is used. Assertions check bytes, state and receipts, not
assistant prose. This is deterministic wiring coverage, not a model-quality test.

## Run

Build `orvek` with `cargo build --locked --package orvek --bin orvek`.
Pass the built binary path as the only argument to each scenario below.
Ordinary runs do not export traces or upload data.
Do not use Python's `-O` flag: it disables assertions.

Native, reconnect, hook and memory/skill scenarios require Unix, but not Docker.
The sandbox scenario also requires local Docker, a locally available `debian:bookworm-slim`
image, and `ORVEK_EXECUTOR_HELPER` pointing to a matching Linux helper. See
[workspace execution](workspace-execution.md) for helper setup. Missing prerequisites
fail; no case silently skips. CI runs all five in the Docker security job.

The shared [fixture](../examples/host-docs/fixture.py) starts a separate foreground
host, local provider, configuration, HOME and workspace for each run. It stops the
host/provider and removes its temporary files on success or failure. Provider
credentials, proxy settings and user configuration are not inherited. Docker
connection settings are retained only for access to the operator's local engine.
Never point an example at your own workspace. Native execution is not a sandbox;
these examples control the provider and use only the temporary workspace.

## Coverage limits

- The memory/skill example uses two fresh CLI sessions on one host and one isolated
  local store. It checks real scan keys, full reads, explicit skill bytes/digests,
  and stored context manifests. It does not test remote memory or a host restart.
  `scripts/test-host-context.py` separately checks the restart path.
- The linked traces are **sanitized, non-exact review bundles** from these controlled
  fixtures. Every artifact payload is omitted. Original journal events, their hashes,
  fixture-only temporary workspace paths and replay identities remain unchanged.
  They cannot prove missing model/tool payloads or fresh execution. See the
  [sanitization boundary and regeneration commands](trace-bundles.md#documentation-fixture-bundles).
- Reconnect drops an IPC acknowledgement and reconnects a journal watch. It does
  not kill or restart the host.
- The hook writes only a local payload, then exits with code 9. A failed delivery
  must not change task truth. This is not proof of external notification delivery
  or exactly-once effects after a host crash.
- Sandbox checks reuse the arithmetic bug and trusted baseline/candidate probes
  from `crates/harness/tests/controller_execution.rs`, through public IPC rather
  than a second verification implementation. Delivery does not overwrite source.

## Runnable scenarios

Generated from the complete executable files. Edit the source files, then run
`python3 scripts/check-doc-examples.py`. CI uses `--check`, verifies local trace
provenance, and runs all five scenarios with export and replay assertions. The
[trace index](../examples/host-docs/traces/index.json) records source and bundle hashes.
UUIDs and timings vary on regeneration; artifacts need not be byte-identical.

### Native completion without certification

Classification: **runnable**. [Source](../examples/host-docs/native.py).

[Sanitized trace bundle](../examples/host-docs/traces/native.trace.json) (non-exact).

Run `python3 examples/host-docs/native.py /absolute/path/to/orvek`.

```python
"""A native write changes live bytes, but is not a verified completion."""
import sys
from fixture import HostFixture, message, tool


def main(binary, *, export=None):
    outputs = [tool("write_file", {"operation": "replace", "path": "result.txt",
               "expected": {"kind": "absent"}, "content": "local result\n"}), message()]
    with HostFixture(binary, outputs) as host:
        session, receipt = host.run()
        assert receipt["status"]["outcome"] == "finished_unverified", receipt
        task = host.query("task", id=receipt["status"]["task"])
        assert task["outcome"] == "finished_unverified"
        assert task["certificate"] is None and task["evidence"] == 0
        assert (host.workspace / "result.txt").read_text() == "local result\n"
        assert host.query("session", id=session)["outcome"] == "finished_unverified"
        host.assert_provider_consumed()
        if export is not None:
            export(host)
    print("PASS native: live bytes changed; FinishedUnverified; no certificate")


if __name__ == "__main__":
    main(sys.argv[1])
```

### Verified sandbox completion

Classification: **runnable**. [Source](../examples/host-docs/sandbox.py).

[Sanitized trace bundle](../examples/host-docs/traces/sandbox.trace.json) (non-exact).

Run `python3 examples/host-docs/sandbox.py /absolute/path/to/orvek`.

```python
"""Reuse controller_execution.rs's addition case through public operator IPC."""
import hashlib
import json
import sys
from fixture import HostFixture, LIMITS, message, tool


BEFORE = "#!/bin/sh\nprintf '3\\n'\n"
AFTER = "#!/bin/sh\nprintf '%s\\n' \"$(($1 + $2))\"\n"


def main(binary, *, export=None):
    outputs = [message(), tool("write_file", {
        "operation": "replace", "path": "add", "content": AFTER,
        "expected": {"kind": "digest", "digest": hashlib.sha256(BEFORE.encode()).hexdigest()}}), message()]
    with HostFixture(binary, outputs, sandbox=True) as host:
        (host.workspace / "add").write_text(BEFORE)
        (host.workspace / "add").chmod(0o755)
        program = {"version": 1, "probes": [{"kind": "command", "id": "sum", "command": "./add 2 2",
                   "exit_code": 0, "stdout": {"kind": "equals", "text": "4\n"}, "stderr": None}],
                   "control_failure": {"probe": "sum", "stdout": {"kind": "equals", "text": "3\n"}, "stderr": None}}
        verifier = host.query("register_program", program=program)["digest"]
        contract = {"request": "Fix addition", "outcome": "add both arguments", "scope": "add command",
                    "requirements": [{"id": "sum", "behavior": "add both arguments", "checks": ["sum"],
                                      "origin": {"kind": "user", "basis": "Fix addition"}, "depends_on": []}],
                    "checks": {"sum": {"purpose": "observe command output", "kind": "behavior",
                        "verifier": verifier, "command": ["tact-verify"], "timeout_ms": 60000,
                        "minimum_assertions": 2, "control": "baseline_failure", "control_source": None,
                        "baseline": {"kind": "must_pass"}, "flake": {"kind": "reject_any_failure"}}},
                    "protected_behavior": [], "assumptions": [], "open_questions": [],
                    "delivery": "source", "limits": LIMITS}
        run = host.query("execute_contract", session=host.session(), input="Fix addition", contract=contract)
        task = run["task"]
        assert task["outcome"] == "complete", task
        certificate, = task["certificates"]
        assert certificate["source"] == task["candidate"]["source"]
        assert certificate["source"] != task["baseline"]["source"]
        evidence = next(item for item in task["evidence"]
                        if item["job_id"] == certificate["evidence"]["sum"])
        assert evidence["identity"]["source"] == certificate["source"]
        assert evidence["identity"]["verifier"] == verifier
        observation = evidence["observation"]
        assert observation["status"] == "passed" and observation["assertions"] >= 2
        assert observation["control"]["rejected"] and observation["control"]["intended_reason"]
        report = json.loads(host.artifact(observation["report"]))
        assert report["source"] == certificate["source"] and report["control_matched"]
        assert bytes(report["probes"][0]["stdout"]) == b"4\n"
        assert bytes(report["control"][0]["stdout"]) == b"3\n"
        delivery = host.root / "host/v1/deliveries" / task["id"] / certificate["source"] / "add"
        assert delivery.read_text() == AFTER
        assert (host.workspace / "add").read_text() == BEFORE
        host.assert_provider_consumed()
        if export is not None:
            export(host)
    print("PASS sandbox: Complete; baseline/candidate checks; certificate; delivered bytes")


if __name__ == "__main__":
    main(sys.argv[1])
```

### Lost acknowledgement and journal reconnect

Classification: **runnable**. [Source](../examples/host-docs/reconnect.py).

[Sanitized trace bundle](../examples/host-docs/traces/reconnect.trace.json) (non-exact).

Run `python3 examples/host-docs/reconnect.py /absolute/path/to/orvek`.

```python
"""Retry one request after losing its acknowledgement; replay the durable journal."""
import sys
from fixture import HostFixture, LIMITS, POLICY, message, request, send_frame, read_frame, tool


def main(binary, *, export=None):
    outputs = [tool("exec_command", {"command": "printf once >> effects.txt"}), message()]
    with HostFixture(binary, outputs) as host:
        session = host.session()
        submitted = request("submit", session=session,
                            content=[{"type": "input_text", "text": "Record one effect"}],
                            intent={"kind": "new_task", "limits": LIMITS, "policy": POLICY})
        with host.connect() as disconnected:
            send_frame(disconnected, submitted)  # Deliberately discard the acknowledgement.
        host.call(submitted)
        receipt = host.settled(session, submitted)
        assert receipt["status"]["outcome"] == "finished_unverified", receipt
        assert host.call(submitted) == receipt
        assert (host.workspace / "effects.txt").read_text() == "once"
        records = host.journal()
        commands = [record["event"].get("data", {}).get("command", {})
                    for record in records if record["aggregate"] == session]
        admissions = [command["data"] for command in commands if command.get("type") == "submitted"]
        links = [command["data"] for command in commands if command.get("type") == "task_linked"]
        assert len(admissions) == 1 and admissions[0]["id"] == submitted["id"]
        assert links == [{"request": submitted["id"], "task": receipt["status"]["task"]}]
        cursor = records[len(records) // 2]["sequence"]
        expected = [record for record in records if record["sequence"] > cursor]
        with host.connect() as reconnected:
            send_frame(reconnected, request("watch", after=cursor, session=session))
            assert read_frame(reconnected) == {"type": "ready", "data": {"after": cursor}}
            replay = []
            while len(replay) < len(expected):
                frame = read_frame(reconnected)
                if frame["type"] == "journal":
                    replay.append(frame["data"])
        assert replay == expected
        host.assert_provider_consumed()
        if export is not None:
            export(host)
    print("PASS reconnect: one submission/effect; same receipt; exact journal replay")


if __name__ == "__main__":
    main(sys.argv[1])
```

### Local terminal hook delivery

Classification: **runnable**. [Source](../examples/host-docs/completion_hook.py).

[Sanitized trace bundle](../examples/host-docs/traces/completion_hook.trace.json) (non-exact).

Run `python3 examples/host-docs/completion_hook.py /absolute/path/to/orvek`.

```python
"""The configured local hook has durable receipts, separate from task truth."""
import json
import sys
from fixture import HostFixture, message, wait_until


def main(binary, *, export=None):
    # No external notification. Append one payload, then deliberately fail the shell.
    with HostFixture(binary, [message()], hook="cat >> completion.jsonl; exit 9") as host:
        session, receipt = host.run()
        assert receipt["status"]["outcome"] == "finished_unverified", receipt

        def delivered():
            events = [record["event"].get("data", {}).get("command", {})
                      for record in host.journal() if record["aggregate"] == session]
            hooks = [event["data"] for event in events if event.get("type") == "completion_hook"]
            return hooks if any(event["type"] == "finished" for event in hooks) else None

        hooks = wait_until(delivered)
        assert [event["type"] for event in hooks] == ["armed", "claimed", "finished"], hooks
        payload, = [json.loads(line) for line in (host.workspace / "completion.jsonl").read_text().splitlines()]
        assert payload == hooks[1]["data"]["payload"]
        assert payload["session"] == session and payload["request"] == receipt["id"]
        assert payload["task"] == receipt["status"]["task"]
        assert payload["outcome"] == "finished_unverified" and payload["version"] == 1
        assert hooks[2]["data"]["result"] == {"type": "failed", "data": {"exit_code": 9}}
        task = host.query("task", id=payload["task"])
        assert task["outcome"] == "finished_unverified" and task["certificate"] is None
        host.assert_provider_consumed()
        if export is not None:
            export(host)
    print("PASS completion hook: one local payload; failed receipt; unchanged task outcome")


if __name__ == "__main__":
    main(sys.argv[1])
```

### Local memory and on-demand skills across sessions

Classification: **runnable**. [Source](../examples/host-docs/memory_skills.py).

[Sanitized trace bundle](../examples/host-docs/traces/memory_skills.trace.json) (non-exact).

Run `python3 examples/host-docs/memory_skills.py /absolute/path/to/orvek`.

```python
"""Persist a local memory across CLI sessions; load a skill only on explicit request."""
import hashlib
import json
import sys
from fixture import HostFixture, message, tool


MEMORY = "Fixture notebooks use blue ink. Keep each durable finding self-contained for later sessions."
SKILL = "---\nname: check-note\ndescription: Check fixture notes.\n---\nBODY-SENTINEL: use blue ink.\n"


def main(binary, *, export=None):
    stored_key = None

    def respond(index, request):
        nonlocal stored_key
        assert {"memory", "read_skill"} <= {entry["name"] for entry in request["tools"]}
        assert "check-note" in request["instructions"]
        assert "Check fixture notes." in request["instructions"]
        assert "BODY-SENTINEL" not in request["instructions"]
        assert MEMORY not in request["instructions"]
        if index < 5:
            assert "BODY-SENTINEL" not in json.dumps(request)
        outputs = [json.loads(item["output"]) for item in request["input"]
                   if item.get("type") == "function_call_output"]
        if index in (0, 3):
            # Each CLI run starts fresh, without the first session's tool history.
            assert not outputs
            assert MEMORY not in json.dumps(request)
            return tool("memory", {"operation": "scan", "query": "fixture notebooks"})
        if index == 1:
            scan, = outputs
            assert scan["operation"] == "scan" and scan["backend"]["source"] == "local"
            assert scan["abstained"] and scan["candidates"] == []
            return tool("memory", {"operation": "put", "content": MEMORY})
        if index == 2:
            put = outputs[-1]
            assert put["operation"] == "put" and not put["replaced"]
            assert put["backend"]["source"] == "local"
            assert put["memory"]["content"] == MEMORY
            stored_key = put["memory"]["key"]
            assert stored_key["id"] > 0 and stored_key["version"] == 1
            assert "namespace" not in stored_key
            return message()
        if index == 4:
            scan, = outputs
            assert scan["operation"] == "scan" and not scan["abstained"]
            candidate, = scan["candidates"]
            assert candidate["key"] == stored_key
            assert MEMORY.startswith(candidate["preview"])
            assert 0 < len(candidate["preview"].encode()) <= 64
            # Preserve the actual scan key, including its version, unchanged.
            return (tool("memory", {"operation": "read", "keys": [candidate["key"]]})
                    + tool("read_skill", {"name": "check-note"}))
        if index == 5:
            read, = [value for value in outputs if value.get("operation") == "read"]
            assert read["backend"]["source"] == "local"
            record, = read["memories"]
            assert record["key"] == stored_key and record["content"] == MEMORY
            skill, = [value for value in outputs if "digest" in value]
            assert skill["content"].encode() == SKILL.encode()
            assert skill["digest"] == hashlib.sha256(SKILL.encode()).hexdigest()
            assert skill["path"] == str((host.root / "skills/check-note/SKILL.md").resolve())
            return message()
        raise AssertionError(f"unexpected provider request {index}")

    with HostFixture(binary, respond, memory=True, skills={"check-note": SKILL}) as host:
        sessions = [host.run("Remember how fixture notebooks record findings"),
                    host.run("Read the fixture notebooks memory and the check-note skill")]
        assert sessions[0][0] != sessions[1][0]
        assert (host.root / "memory/v1.sqlite3").is_file()
        manifests = []
        calls = set()
        for session, receipt in sessions:
            assert receipt["status"]["outcome"] == "finished_unverified", receipt
            commands = [record["event"].get("data", {}).get("command", {})
                        for record in host.journal() if record["aggregate"] == session]
            prepared = [command["data"] for command in commands
                        if command.get("type") == "context_prepared"]
            assert len(prepared) == 3, prepared
            for event in prepared:
                assert event["request"] == receipt["id"]
                assert event["call"] not in calls
                calls.add(event["call"])
                body = host.artifact(event["manifest"])
                assert hashlib.sha256(body).hexdigest() == event["manifest"]
                manifest = json.loads(body)
                assert manifest["version"] == 1 and manifest["diagnostics"] == []
                assert "check-note" in manifest["skills"]
                assert MEMORY.encode() not in body and b"BODY-SENTINEL" not in body
                assert manifest["memory"]["backend"]["source"] == "local"
                manifests.append(manifest)
        assert manifests[0]["memory"]["keys"] == []
        assert manifests[0] == manifests[1]
        assert manifests[2]["memory"]["keys"] == [stored_key]
        assert all(manifest == manifests[2] for manifest in manifests[3:])
        host.assert_provider_consumed(6)
        if export is not None:
            export(host)
    print("PASS memory/skills: two sessions; put/scan/read; exact skill bytes/digest; context manifests")


if __name__ == "__main__":
    main(sys.argv[1])
```

## Guide inventory

Scope: every fenced block in `README.md` and top-level `docs/*.md`, excluding this
generated page. Design records, the derived codebase graph, and deployment-specific
example READMEs are outside this first inventory. Nothing outside it is claimed as
executed. Block numbers follow source order. Changed or new blocks fail the check
until their classification is reviewed in `examples/host-docs/inventory.json`.
The existing `scripts/check-docs.py` still checks links and syntax separately.

| Guide block | Classification | Reason |
| --- | --- | --- |
| [README.md](../README.md) #1 | external-service | Downloads repository and crates; not isolated host behavior. |
| [README.md](../README.md) #2 | external-service | Requires real authentication and an interactive terminal. |
| [README.md](../README.md) #3 | external-service | Uses a live provider and existing user sessions. |
| [README.md](../README.md) #4 | external-service | Downloads and installs a binary into the user environment. |
| [README.md](../README.md) #5 | illustrative | Contributor checks; not an isolated capability scenario. |
| [docs/compaction.md](../docs/compaction.md) #1 | illustrative | Configuration fragment only; does not exercise context projection. |
| [docs/configuration.md](../docs/configuration.md) #1 | illustrative | Memory host example pending T01; TOML parsing is not wiring proof. |
| [docs/configuration.md](../docs/configuration.md) #2 | external-service | Needs user credentials and authentication service. |
| [docs/configuration.md](../docs/configuration.md) #3 | illustrative | Token-helper placeholder must be replaced by the operator. |
| [docs/configuration.md](../docs/configuration.md) #4 | illustrative | Configuration reference; not a behavior test. |
| [docs/configuration.md](../docs/configuration.md) #5 | illustrative | Handler path is a placeholder; runnable hook coverage is below. |
| [docs/configuration.md](../docs/configuration.md) #6 | illustrative | Visual settings require an interactive terminal. |
| [docs/configuration.md](../docs/configuration.md) #7 | illustrative | Paths are placeholders; host skill discovery example pending T01. |
| [docs/configuration.md](../docs/configuration.md) #8 | external-service | Downloads an MCP server and needs a real workspace path. |
| [docs/configuration.md](../docs/configuration.md) #9 | external-service | Needs an external MCP service and credentials. |
| [docs/harness-host.md](../docs/harness-host.md) #1 | illustrative | Partial Rust call flow with caller-owned variables; reconnect coverage is below. |
| [docs/harness-integration.md](../docs/harness-integration.md) #1 | illustrative | Contributor checks, not a self-contained host task. |
| [docs/memory.md](../docs/memory.md) #1 | illustrative | Configuration fragment; the runnable memory_skills.py scenario enables an isolated local store. |
| [docs/memory.md](../docs/memory.md) #2 | illustrative | Read shape assumes an existing remote namespace/key/version; memory_skills.py instead reads local keys returned by the real scan. |
| [docs/memory.md](../docs/memory.md) #3 | illustrative | Write shape assumes an existing remote record and writer permission. |
| [docs/memory.md](../docs/memory.md) #4 | illustrative | Delete shape assumes an existing remote record and writer permission. |
| [docs/memory.md](../docs/memory.md) #5 | external-service | Remote endpoint, workspace and credential are placeholders. |
| [docs/memory.md](../docs/memory.md) #6 | illustrative | Skill root configuration contains a user-specific path; memory_skills.py verifies an isolated catalog and explicit skill reads. |
| [docs/memory.md](../docs/memory.md) #7 | illustrative | Build-and-run instructions for the standalone host-context verification script; they require a built binary rather than forming a self-contained scenario. |
| [docs/memory.md](../docs/memory.md) #8 | external-service | Push/pull mutates remote/local memory and requires a configured service. |
| [docs/performance.md](../docs/performance.md) #1 | illustrative | Benchmark command; measures performance rather than host capability. |
| [docs/sessions.md](../docs/sessions.md) #1 | illustrative | Interactive selector and SESSION_ID need existing user state. |
| [docs/sessions.md](../docs/sessions.md) #2 | external-service | Downloads browser dependencies and installs development assets. |
| [docs/subagents.md](../docs/subagents.md) #1 | illustrative | Configuration fragment; does not drive child admission or outcomes. |
| [docs/trace-bundles.md](../docs/trace-bundles.md) #1 | illustrative | Commands require a real private host journal or bundle. Isolated trace tests provide those fixtures; these path placeholders are not standalone scenarios. |
| [docs/trace-bundles.md](../docs/trace-bundles.md) #2 | external-service | Experimental reexecution needs configured provider credentials and can run tools or incur cost in a fresh workspace. |
| [docs/trace-bundles.md](../docs/trace-bundles.md) #3 | illustrative | Regeneration and validation commands require a built CLI and local Docker; CI executes the same generator with isolated output. |
| [docs/tui-scrolling.md](../docs/tui-scrolling.md) #1 | illustrative | Contributor test commands; no terminal interaction is driven here. |
| [docs/workspace-execution.md](../docs/workspace-execution.md) #1 | illustrative | Workspace placeholder and interactive session; native coverage is below. |
| [docs/workspace-execution.md](../docs/workspace-execution.md) #2 | illustrative | Platform-specific manual sandbox setup; executable sandbox coverage is below. |
