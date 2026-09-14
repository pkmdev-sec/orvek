# Harbor durable-host integration

Status: accepted for implementation. The user explicitly requested that Harbor use the same
durable host as the terminal interface.

## Shape

Harbor's Python adapter owns one trusted host sidecar per trial. The sidecar runs `orvek host`,
has the local Docker socket, and uses the same `DockerExecutor` as interactive Orvek. The Harbor
`main` container remains an untrusted task environment without Docker authority.

Before starting the host, the adapter validates every task container with the existing iron-proxy
isolation rules, installs the requested command-line tools, snapshots the initialized `main`
container as an immutable executor image, and copies `/app` to a private trial directory. That
directory is mounted into the host sidecar at the identical absolute path because the executor's
staging mounts are resolved by the outer Docker daemon.

The credential proxy joins the host sidecar's network namespace. Only fake auth and the public CA
are mounted into the host. Real credentials remain in the proxy-only private mount. Provider calls
therefore originate from the trusted host; model-directed commands continue to run in disposable,
network-none executor containers.

The adapter submits through headless mode, retains its versioned stream, and treats the durable
submission receipt as completion authority. A client disconnect is recovered by receipt rather
than interpreted as task completion. Only a terminal, fenced receipt permits copying the published
workspace back to `main:/app` for Harbor's verifier.

## Rejected shapes

- Running the host in `main` would require exposing Docker authority to untrusted task code.
- Adding a Harbor-only direct-process executor would weaken the TUI's containment model and create
  two execution semantics.
- Treating headless stdout as the deleted orchestration protocol would fabricate evidence because
  the current stream contains durable host envelopes, journal frames, and submission receipts.

## Evidence contract

Harbor consumes one versioned, bounded projection derived from durable session and task journals.
It validates session and submission identity, monotonic journal cursors, gaps, exactly one terminal
receipt, tool call/result pairing, provider usage, child lifecycle, and wait evidence. Unsupported
policy facts fail closed; they are never inferred from a process exit or missing legacy file.

## Failure and cleanup

Cancellation first requests durable submission cancellation and waits for terminal or fenced state.
The proxy, host, executor image, and trial directory are removed only after that boundary. If safe
cleanup or workspace publication cannot be proved, Harbor fails the trial and retains bounded logs
needed to diagnose it.
