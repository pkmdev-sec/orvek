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
        program = {"version": 1, "probes": [{"kind": "command", "id": "sum", "command": "sh ./add 2 2",
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
