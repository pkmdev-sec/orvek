"""Trusted per-trial Orvek host sidecar for Harbor's untrusted task."""

from __future__ import annotations

import asyncio
import json
import shutil
import tempfile
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence

from harbor.environments.base import BaseEnvironment

from harbor_adapter.installation import executor_helper_path
from harbor_adapter.iron_proxy import (
    CLEANUP_TIMEOUT_SECONDS,
    LocalCodexAuthProxy,
    _docker_endpoint,
    _run_docker,
)


HOST_COMMAND_TIMEOUT_SECONDS = 24 * 60 * 60
HOST_BINARY = "/installed-agent/orvek"
HOST_HELPER = "/installed-agent/orvek-executor"
HOST_RUNTIME_IMAGE = (
    "debian:bookworm-slim@"
    "sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171"
)


@dataclass(frozen=True)
class HostRunResult:
    stdout: str
    stderr: str
    returncode: int
    terminal_fence: bool


class HostSidecar:
    """Own the trusted host, executor image, workspace, and publication fence."""

    def __init__(
        self,
        *,
        logs_dir: Path,
        binary_path: Path,
        auth_proxy: LocalCodexAuthProxy,
    ) -> None:
        self.logs_dir = logs_dir.resolve()
        self.binary_path = binary_path.resolve()
        self.auth_proxy = auth_proxy

    async def run(
        self,
        *,
        environment: BaseEnvironment,
        client_arguments: Sequence[str],
        client_environment: Mapping[str, str],
    ) -> HostRunResult:
        main_container = await self.auth_proxy.validate_task(environment)
        root = Path(
            tempfile.mkdtemp(
                prefix=".orvek-harbor-trial-", dir=self.logs_dir.parent.resolve()
            )
        ).resolve()
        root.chmod(0o700)
        if not root.is_relative_to(self.logs_dir.parent.resolve()):
            raise RuntimeError("private Harbor trial root escaped its safe parent")

        image = f"orvek-harbor-executor:{uuid.uuid4().hex}"
        host_container = f"orvek-host-{uuid.uuid4().hex}"
        cleanup_allowed = True
        active_error: BaseException | None = None
        try:
            await _run_docker(
                ["commit", "--pause=true", main_container, image],
                timeout_seconds=300,
            )
            architecture = await self._image_architecture(image)
            helper_path = executor_helper_path(self.binary_path, architecture)
            workspace = root / "workspace"
            workspace.mkdir(mode=0o700)
            await _run_docker(
                ["cp", f"{main_container}:/app/.", str(workspace)],
                timeout_seconds=300,
            )

            state = root / "state"
            state.mkdir(mode=0o700)
            config = state / "config.toml"
            config.write_text("", encoding="utf-8")
            config.chmod(0o600)
            auth = root / "auth"
            socket = _local_docker_socket()
            host_environment = {
                **client_environment,
                "DOCKER_HOST": f"unix://{socket}",
                "ORVEK_EXECUTOR_IMAGE": image,
                "ORVEK_EXECUTOR_HELPER": HOST_HELPER,
                "ORVEK_CONFIG": str(config),
                "ORVEK_HOST_STATE": str(state / "host" / "v1"),
                "ORVEK_WORKSPACE": str(workspace),
                **self.auth_proxy.host_environment(
                    auth / "auth.json", auth / "ca.crt"
                ),
            }
            await self._start_host_container(
                container_name=host_container,
                architecture=architecture,
                root=root,
                socket=socket,
                helper_path=helper_path,
                environment=host_environment,
            )
            async with self.auth_proxy.running(
                host_container=host_container, public_directory=auth
            ):
                await _run_docker(
                    ["exec", "--detach", host_container, HOST_BINARY, "host"],
                    timeout_seconds=30,
                )
                await self._wait_for_host(host_container, state / "host" / "v1" / "host.sock")
                cleanup_allowed = False
                result = await _run_docker(
                    ["exec", host_container, HOST_BINARY, *client_arguments],
                    check=False,
                    timeout_seconds=HOST_COMMAND_TIMEOUT_SECONDS,
                    interrupt_on_cancel=True,
                )
                stdout = result.stdout
                terminal_fence = _has_terminal_fence(stdout)
                if terminal_fence:
                    await self._publish_workspace(main_container, workspace)
                    cleanup_allowed = True
                stderr = result.stderr
                if not terminal_fence:
                    stderr += (
                        "\nOrvek retained incomplete Harbor resources for recovery: "
                        f"host={host_container} image={image} root={root}\n"
                    )
                return HostRunResult(
                    stdout=stdout,
                    stderr=stderr,
                    returncode=result.returncode,
                    terminal_fence=terminal_fence,
                )
        except BaseException as error:
            active_error = error
            raise
        finally:
            cleanup_error = await self._finish_cleanup(
                host_container=host_container,
                image=image,
                root=root,
                cleanup_allowed=cleanup_allowed,
            )
            if cleanup_error is not None:
                if active_error is not None:
                    raise cleanup_error from active_error
                raise cleanup_error

    async def _start_host_container(
        self,
        *,
        container_name: str,
        architecture: str,
        root: Path,
        socket: Path,
        helper_path: Path,
        environment: Mapping[str, str],
    ) -> None:
        arguments = [
            "run", "--detach", "--name", container_name,
            "--platform", f"linux/{architecture}",
            "--entrypoint", "/bin/sh",
            "--user", "0:0",
            "--cap-drop", "ALL",
            "--security-opt", "no-new-privileges",
            "--mount", f"type=bind,src={root},dst={root}",
            "--mount", f"type=bind,src={socket},dst={socket}",
            "--mount", f"type=bind,src={self.binary_path},dst={HOST_BINARY},readonly",
            "--mount", f"type=bind,src={helper_path},dst={HOST_HELPER},readonly",
        ]
        for name, value in environment.items():
            arguments.extend(("--env", f"{name}={value}"))
        arguments.extend((HOST_RUNTIME_IMAGE, "-c", "while :; do sleep 3600; done"))
        await _run_docker(arguments, timeout_seconds=120)

    @staticmethod
    async def _wait_for_host(host_container: str, socket: Path) -> None:
        for _ in range(300):
            result = await _run_docker(
                ["exec", host_container, "test", "-S", str(socket)],
                check=False,
                timeout_seconds=5,
            )
            if result.returncode == 0:
                return
            await asyncio.sleep(0.1)
        raise RuntimeError("trusted Orvek host did not create its control socket")

    @staticmethod
    async def _image_architecture(image: str) -> str:
        result = await _run_docker(
            ["image", "inspect", "--format", "{{.Architecture}}", image],
            timeout_seconds=30,
        )
        architecture = result.stdout.strip()
        if not architecture:
            raise RuntimeError("committed Harbor executor image has no architecture")
        return architecture

    @staticmethod
    async def _publish_workspace(main_container: str, workspace: Path) -> None:
        await _run_docker(
            [
                "exec", main_container, "find", "/app", "-mindepth", "1",
                "-maxdepth", "1", "-exec", "rm", "-rf", "--", "{}", "+",
            ],
            timeout_seconds=120,
        )
        await _run_docker(
            ["cp", f"{workspace}/.", f"{main_container}:/app"],
            timeout_seconds=300,
        )

    async def _finish_cleanup(
        self,
        *,
        host_container: str,
        image: str,
        root: Path,
        cleanup_allowed: bool,
    ) -> Exception | None:
        if not cleanup_allowed:
            return None
        task = asyncio.create_task(
            self._cleanup(
                host_container=host_container,
                image=image,
                root=root,
            )
        )
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError:
            while not task.done():
                try:
                    await asyncio.shield(task)
                except asyncio.CancelledError:
                    continue
            raise
        except Exception as error:
            return error
        return None

    @staticmethod
    async def _cleanup(*, host_container: str, image: str, root: Path) -> None:
        host_result = await _run_docker(
            ["rm", "--force", host_container],
            check=False,
            timeout_seconds=CLEANUP_TIMEOUT_SECONDS,
        )
        image_result = await _run_docker(
            ["image", "rm", "--force", image],
            check=False,
            timeout_seconds=CLEANUP_TIMEOUT_SECONDS,
        )
        errors = []
        if host_result.returncode != 0 and "No such container" not in host_result.stderr:
            errors.append(RuntimeError("failed to remove trusted Harbor host container"))
        if image_result.returncode != 0 and "No such image" not in image_result.stderr:
            errors.append(RuntimeError("failed to remove Harbor executor image"))
        if not errors:
            shutil.rmtree(root)
        if len(errors) == 1:
            raise errors[0]
        if errors:
            raise BaseExceptionGroup("trusted Harbor host cleanup failed", errors)


def _local_docker_socket() -> Path:
    endpoint = _docker_endpoint()
    raw_path = endpoint.removeprefix("unix://")
    path = Path(raw_path)
    if not path.is_absolute():
        raise RuntimeError("trusted Harbor host requires an absolute local Docker socket")
    return path


def _has_terminal_fence(stdout: str) -> bool:
    events = []
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            return False
        if not isinstance(event, dict):
            return False
        if event.get("protocol") != "orvek.host" or event.get("version") != 1:
            return False
        events.append(event)
    terminals = [event for event in events if event.get("type") == "submission_result"]
    if len(terminals) != 1 or not events or terminals[0] is not events[-1]:
        return False
    data = terminals[0].get("data")
    status = data.get("status") if isinstance(data, dict) else None
    return isinstance(status, dict) and status.get("state") in {"finished", "cancelled"}
