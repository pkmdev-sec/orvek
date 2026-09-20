"""Persist a local memory across CLI sessions; load a skill only on explicit request."""
import hashlib
import json
import sys
from fixture import HostFixture, message, tool


MEMORY = "Fixture notebooks use blue ink. Keep each durable finding self-contained for later sessions."
SKILL = "---\nname: check-note\ndescription: Check fixture notes.\n---\nBODY-SENTINEL: use blue ink.\n"


def main(binary):
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
    print("PASS memory/skills: two sessions; put/scan/read; exact skill bytes/digest; context manifests")


if __name__ == "__main__":
    main(sys.argv[1])
