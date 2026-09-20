"""Isolated real CLI/IPC fixture. Uses only Python's standard library."""

import base64
from contextlib import ExitStack
import http.server
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading
import time
import uuid


if not __debug__:
    raise RuntimeError("Run host examples without -O; their assertions are the checks")


LIMITS = {"model_calls": 8, "tokens": 10000, "elapsed_ms": 120000,
          "concurrent_jobs": 2, "artifact_bytes": 16 * 1024 * 1024}
POLICY = {"version": 1, "delivery": "source",
          "profile": {"version": 1, "name": "docs fixture", "checks": {}}}


def message():
    return [{"type": "message", "id": "msg_done", "role": "assistant",
             "status": "completed", "content": [
                 {"type": "output_text", "text": "Done", "annotations": []}]}]


def tool(name, arguments):
    call_id = uuid.uuid4().hex
    return [{"type": "function_call", "id": "fc_" + call_id, "call_id": "call_" + call_id,
             "name": name, "arguments": json.dumps(arguments), "status": "completed"}]


def request(kind, **data):
    command = {"type": kind}
    if data:
        command["data"] = data
    return {"version": 4, "id": str(uuid.uuid4()), "command": command}


def send_frame(stream, value):
    body = json.dumps(value).encode()
    stream.sendall(struct.pack(">I", len(body)) + body)


def read_frame(stream):
    def exact(size):
        result = b""
        while len(result) < size:
            chunk = stream.recv(size - len(result))
            if not chunk:
                raise EOFError("host closed the IPC frame")
            result += chunk
        return result
    size, = struct.unpack(">I", exact(4))
    assert 0 < size <= 8 * 1024 * 1024, size
    return json.loads(exact(size))


def wait_until(check, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        threading.Event().wait(0.02)
    raise TimeoutError("fixture condition did not become true")


class HostFixture:
    def __init__(self, binary, outputs, *, sandbox=False, hook=None, memory=False, skills=None):
        self.binary = str(Path(binary).resolve())
        # A callback receives (index, request) to choose calls from real tool outputs.
        self.outputs = outputs
        self.sandbox = sandbox
        self.hook = hook
        self.memory = memory
        self.skills = skills or {}
        self.requests = []
        self.provider_errors = []
        self.stack = ExitStack()

    def __enter__(self):
        try:
            # Short paths also fit macOS Unix socket limits.
            self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory(prefix="ov-doc-", dir="/tmp")))
            self.workspace = self.root / "workspace"
            self.workspace.mkdir()
            self.home = self.root / "home"
            self.home.mkdir()
            self.config = self.root / "config.toml"
            self.socket = self.root / "host/v1/host.sock"
            fixture = self

            class Provider(http.server.BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass

                def do_POST(self):
                    index = len(fixture.requests)
                    body = self.rfile.read(int(self.headers["Content-Length"]))
                    try:
                        provider_request = json.loads(body)
                        fixture.requests.append(provider_request)
                        assert self.path == "/v1/responses", self.path
                        output = (fixture.outputs(index, provider_request) if callable(fixture.outputs)
                                  else fixture.outputs[index])
                    except Exception as error:
                        fixture.provider_errors.append(f"request {index}: {error!r}")
                        self.send_error(500)
                        return
                    event = {"type": "response.completed", "response": {
                        "id": f"resp_{index}", "status": "completed",
                        "output": output,
                        "usage": {"input_tokens": 5, "output_tokens": 1, "total_tokens": 6}}}
                    data = ("event: response.completed\ndata: " + json.dumps(event) + "\n\n").encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)

            self.provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
            self.stack.callback(self.provider.server_close)
            self.provider_thread = threading.Thread(target=self.provider.serve_forever, daemon=True)
            self.provider_thread.start()
            self.stack.callback(self.provider_thread.join, 5)
            self.stack.callback(self.provider.shutdown)
            config = (f'[agent]\nexecution = "{"sandbox" if self.sandbox else "host"}"\n'
                      f'api_base_url = "http://127.0.0.1:{self.provider.server_port}/v1"\n'
                      f'workspace = {json.dumps(str(self.workspace))}\n'
                      'web_search = false\nimage_generation = false\n')
            if self.hook:
                config += f'completion_hook = {json.dumps(self.hook)}\n'
            config += f'[memory]\nenabled = {str(self.memory).lower()}\n'
            config += f'[skills]\nenabled = {str(bool(self.skills)).lower()}\n'
            if self.skills:
                skills_root = self.root / "skills"
                for name, content in self.skills.items():
                    assert name and Path(name).name == name and name not in (".", ".."), name
                    skill = skills_root / name / "SKILL.md"
                    skill.parent.mkdir(parents=True)
                    skill.write_text(content, encoding="utf-8")
                config += f'roots = [{json.dumps(str(skills_root))}]\n'
            self.config.write_text(config)
            self.config.chmod(0o600)
            # Do not inherit user configuration, provider credentials, or proxy settings.
            self.env = {key: os.environ[key] for key in (
                "PATH", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG",
                "ORVEK_EXECUTOR_HELPER") if key in os.environ}
            if "ORVEK_EXECUTOR_HELPER" in self.env:
                self.env["ORVEK_EXECUTOR_HELPER"] = str(Path(self.env["ORVEK_EXECUTOR_HELPER"]).resolve())
            self.env.update(HOME=str(self.home), XDG_CONFIG_HOME=str(self.home),
                            OPENAI_API_KEY="docs-fixture-not-a-secret", NO_PROXY="127.0.0.1,localhost")
            self.command = [self.binary, "--config", str(self.config), "--auth", "api-key",
                            "--workspace", str(self.workspace)]
            self.log = self.stack.enter_context((self.root / "host.log").open("w+"))
            self.host = subprocess.Popen(self.command + ["host"], cwd=self.workspace,
                                         env=self.env, stdin=subprocess.DEVNULL,
                                         stdout=self.log, stderr=self.log)
            self.stack.callback(self.stop)
            def ready():
                if self.host.poll() is not None:
                    self.log.seek(0)
                    raise RuntimeError(self.log.read())
                return self.socket.exists()
            wait_until(ready)
            assert self.query("info")["protocol_version"] == 4
            return self
        except BaseException:
            self.stack.close()
            raise

    def __exit__(self, *exc):
        self.stack.close()

    def stop(self):
        if self.host.poll() is None:
            self.host.terminate()
            try:
                self.host.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.host.kill()
                self.host.wait(timeout=5)

    def connect(self):
        stream = socket.socket(socket.AF_UNIX)
        stream.settimeout(120)
        try:
            stream.connect(str(self.socket))
        except BaseException:
            stream.close()
            raise
        return stream

    def call(self, value):
        with self.connect() as stream:
            send_frame(stream, value)
            reply = read_frame(stream)
        assert reply["type"] != "error", reply
        return reply["data"]

    def query(self, kind, **data):
        return self.call(request(kind, **data))

    def session(self):
        return self.query("create_session", id=str(uuid.uuid4()), request={
            "workspace": str(self.workspace), "channel": "stable", "context_window_tokens": 272000,
            "model": {"model": "sol", "thinking": "medium",
                      "reasoning_mode": "standard", "fast_mode": False}})["id"]

    def run(self, prompt="Write the local fixture result"):
        result = subprocess.run(self.command + ["run", prompt],
                                cwd=self.workspace, env=self.env, text=True,
                                capture_output=True, timeout=120)
        assert not self.provider_errors, self.provider_errors
        assert result.returncode == 0, (result.stdout, result.stderr)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        assert all(event["protocol"] == "orvek.host" and event["version"] == 1 for event in events)
        session = next(event["data"]["id"] for event in events if event["type"] == "session")
        receipt = next(event["data"] for event in events if event["type"] == "submission_result")
        return session, receipt

    def settled(self, session, submitted):
        def terminal():
            receipt = self.query("submission", session=session, request=submitted["id"])
            return receipt if receipt["status"]["state"] not in ("queued", "running") else None
        return wait_until(terminal, 120)

    def artifact(self, digest):
        body = bytearray()
        while True:
            chunk = self.query("read_artifact", digest=digest, offset=len(body), limit=65536)
            body.extend(base64.b64decode(chunk["data"], validate=True))
            if chunk["next"] is None:
                return bytes(body)

    def journal(self):
        records = []
        while True:
            page = self.query("journal", after=records[-1]["sequence"] if records else 0, limit=256)
            if not page:
                return records
            records.extend(page)

    def assert_provider_consumed(self, expected=None):
        assert not self.provider_errors, self.provider_errors
        if expected is None:
            expected = len(self.outputs)
        assert len(self.requests) == expected, (len(self.requests), expected)

    def export_trace(self, output, *, through=None, omit=()):
        """Export only this isolated host; the caller owns sanitization and publication."""
        command = self.command + ["trace", "export", "--host-root", str(self.root / "host/v1"),
                                  "--output", str(output)]
        if through is not None:
            command += ["--through", str(through)]
        for digest in omit:
            command += ["--omit", digest]
        result = subprocess.run(command, cwd=self.workspace, env=self.env,
                                text=True, capture_output=True, timeout=120)
        assert result.returncode == 0, (result.stdout, result.stderr)
        return json.loads(result.stdout)
