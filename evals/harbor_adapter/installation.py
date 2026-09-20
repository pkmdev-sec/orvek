"""Build and locate installation artifacts for the Orvek Harbor adapter."""

from pathlib import Path


def cli_tools_install_command(*, install_node: bool) -> str:
    """Build a portable installer for Orvek's task-side CLI dependencies."""
    packages = ["ca-certificates", "curl", "bash", "ripgrep"]
    checks = ["curl", "bash", "rg"]
    if install_node:
        packages.extend(("nodejs", "npm"))
        checks.extend(("node", "npm"))

    package_list = " ".join(packages)
    command_checks = "; ".join(
        f"command -v {command} >/dev/null 2>&1" for command in checks
    )
    return (
        "if ldd --version 2>&1 | grep -qi musl || "
        "[ -f /etc/alpine-release ]; then "
        f"apk add --no-cache {package_list}; "
        "elif command -v apt-get >/dev/null 2>&1; then "
        "apt-get update && DEBIAN_FRONTEND=noninteractive "
        "apt-get install --yes --no-install-recommends "
        f"{package_list}; "
        "elif command -v yum >/dev/null 2>&1; then "
        f"yum install -y {package_list}; "
        "else "
        "echo 'No supported package manager found; checking preinstalled tools' >&2; "
        "fi; "
        f"{command_checks}"
    )


def executor_helper_path(binary_path: Path, image_architecture: str) -> Path:
    """Return the local executor helper matching Docker's image architecture."""
    suffixes = {
        "amd64": "x86_64",
        "x86_64": "x86_64",
        "arm64": "aarch64",
        "aarch64": "aarch64",
    }
    try:
        suffix = suffixes[image_architecture]
    except KeyError as error:
        raise RuntimeError(
            f"unsupported Harbor task architecture: {image_architecture}"
        ) from error
    helper = binary_path.with_name(f"orvek-executor-linux-{suffix}")
    if not helper.is_file():
        raise RuntimeError(
            f"missing architecture-matched executor helper at {helper}; "
            "run `just build-harbor-agent`"
        )
    return helper.resolve()
