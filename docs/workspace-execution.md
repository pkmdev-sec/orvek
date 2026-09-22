# Workspace execution

## Use the default host mode

Orvek uses host execution by default. A bare launch selects the current directory as the workspace:

```sh
orvek
```

Use `--workspace` to select another repository:

```sh
orvek --workspace /absolute/path/to/repository
```

You can also set `agent.workspace` in the configuration file. The selected path is the live
workspace. Host mode does not copy it into a private snapshot, and changes apply directly to files
on the machine.

The primary model can use `read_file`, `search`, `write_file`, and `exec_command`. Host file tools
accept paths relative to the workspace, absolute paths, and `~/` paths. They follow ordinary
symlinks. `exec_command` runs `/bin/sh` with the user's permissions and inherits the host's
environment, network access, `HOME`, and `PATH`. A command can change anything that the user can
access, including files outside the selected workspace.

The host records tool jobs and receipts with the native backend, working directory, result, and
command metadata. These records do not grant extra authority. The model supplies tool arguments,
while the host controls admission, limits, cancellation, and job identity.

Host work finishes without protected verification, a verification certificate, or reproducible
patch delivery. The result is the live workspace state, not a patch from an isolated copy.

## Use the optional sandbox mode

Set sandbox execution in the configuration file when you need an isolated working copy and
contract-gated verification:

```toml
[agent]
execution = "sandbox"
```

Sandbox mode copies the selected source into a private task working copy. File tools stay inside
that copy, require relative paths, and reject symlinks and parent traversal. Writes change the
private copy. Successful delivery produces a patch instead of overwriting the source checkout.

The host still decides whether a tool may write and supplies its deadline, output limit, and job
identity. `exec_command` runs through `WorkspaceTools` in the isolated Docker executor. The
executor cannot access the host's SQLite database or artifact store.

Commands in the editable workspace phase have network access and can run generated binaries and
scripts. Install user-level dependencies under `/workspace` to retain them between commands.
`/cache` and `/tmp` are temporary. Commands run without root privileges, and system directories
are read-only.

Protected verification is a separate sandbox phase. It runs without network access against the
frozen candidate and dependencies already present in the workspace or executor image. Permission
to edit does not prove completion. Sandbox completion requires the admitted contract, successful
checks, durable evidence, and a verification certificate before patch delivery.

## Run an operator shell command

Operator `!` submissions are available only when `agent.execution = "sandbox"` and the Docker
executor is ready. Type `!COMMAND` in the TUI composer and press Enter. The detached host records
the submission, then runs the command as a non-root process in the isolated Docker workspace. It
does not ask the model, create a coding task, or grant access to the host user's shell permissions.
Host mode rejects the submission instead of falling back to native execution.

The TUI submission sets a 120-second timeout and a 256 KiB output bound. Workspace changes are
adopted into the session's durable sandbox checkpoint. The host also records shell start and
publication events plus the execution report, so resume can recover the command and its outcome.
Replaying the same accepted request returns its recorded result instead of running the command
again.

After publication, the TUI reads the report and displays its status or error plus at most 16 KiB
each of stdout and stderr. This display limit can be lower than the executor's output bound. Shell
input and output are not added to provider history automatically. To give the model a result, copy
the relevant output into a later prompt. These submissions are separate from model-issued
`exec_command` calls and do not send completion notifications.

## Prepare the sandbox executor

Docker must be running, and the selected executor image must already exist locally. The default
`debian:bookworm-slim` image does not include Git, Cargo, or Node.js. Set
`ORVEK_EXECUTOR_IMAGE` to use a prepared image.

For a source build, build the matching Linux helper and pass its absolute path:

```sh
crates/executor/build-linux.sh aarch64
cargo build -p orvek --bin orvek
ORVEK_EXECUTOR_HELPER="$PWD/target/executor/orvek-executor-linux-aarch64" \
  target/debug/orvek --workspace "$PWD"
```

Use `x86_64` instead of `aarch64` for an AMD64 Docker daemon. An installed release-archive binary
can find a matching regular helper file beside itself. A plain Cargo build does not install that
sidecar.

## Provider failures

Summary-only reasoning items are valid continuation input; they do not require an encrypted
payload. The context projection retains that history.

A provider rejection is not a successful run. Failure output includes the safe failure category
and HTTP status. Rate-limit retries are bounded and recorded as separate model attempts. Orvek does
not retry an attempt when it cannot tell whether the provider dispatched it.

## Deferred sandbox work

System-wide package installation and persistent container environments need a separate lifecycle
design. Sandbox mode does not grant root access or persist a container's system filesystem.
