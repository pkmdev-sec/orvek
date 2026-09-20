#!/usr/bin/env python3
"""Test real headless processes and host restart: test-host-context.py target/debug/orvek."""
import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading
import uuid


def tool(call_id, name, arguments):
    return {"type": "function_call", "id": "fc_" + call_id, "call_id": call_id,
            "name": name, "arguments": json.dumps(arguments), "status": "completed"}


def final():
    return {"type": "message", "id": "msg_done", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": "Done", "annotations": []}]}


def outputs(request):
    return [json.loads(item["output"]) for item in request["input"]
            if item.get("type") == "function_call_output"]


class Provider(http.server.BaseHTTPRequestHandler):
    requests = []
    errors = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            index = len(self.requests)
            self.requests.append(request)
            tools = {entry["name"] for entry in request["tools"]}
            assert {"memory", "read_skill"} <= tools
            assert "check-note" in request["instructions"]
            assert "BODY-SENTINEL" not in request["instructions"]
            if index in (0, 3):
                result = [tool("scan", "memory", {"operation": "scan", "query": "fixture persistent"})]
            elif index == 1:
                result = [tool("put", "memory", {"operation": "put", "content": "fixture persistent memory uses blue ink"}),
                          tool("skill", "read_skill", {"name": "check-note"})]
            elif index == 2:
                values = outputs(request)
                assert any(value.get("memory", {}).get("content") == "fixture persistent memory uses blue ink" for value in values)
                assert any("FIRST-BODY-SENTINEL" in value.get("content", "") for value in values)
                result = [final()]
            elif index == 4:
                scans = [value for value in outputs(request) if value.get("operation") == "scan"]
                key = scans[-1]["candidates"][0]["key"]
                result = [tool("read", "memory", {"operation": "read", "keys": [key]}),
                          tool("skill", "read_skill", {"name": "check-note"})]
            elif index == 5:
                values = outputs(request)
                assert any(value["memories"][0]["content"] == "fixture persistent memory uses blue ink"
                           for value in values if value.get("operation") == "read")
                assert any("SECOND-BODY-SENTINEL" in value.get("content", "") for value in values)
                assert "Updated catalog description" in request["instructions"]
                result = [final()]
            else:
                raise AssertionError(f"unexpected request {index}")
            event = {"type": "response.completed", "response": {"id": f"resp_{index}",
                     "status": "completed", "output": result,
                     "usage": {"input_tokens": 5, "output_tokens": 1, "total_tokens": 6}}}
            body = ("event: response.completed\ndata: " + json.dumps(event) + "\n\n").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except Exception as error:
            self.errors.append(repr(error))
            self.send_error(500)


def shutdown(root):
    path = root / "host/v1/host.sock"
    if not path.exists():
        return
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(path))
        request = json.dumps({"version": 4, "id": str(uuid.uuid4()), "command": {"type": "shutdown_if_idle"}}).encode()
        connection.sendall(struct.pack(">I", len(request)) + request)
        stream = connection.makefile("rb")
        length = struct.unpack(">I", stream.read(4))[0]
        response = json.loads(stream.read(length))
        assert response == {"type": "shutdown", "data": {"accepted": True}}, response
    # Host teardown releases the owner lock after IPC and watchers stop.
    with (root / "host/v1/owner.lock").open("rb") as owner:
        fcntl.flock(owner, fcntl.LOCK_EX)
        fcntl.flock(owner, fcntl.LOCK_UN)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    binary = parser.parse_args().binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="orvek-context-") as temporary:
        root = Path(temporary)
        source = root / "source"
        source.mkdir()
        skill = root / "skills/check-note/SKILL.md"
        skill.parent.mkdir(parents=True)
        skill.write_text("---\nname: check-note\ndescription: Check fixture notes.\n---\nFIRST-BODY-SENTINEL\n")
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        config = root / "config.toml"
        config.write_text(
            '[auth]\nmode = "api-key"\napi_key_env = "CONTEXT_FIXTURE_KEY"\n'
            f'[agent]\nworkspace = {json.dumps(str(source))}\nexecution = "host"\n'
            f'api_base_url = "http://127.0.0.1:{server.server_port}/v1"\n'
            '[memory]\nenabled = true\n[skills]\nenabled = true\n'
            f'roots = [{json.dumps(str(root / "skills"))}]\n')
        config.chmod(0o600)
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith(("ORVEK_", "TACT_"))}
        environment.update(HOME=str(root), CODEX_HOME=str(root / "codex"), CONTEXT_FIXTURE_KEY="fixture-provider-token")
        sessions = []
        try:
            for run in range(2):
                result = subprocess.run([str(binary), "--config", str(config), "run", "Use check-note and fixture persistent memory"],
                                        env=environment, cwd=source, capture_output=True, text=True, timeout=60)
                assert not Provider.errors, Provider.errors
                assert result.returncode == 0, (result.returncode, result.stderr, result.stdout)
                events = [json.loads(line) for line in result.stdout.splitlines()]
                sessions.append(next(event["data"]["id"] for event in events if event["type"] == "session"))
                assert any(event["type"] == "submission_result" for event in events)
                assert "context_prepared" in result.stdout
                shutdown(root)
                if run == 0:
                    skill.write_text("---\nname: check-note\ndescription: Updated catalog description.\n---\nSECOND-BODY-SENTINEL\n")
            assert sessions[0] != sessions[1]
            assert len(Provider.requests) == 6
            print("PASS: fresh headless sessions, host restart, persistent memory put/scan/read, skill body refresh, recorded manifests")
        finally:
            shutdown(root)
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
