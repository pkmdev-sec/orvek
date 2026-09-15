# Workspace execution

## Work on a repository

Select the source explicitly:

```sh
orvek --workspace /absolute/path/to/repository
```

A bare launch uses `<config-directory>/workspaces/default`. It does not copy the
current directory into that workspace. The task works on a private snapshot.
The default delivery is a patch, not an overwrite of the source checkout.

## Available tools

Task reads, writes, searches, and commands are available before contract admission
and while a follow-up awaits admission. Instructions recommend a contract first;
workspace access does not depend on that recommendation. Unresolved contract
questions no longer stop further research automatically.

Workspace commands have network access. The workspace supports executing generated
binaries and scripts. Install user-level dependencies under `/workspace` to retain
them between commands. `/cache` and `/tmp` are temporary, not persistent installs.

Commands still run without root privileges. System directories are read-only.
For system packages or toolchains not present in the executor image, select a
prepared image with `ORVEK_EXECUTOR_IMAGE`. The default `debian:bookworm-slim`
image does not include Git, Cargo, or Node.js.

Protected verification runs without network access. It can use dependencies
included in the workspace snapshot or executor image. Permission to edit does not
mean a task has passed verification: completion still requires the contract,
checks, and durable evidence. Workspace containment, cancellation, resource
bounds, and journal ownership remain active.

## Run a development build

Docker must be running, and the selected executor image must already exist locally.
Build the matching Linux helper, then pass its absolute path:

```sh
crates/executor/build-linux.sh aarch64
cargo build -p orvek --bin orvek
ORVEK_EXECUTOR_HELPER="$PWD/target/executor/orvek-executor-linux-aarch64"   target/debug/orvek --workspace "$PWD"
```

Use `x86_64` instead of `aarch64` for an AMD64 Docker daemon. An installed binary
can find a matching regular helper file beside itself. A plain Cargo build does
not install that sidecar.

## Provider failures

Summary-only reasoning items are valid continuation input; they no longer require
an encrypted payload. The context projection retains that history.

A provider rejection is not a successful run. Failure output includes the safe
failure category and HTTP status. Rate-limit retries are bounded and recorded as
separate model attempts; uncertain dispatched attempts are not retried.

## Deferred work

System-wide package installation and persistent container environments need a
separate lifecycle design. This change does not grant root access or persist a
container's system filesystem. Self-evolution rollout and promotion/rollback
corrections are also separate work; workspace freedom does not complete those
phases.
