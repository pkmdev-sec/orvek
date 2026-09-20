# Executable host examples

These examples run the real CLI and same-user operator IPC against a loopback fake
Responses provider. They use Python 3.11+ and its standard library. No live provider
or notification service is used. Assertions check bytes, state and receipts, not
assistant prose. This is deterministic wiring coverage, not a model-quality test.

## Run

Build `orvek` with `cargo build --locked --package orvek --bin orvek`.
Pass the built binary path as the only argument to each scenario below.
Do not use Python's `-O` flag: it disables assertions.

Native, reconnect and hook scenarios require Unix, but not Docker. The sandbox
scenario also requires local Docker, a locally available `debian:bookworm-slim`
image, and `ORVEK_EXECUTOR_HELPER` pointing to a matching Linux helper. See
[workspace execution](workspace-execution.md) for helper setup. Missing prerequisites
fail; no case silently skips. CI runs all four in the Docker security job.

The shared [fixture](../examples/host-docs/fixture.py) starts a separate foreground
host, local provider, configuration, HOME and workspace for each run. It stops the
host/provider and removes its temporary files on success or failure. Provider
credentials, proxy settings and user configuration are not inherited. Docker
connection settings are retained only for access to the operator's local engine.
Never point an example at your own workspace. Native execution is not a sandbox;
these examples control the provider and use only the temporary workspace.

## Coverage limits

- **Memory scan/read and skill discovery: pending T01.** Configuration and JSON
  shapes in the guides are illustrative, not proof that the host exposes them.
  Add examples that inspect actual tool outputs and persistence after integration.
- **Sanitized trace links: pending T04.** Journal assertions below are not portable
  trace bundles. No trace exports or offline replay claims are made here.
- Reconnect drops an IPC acknowledgement and reconnects a journal watch. It does
  not kill or restart the host.
- The hook writes only a local payload, then exits with code 9. A failed delivery
  must not change task truth. This is not proof of external notification delivery
  or exactly-once effects after a host crash.
- Sandbox checks reuse the arithmetic bug and trusted baseline/candidate probes
  from `crates/harness/tests/controller_execution.rs`, through public IPC rather
  than a second verification implementation. Delivery does not overwrite source.

T11 remains partial until the pending examples and trace links are verified.

## Runnable scenarios

Generated from the complete executable files. Edit the source files, then run
`python3 scripts/check-doc-examples.py`. CI uses `--check` and runs the scenarios.

### Native completion without certification

Classification: **runnable**. [Source](../examples/host-docs/native.py).

Run `python3 examples/host-docs/native.py /absolute/path/to/orvek`.

```python
"""A native write changes live bytes, but is not a verified completion."""
import sys
from fixture import HostFixture, message, tool


def main(binary):
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
    print("PASS native: live bytes changed; FinishedUnverified; no certificate")


if __name__ == "__main__":
    main(sys.argv[1])
```

### Verified sandbox completion

Classification: **runnable**. [Source](../examples/host-docs/sandbox.py).

Run `python3 examples/host-docs/sandbox.py /absolute/path/to/orvek`.

```python
"""Reuse controller_execution.rs's addition case through public operator IPC."""
import hashlib
import json
import sys
from fixture import HostFixture, LIMITS, message, tool


BEFORE = "#!/bin/sh\nprintf '3\\n'\n"
AFTER = "#!/bin/sh\nprintf '%s\\n' \"$(($1 + $2))\"\n"


def main(binary):
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
    print("PASS sandbox: Complete; baseline/candidate checks; certificate; delivered bytes")


if __name__ == "__main__":
    main(sys.argv[1])
```

### Lost acknowledgement and journal reconnect

Classification: **runnable**. [Source](../examples/host-docs/reconnect.py).

Run `python3 examples/host-docs/reconnect.py /absolute/path/to/orvek`.

```python
"""Retry one request after losing its acknowledgement; replay the durable journal."""
import sys
from fixture import HostFixture, LIMITS, POLICY, message, request, send_frame, read_frame, tool


def main(binary):
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
    print("PASS reconnect: one submission/effect; same receipt; exact journal replay")


if __name__ == "__main__":
    main(sys.argv[1])
```

### Local terminal hook delivery

Classification: **runnable**. [Source](../examples/host-docs/completion_hook.py).

Run `python3 examples/host-docs/completion_hook.py /absolute/path/to/orvek`.

```python
"""The configured local hook has durable receipts, separate from task truth."""
import json
import sys
from fixture import HostFixture, message, wait_until


def main(binary):
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
    print("PASS completion hook: one local payload; failed receipt; unchanged task outcome")


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
| [docs/memory.md](../docs/memory.md) #1 | illustrative | Memory host example pending T01. |
| [docs/memory.md](../docs/memory.md) #2 | illustrative | Read shape assumes an existing namespace/key/version; pending T01 wiring example. |
| [docs/memory.md](../docs/memory.md) #3 | illustrative | Write shape assumes an existing remote record and writer permission. |
| [docs/memory.md](../docs/memory.md) #4 | illustrative | Delete shape assumes an existing remote record and writer permission. |
| [docs/memory.md](../docs/memory.md) #5 | external-service | Remote endpoint, workspace and credential are placeholders. |
| [docs/memory.md](../docs/memory.md) #6 | external-service | Mutates remote/local memory and requires a configured service. |
| [docs/performance.md](../docs/performance.md) #1 | illustrative | Benchmark command; measures performance rather than host capability. |
| [docs/sessions.md](../docs/sessions.md) #1 | illustrative | Interactive selector and SESSION_ID need existing user state. |
| [docs/sessions.md](../docs/sessions.md) #2 | external-service | Downloads browser dependencies and installs development assets. |
| [docs/subagents.md](../docs/subagents.md) #1 | illustrative | Configuration fragment; does not drive child admission or outcomes. |
| [docs/tui-scrolling.md](../docs/tui-scrolling.md) #1 | illustrative | Contributor test commands; no terminal interaction is driven here. |
| [docs/workspace-execution.md](../docs/workspace-execution.md) #1 | illustrative | Workspace placeholder and interactive session; native coverage is below. |
| [docs/workspace-execution.md](../docs/workspace-execution.md) #2 | illustrative | Platform-specific manual sandbox setup; executable sandbox coverage is below. |
