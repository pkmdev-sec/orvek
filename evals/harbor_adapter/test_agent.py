"""Contracts for the Orvek Harbor adapter."""

import asyncio
import base64
import json
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

import yaml
from harbor.models.agent.context import AgentContext
from harbor.environments.docker.docker import DockerEnvironment

from harbor_adapter.agent import MODEL, OrvekAgent
from harbor_adapter.credentials import (
    FAKE_ACCESS_TOKEN,
    fake_auth_document,
    read_codex_credentials,
)
from harbor_adapter.evidence import (
    EvidencePolicy,
    populate_context,
)
from harbor_adapter.host_sidecar import HostSidecar, _has_terminal_fence
from harbor_adapter.installation import cli_tools_install_command, executor_helper_path
from harbor_adapter.iron_proxy import (
    IRON_PROXY_IMAGE,
    LocalCodexAuthProxy,
    _run_docker,
)


def jwt(claims: dict[str, object]) -> str:
    def encode(value: dict[str, object]) -> str:
        content = json.dumps(value, separators=(",", ":")).encode()
        return base64.urlsafe_b64encode(content).decode().rstrip("=")

    return f"{encode({'alg': 'none'})}.{encode(claims)}.signature"


def auth_document(suffix: str, expires_at: float | None = None) -> bytes:
    access_token = jwt(
        {
            "exp": expires_at or time.time() + 24 * 60 * 60,
            "suffix": suffix,
        }
    )
    id_token = jwt(
        {
            "https://api.openai.com/auth": {
                "account_id": "account-1",
                "fedramp": False,
            },
            "suffix": suffix,
        }
    )
    return json.dumps(
        {
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": id_token,
                "access_token": access_token,
                "refresh_token": f"refresh-{suffix}",
                "account_id": "account-1",
            },
        }
    ).encode()


class ArgumentContractTests(unittest.TestCase):
    def test_run_uses_durable_headless_mode(self) -> None:
        agent = object.__new__(OrvekAgent)
        agent._effort = "low"
        agent._reasoning_mode = "standard"
        agent._max_subagents = 8
        agent._model = MODEL
        agent._auth_proxy = LocalCodexAuthProxy("unused")

        arguments = agent._run_arguments("- inspect the workspace")

        self.assertEqual(arguments[0:2], ["--model", MODEL])
        self.assertNotIn("--orchestration-log", arguments)
        self.assertEqual(arguments[-2:], ["--", "- inspect the workspace"])

    def test_model_name_must_match_the_orvek_build(self) -> None:
        self.assertEqual(OrvekAgent._api_model_name(f"openai/{MODEL}"), MODEL)
        self.assertEqual(OrvekAgent._api_model_name(MODEL), MODEL)

    def test_user_environment_cannot_override_subscription_auth(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            agent = OrvekAgent(
                logs_dir=Path(directory),
                model_name=f"openai/{MODEL}",
                extra_env={
                    "OPENAI_API_KEY": "must-not-enter-task",
                    "HTTPS_PROXY": "http://wrong-proxy",
                    "UNRELATED": "retained",
                },
            )

        self.assertNotIn("OPENAI_API_KEY", agent.extra_env)
        self.assertNotIn("HTTPS_PROXY", agent.extra_env)
        self.assertEqual(agent.extra_env["UNRELATED"], "retained")

    def test_tool_installer_supports_common_task_images(self) -> None:
        command = cli_tools_install_command(install_node=True)

        for package_manager in ("apk add", "apt-get install", "yum install"):
            self.assertIn(package_manager, command)
        for tool in ("bash", "curl", "rg", "node", "npm"):
            self.assertIn(f"command -v {tool}", command)

class InstallContractTests(unittest.TestCase):
    def test_local_binary_is_uploaded_and_made_executable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "orvek"
            binary.write_bytes(b"binary")
            agent = object.__new__(OrvekAgent)
            agent._binary_path = binary
            agent._install_node = False
            agent._auth_proxy = SimpleNamespace(require_local_docker=lambda _: None)
            agent.exec_as_root = AsyncMock()
            environment = SimpleNamespace(upload_file=AsyncMock())

            asyncio.run(agent.install(environment))

            environment.upload_file.assert_awaited_once_with(
                binary, "/installed-agent/orvek"
            )
        self.assertEqual(
            agent.exec_as_root.await_args_list[-1].args[1],
            "chmod 0755 /installed-agent/orvek",
        )

    def test_executor_helper_matches_committed_image_architecture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "orvek"
            helper = Path(directory) / "orvek-executor-linux-aarch64"
            helper.write_bytes(b"helper")

            self.assertEqual(executor_helper_path(binary, "arm64"), helper.resolve())


class CredentialContractTests(unittest.TestCase):
    def test_remote_environments_are_rejected_before_reading_credentials(self) -> None:
        proxy = LocalCodexAuthProxy("/path/that/does/not/exist")

        with self.assertRaisesRegex(RuntimeError, "local Docker"):
            proxy.require_local_docker(SimpleNamespace())

    def test_remote_docker_daemon_is_rejected(self) -> None:
        proxy = LocalCodexAuthProxy("/path/that/does/not/exist")
        environment = object.__new__(DockerEnvironment)
        environment._is_windows_container = False

        with patch(
            "harbor_adapter.iron_proxy._docker_endpoint",
            return_value="ssh://remote-builder",
        ):
            with self.assertRaisesRegex(RuntimeError, "local Docker socket"):
                proxy.require_local_docker(environment)

    def test_trusted_host_receives_only_fake_auth_and_the_public_ca(self) -> None:
        async def exercise(
            auth_file: Path,
        ) -> tuple[bytes, bytes, list[list[str]]]:
            proxy = LocalCodexAuthProxy(auth_file, safe_bind_root="/safe-trial")
            docker_commands: list[list[str]] = []

            async def docker(arguments: list[str], **_: object) -> SimpleNamespace:
                docker_commands.append(arguments)
                if "generate-ca" in arguments:
                    output = Path(
                        arguments[arguments.index("--volume") + 1].split(":", 1)[0]
                    )
                    (output / "ca.crt").write_text("public-ca", encoding="utf-8")
                    (output / "ca.key").write_text("private-ca", encoding="utf-8")
                if arguments[0] == "inspect":
                    inspection = [
                        {
                            "HostConfig": {
                                "Privileged": False,
                                "PidMode": "",
                                "NetworkMode": "default",
                                "IpcMode": "private",
                                "CapAdd": None,
                                "Devices": [],
                            },
                            "Mounts": [
                                {
                                    "Type": "bind",
                                    "Source": "/safe-trial/agent",
                                    "Destination": "/logs/agent",
                                }
                            ],
                        }
                    ]
                    return SimpleNamespace(
                        returncode=0,
                        stdout=json.dumps(inspection),
                    )
                return SimpleNamespace(returncode=0, stdout="", stderr="")

            with patch("harbor_adapter.iron_proxy._run_docker", side_effect=docker):
                public = Path(tempfile.mkdtemp(dir=auth_file.parent))
                public.rmdir()
                async with proxy.running(
                    host_container="trusted-host", public_directory=public
                ) as environment:
                    self.assertEqual(environment["ORVEK_AUTH_FILE"], str(public / "auth.json"))
                    auth_bytes = (public / "auth.json").read_bytes()
                    ca_bytes = (public / "ca.crt").read_bytes()
            return auth_bytes, ca_bytes, docker_commands

        with tempfile.TemporaryDirectory() as directory:
            auth_file = Path(directory) / "auth.json"
            source = auth_document("real")
            auth_file.write_bytes(source)
            auth_file.chmod(0o600)
            fake_auth, public_ca, docker_commands = asyncio.run(exercise(auth_file))

        source_tokens = json.loads(source)["tokens"]
        public_bytes = fake_auth + public_ca
        self.assertNotIn(source_tokens["access_token"].encode(), public_bytes)
        self.assertNotIn(source_tokens["refresh_token"].encode(), public_bytes)
        self.assertEqual(json.loads(fake_auth), json.loads(fake_auth_document()))

        start = next(command for command in docker_commands if "--detach" in command)
        self.assertIn("container:trusted-host", start)
        self.assertIn(f"{IRON_PROXY_IMAGE}", start)
        self.assertIn(":/run/orvek-auth:ro", " ".join(start))
        self.assertNotIn(source_tokens["access_token"], " ".join(start))
        self.assertEqual(docker_commands[-1][0:2], ["rm", "--force"])

    def test_access_token_must_cover_the_next_trial(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            auth_file = Path(directory) / "auth.json"
            auth_file.write_bytes(auth_document("expired", time.time() - 1))
            auth_file.chmod(0o600)
            proxy = LocalCodexAuthProxy(auth_file)

            with self.assertRaisesRegex(RuntimeError, "expires too soon"):
                read_codex_credentials(
                    proxy.path,
                    minimum_lifetime_seconds=proxy.minimum_lifetime_seconds,
                )

    def test_owned_credentials_are_redacted_and_cleared(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            auth_file = Path(directory) / "auth.json"
            auth_file.write_bytes(auth_document("mutable"))
            auth_file.chmod(0o600)
            credentials = read_codex_credentials(auth_file)

            self.assertEqual(repr(credentials), "CodexCredentials([REDACTED])")
            self.assertTrue(credentials.access_token.startswith(b"ey"))
            credentials.clear()

        self.assertEqual(set(credentials.access_token), {0})
        self.assertEqual(set(credentials.account_id), {0})

    def test_proxy_removal_failure_is_reported(self) -> None:
        async def exercise() -> None:
            proxy = LocalCodexAuthProxy("unused")
            with patch(
                "harbor_adapter.iron_proxy._run_docker",
                AsyncMock(side_effect=RuntimeError("removal failed")),
            ):
                with self.assertRaisesRegex(RuntimeError, "removal failed"):
                    await proxy._cleanup("proxy-container")

        asyncio.run(exercise())

    def test_cancelled_headless_exec_is_interrupted_then_fenced(self) -> None:
        async def exercise() -> None:
            started = asyncio.Event()
            finish = asyncio.Event()

            class Process:
                returncode = None
                signals: list[int] = []

                def send_signal(self, value: int) -> None:
                    self.signals.append(value)

                async def communicate(self) -> tuple[bytes, bytes]:
                    started.set()
                    await finish.wait()
                    self.returncode = 130
                    return b'{}\n', b""

            process = Process()
            with patch(
                "harbor_adapter.iron_proxy.asyncio.create_subprocess_exec",
                AsyncMock(return_value=process),
            ):
                command = asyncio.create_task(
                    _run_docker(
                        ["exec", "host", "orvek", "run"],
                        timeout_seconds=30,
                        interrupt_on_cancel=True,
                    )
                )
                await started.wait()
                command.cancel()
                await asyncio.sleep(0)
                self.assertTrue(process.signals)
                self.assertFalse(command.done())
                finish.set()
                with self.assertRaises(asyncio.CancelledError):
                    await command

        asyncio.run(exercise())


class SidecarContractTests(unittest.TestCase):
    def test_task_validation_is_an_awaitable_preflight(self) -> None:
        async def exercise() -> str:
            environment = SimpleNamespace()
            proxy = LocalCodexAuthProxy("unused")
            proxy.require_local_docker = lambda candidate: candidate
            proxy._main_container_id = AsyncMock(return_value="main")
            proxy._task_container_ids = AsyncMock(return_value=["main", "helper"])
            proxy._require_isolated_task = AsyncMock()

            result = await proxy.validate_task(environment)

            self.assertEqual(proxy._require_isolated_task.await_count, 2)
            return result

        self.assertEqual(asyncio.run(exercise()), "main")

    def test_terminal_fence_requires_typed_single_submission_result(self) -> None:
        envelope = {"protocol": "orvek.host", "version": 1, "type": "session", "data": {}}
        terminal = {
            **envelope,
            "type": "submission_result",
            "data": {"status": {"state": "finished"}},
        }

        self.assertTrue(_has_terminal_fence(json.dumps(envelope) + "\n" + json.dumps(terminal)))
        self.assertFalse(_has_terminal_fence(json.dumps(terminal) + "\n" + json.dumps(terminal)))
        self.assertFalse(_has_terminal_fence(json.dumps(terminal) + "\n" + json.dumps(envelope)))
        interrupted = {
            **terminal,
            "data": {"status": {"state": "interrupted"}},
        }
        self.assertFalse(_has_terminal_fence(json.dumps(interrupted)))

    def test_host_has_socket_and_path_identical_trial_mount_only(self) -> None:
        async def exercise(root: Path) -> list[str]:
            binary = root / "orvek"
            helper = root / "orvek-executor"
            binary.write_bytes(b"binary")
            helper.write_bytes(b"helper")
            sidecar = HostSidecar(
                logs_dir=root / "agent",
                binary_path=binary,
                auth_proxy=LocalCodexAuthProxy("unused"),
            )
            with patch(
                "harbor_adapter.host_sidecar._run_docker", AsyncMock()
            ) as docker:
                await sidecar._start_host_container(
                    container_name="trusted-host",
                    architecture="arm64",
                    root=root,
                    socket=Path("/var/run/docker.sock"),
                    helper_path=helper,
                    environment={"ORVEK_EXECUTOR_IMAGE": "executor:image"},
                )
            return docker.await_args.args[0]

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            arguments = asyncio.run(exercise(root))

        joined = " ".join(arguments)
        self.assertIn(f"src={root},dst={root}", joined)
        self.assertIn("src=/var/run/docker.sock,dst=/var/run/docker.sock", joined)
        self.assertIn("--cap-drop ALL", joined)
        self.assertIn("--platform linux/arm64", joined)
        self.assertNotIn("executor:image -c", joined)
        self.assertIn("debian:bookworm-slim@sha256:", joined)

    def test_unsafe_task_container_is_rejected_before_credentials_are_read(
        self,
    ) -> None:
        async def exercise() -> None:
            inspection = [
                {
                    "HostConfig": {
                        "Privileged": False,
                        "PidMode": "",
                        "NetworkMode": "default",
                        "IpcMode": "private",
                        "CapAdd": None,
                        "Devices": [],
                    },
                    "Mounts": [
                        {
                            "Type": "bind",
                            "Destination": "/var/run/docker.sock",
                        }
                    ],
                }
            ]
            result = SimpleNamespace(stdout=json.dumps(inspection))
            with patch(
                "harbor_adapter.iron_proxy._run_docker",
                AsyncMock(return_value=result),
            ):
                with self.assertRaisesRegex(RuntimeError, "task bind mounts"):
                    await LocalCodexAuthProxy("unused")._require_isolated_task("task")

        asyncio.run(exercise())

    def test_docker_startup_finishes_before_cancellation_returns(self) -> None:
        async def exercise() -> None:
            started = asyncio.Event()
            finish = asyncio.Event()

            class Process:
                returncode = 0

                async def communicate(self) -> tuple[bytes, bytes]:
                    started.set()
                    await finish.wait()
                    return b"container-id", b""

            with patch(
                "harbor_adapter.iron_proxy.asyncio.create_subprocess_exec",
                AsyncMock(return_value=Process()),
            ):
                command = asyncio.create_task(
                    _run_docker(["run", "--detach"], timeout_seconds=30)
                )
                await started.wait()
                command.cancel()
                await asyncio.sleep(0)
                self.assertFalse(command.done())
                finish.set()
                with self.assertRaises(asyncio.CancelledError):
                    await command

        asyncio.run(exercise())

    def test_proxy_config_swaps_mock_headers_only_on_codex_routes(self) -> None:
        proxy = LocalCodexAuthProxy("unused")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            proxy._write_public_files(path)
            config = yaml.safe_load((path / "proxy.yaml").read_text())

        codex_rule = {
            "host": "chatgpt.com",
            "methods": ["GET", "POST"],
            "paths": [
                "/backend-api/codex",
                "/backend-api/codex/*",
            ],
        }
        allowlist = config["transforms"][0]["config"]["rules"]
        secrets = config["transforms"][1]["config"]["secrets"]
        self.assertEqual(
            allowlist,
            [
                {"host": "chatgpt.com", "methods": ["CONNECT"]},
                codex_rule,
            ],
        )
        self.assertEqual(secrets[0]["replace"]["proxy_value"], FAKE_ACCESS_TOKEN)
        self.assertTrue(secrets[0]["replace"]["require"])
        self.assertEqual(secrets[0]["source"]["type"], "file")
        self.assertTrue(all(secret["rules"] == [codex_rule] for secret in secrets))


def host_envelope(kind: str, data: object) -> dict[str, object]:
    return {"protocol": "orvek.host", "version": 1, "type": kind, "data": data}


def journal_command(
    sequence: int,
    kind: str,
    data: dict[str, object],
    *,
    operation: str = "request-1",
) -> dict[str, object]:
    return host_envelope(
        "event",
        {
            "type": "journal",
            "data": {
                "sequence": sequence,
                "aggregate": "session-1",
                "kind": "session",
                "revision": sequence,
                "event": {
                    "type": "command",
                    "data": {
                        "operation": operation,
                        "command": {"type": kind, "data": data},
                        "at_ms": sequence,
                    },
                },
            },
        },
    )


def durable_host_events(
    *,
    items: list[dict[str, object]] | None = None,
    tool_results: list[tuple[str, object]] | None = None,
    usage: dict[str, object] | None = None,
) -> list[dict[str, object]]:
    events = [
        host_envelope(
            "session",
            {
                "id": "session-1",
                "journal_sequence": 0,
                "model": {"model": MODEL, "thinking": "low"},
            },
        ),
        host_envelope(
            "submission_pending",
            {"session": "session-1", "request": "request-1"},
        ),
        host_envelope(
            "submission",
            {"id": "request-1", "status": {"state": "queued"}},
        ),
        journal_command(
            2,
            "input",
            {"kind": "task", "content": [{"type": "input_text", "text": "inspect"}]},
        ),
    ]
    sequence = 4
    if items is not None:
        events.append(
            journal_command(
                sequence,
                "response",
                {"request": "request-1", "items": items},
            )
        )
        sequence += 2
    if usage is not None:
        events.append(
            journal_command(
                sequence,
                "provider_usage",
                {"request": "request-1", "usage": usage},
            )
        )
        sequence += 2
    for call_id, output in tool_results or []:
        events.append(
            journal_command(
                sequence,
                "tool_result",
                {
                    "request": "request-1",
                    "call_id": call_id,
                    "output": json.dumps(output, separators=(",", ":")),
                },
            )
        )
        sequence += 2
    events.extend(
        [
            journal_command(
                sequence,
                "turn_settled",
                {"request": "request-1", "outcome": "complete", "error": None},
            ),
            host_envelope(
                "submission_result",
                {
                    "id": "request-1",
                    "status": {
                        "state": "finished",
                        "task": "task-1",
                        "outcome": "complete",
                        "error": None,
                    },
                },
            ),
        ]
    )
    return events


class EvidenceContractTests(unittest.TestCase):
    def populate(
        self,
        events: list[dict[str, object]],
        policy: EvidencePolicy | None = None,
    ) -> tuple[AgentContext, dict[str, object]]:
        with tempfile.TemporaryDirectory() as directory:
            logs = Path(directory)
            (logs / "input.jsonl").write_text(
                '{"instruction":"inspect"}\n', encoding="utf-8"
            )
            (logs / "events.jsonl").write_text(
                "".join(json.dumps(event) + "\n" for event in events),
                encoding="utf-8",
            )
            # A legacy sidecar artifact must be neither required nor trusted.
            (logs / "orchestration.jsonl").write_text("not-json\n", encoding="utf-8")
            context = AgentContext()
            populate_context(
                logs_dir=logs,
                context=context,
                agent_name="orvek",
                agent_version="test",
                policy=policy
                or EvidencePolicy(
                    minimum_subagents=0,
                    fail_on_subagent_error=False,
                    require_wait=False,
                ),
            )
            trajectory = json.loads((logs / "trajectory.json").read_text())
            return context, trajectory

    def test_journal_evidence_writes_exact_available_atif(self) -> None:
        items = [
            {"type": "reasoning", "summary": [{"text": "checked"}]},
            {
                "type": "function_call",
                "call_id": "call-1",
                "name": "read_file",
                "arguments": '{"path":"README.md"}',
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "done"}],
            },
        ]
        context, trajectory = self.populate(
            durable_host_events(
                items=items,
                tool_results=[("call-1", {"content": "hello"})],
                usage={
                    "input_tokens": 10,
                    "output_tokens": 4,
                    "total_tokens": 14,
                    "cached_input_tokens": None,
                    "reasoning_tokens": 2,
                },
            )
        )

        self.assertEqual(context.n_input_tokens, 10)
        self.assertIsNone(context.n_cache_tokens)
        self.assertIsNone(context.cost_usd)
        self.assertEqual(trajectory["session_id"], "session-1")
        self.assertEqual(trajectory["steps"][1]["message"], "done")
        self.assertEqual(trajectory["steps"][1]["reasoning_content"], "checked")
        self.assertEqual(
            trajectory["steps"][1]["observation"]["results"][0]["content"],
            '{"content":"hello"}',
        )
        self.assertEqual(
            trajectory["final_metrics"]["extra"]["available_usage"]["reasoning_tokens"],
            2,
        )

    def test_malformed_protocol_is_rejected(self) -> None:
        events = durable_host_events()
        events[0]["protocol"] = "orvek.deleted"
        with self.assertRaisesRegex(RuntimeError, "orvek.host v1"):
            self.populate(events)

    def test_view_gap_and_nonmonotonic_journal_are_rejected(self) -> None:
        gap = durable_host_events()
        gap.insert(-1, host_envelope("view_gap", {"after": 2, "through": 5}))
        with self.assertRaisesRegex(RuntimeError, "view_gap"):
            self.populate(gap)

        preview_gap = durable_host_events()
        preview_gap.insert(
            -1,
            host_envelope("event", {"type": "preview_gap", "dropped": 1}),
        )
        with self.assertRaisesRegex(RuntimeError, "view_gap"):
            self.populate(preview_gap)

        nonmonotonic = durable_host_events(items=[])
        journal_events = [event for event in nonmonotonic if event["type"] == "event"]
        journal_events[1]["data"]["data"]["sequence"] = 1
        with self.assertRaisesRegex(RuntimeError, "strictly monotonic"):
            self.populate(nonmonotonic)

    def test_interrupted_submission_result_is_rejected(self) -> None:
        events = durable_host_events()
        events[-1]["data"]["status"] = {"state": "interrupted"}
        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            self.populate(events)

    def test_tool_calls_and_results_must_pair_exactly_once(self) -> None:
        call = {
            "type": "function_call",
            "call_id": "missing",
            "name": "read_file",
            "arguments": "{}",
        }
        with self.assertRaisesRegex(RuntimeError, "no durable tool_result"):
            self.populate(durable_host_events(items=[call]))

        with self.assertRaisesRegex(RuntimeError, "no matching function_call"):
            self.populate(durable_host_events(tool_results=[("orphan", {})]))

    def test_subagent_policy_uses_only_durable_paired_results(self) -> None:
        calls = [
            {
                "type": "function_call",
                "call_id": "spawn",
                "name": "spawn_agent",
                "arguments": "{}",
            },
            {
                "type": "function_call",
                "call_id": "wait",
                "name": "wait_agent",
                "arguments": '{"agent_ids":[7]}',
            },
        ]
        policy = EvidencePolicy(
            minimum_subagents=1,
            fail_on_subagent_error=True,
            require_wait=True,
        )
        events = durable_host_events(
            items=calls,
            tool_results=[
                (
                    "spawn",
                    {
                        "agent_id": 7,
                        "model": "selected",
                        "role": "reviewer",
                        "status": {"state": "running"},
                    },
                ),
                (
                    "wait",
                    {
                        "agents": [
                            {"agent_id": 7, "status": {"state": "completed"}}
                        ],
                        "timed_out": False,
                    },
                ),
            ],
        )
        context, _ = self.populate(events, policy)
        self.assertEqual(context.metadata["orchestration"]["latest_states"], {"7": "completed"})

        malformed = durable_host_events(
            items=calls[:1], tool_results=[("spawn", {"error": "unavailable"})]
        )
        with self.assertRaisesRegex(RuntimeError, "durably proven subagents"):
            self.populate(malformed, policy)


class ConfigurationContractTests(unittest.TestCase):
    def test_eval_python_project_is_self_contained(self) -> None:
        evals_directory = Path(__file__).resolve().parents[1]
        repository = evals_directory.parent

        self.assertTrue((evals_directory / "pyproject.toml").is_file())
        self.assertTrue((evals_directory / "uv.lock").is_file())
        self.assertFalse((repository / "pyproject.toml").exists())
        self.assertFalse((repository / "uv.lock").exists())

    def test_terminal_bench_is_pinned_and_uses_supported_agent_settings(self) -> None:
        evals_directory = Path(__file__).resolve().parents[1]
        config = yaml.safe_load(
            (evals_directory / "terminal-bench.yaml").read_text(encoding="utf-8")
        )

        self.assertEqual(config["datasets"][0]["ref"], "6")
        self.assertEqual(config["environment"]["type"], "docker")
        self.assertNotIn("auth_mode", config["agents"][0]["kwargs"])
        self.assertNotIn("env", config["agents"][0])
        self.assertNotIn("web_search", config["agents"][0]["kwargs"])
        self.assertNotIn("image_generation", config["agents"][0]["kwargs"])

    def test_harbor_recipe_passes_dataset_for_cli_filters(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        justfile = (repository / "justfile").read_text(encoding="utf-8")
        harbor_recipe = justfile.split("harbor-eval *args='':", 1)[1].split(
            "# Browse retained Harbor jobs",
            1,
        )[0]

        self.assertIn(
            "--dataset terminal-bench/terminal-bench-2-1@6",
            harbor_recipe,
        )


if __name__ == "__main__":
    unittest.main()
