# Orvek executor sidecar

`orvek-executor` is a static Linux PID-1 supervisor and bounded binary transport. Its only dependencies are serde, serde_json and libc. It has no provider, controller, evaluator, tool, credential or task-state access. A completed protocol frame reports process and export facts; it does not certify a task.

Build the sidecar for the **Docker image architecture**:

```sh
crates/executor/build-linux.sh aarch64
# or: crates/executor/build-linux.sh x86_64
```

The script installs the Rust musl target, uses Rust's bundled `rust-lld`, and writes `target/executor/orvek-executor-linux-{aarch64,x86_64}`. Rustup, Cargo and the target's musl standard library are build prerequisites; Docker does not need a compiler. The arm64 cross-build and execution were verified on macOS arm64 with a Linux arm64 Debian image. The x86_64 path is provided but has not been executed here.

For development, set `ORVEK_EXECUTOR_HELPER` to that absolute file path. For an installed Orvek binary, install the matching sidecar beside it with the exact architecture suffix. `DockerExecutor::connect_with_helper` accepts an explicit helper path and resource limits. The runtime does not compile or download a helper automatically. It checks static ELF format and architecture, records the digest, and checks that digest again before each run. It mounts a private immutable copy.

The runtime requires a local Unix Docker socket, cgroup v2 memory/swap/PID limits, Docker's builtin seccomp profile, and a non-rootless, non-userns-remapped daemon. `DOCKER_HOST` overrides and remote sockets are rejected. An existing context is resolved once; subsequent commands use the pinned socket, an empty private Docker config and a cleared environment. Images must be Linux, contain `/bin/sh`, and declare no `VOLUME`; healthchecks and network are disabled. Source staging must be on a path shared with the Docker VM. These requirements are enforced and are not an automatic fallback to weaker isolation.

Only readonly input and helper mounts enter the container. Writable source, cache, `/tmp` and `/dev/shm` each use tmpfs with explicit byte and inode limits. Memory plus swap is capped at the memory limit, so the guest cannot spill tmpfs to unbounded swap. The root filesystem is readonly. The helper verifies filesystem type and actual quotas before code starts. It keeps only the capabilities needed for source setup, UID transition and descendant termination. Code runs with empty supplementary groups, a nonzero UID, no capabilities and `no_new_privs`. The UID matches the staged source owner (root-created staging is assigned UID 65534), preserving owner-private source modes. Source ownership is not exported as mutable metadata. The host-managed workspace root keeps its original mode; attempts to change that root mode are rejected.

The supervisor enforces command time and combined output bounds. The host bounds handshake wait to ten seconds and partial-frame/export idle waits to five seconds; a silent command is governed by its requested total deadline. It kills and reaps every remaining descendant before export, including detached process sessions. Export uses length-bounded frames, not tar. Host validation rejects traversal, duplicate paths, unsafe links, special files, hardlinks, special mode bits and incomplete transfers. Failed, cancelled, exhausted or unknown jobs do not publish partial guest changes. Normal process exits, including known nonzero exits, can publish complete source changes. Cache contents are ephemeral and never exported.

Host publication requires an exclusive runtime lease, full baseline content/mode identity and a validated export. It uses an atomic directory exchange, validates the displaced baseline and rolls back detected races. A baseline conflict retains the validated guest in the workspace parent's `.orvek-guest-results/<job-id>` with a receipt and full snapshot identities; `retained_guest` verifies the receipt and tree before returning it. Retention count is bounded. The host controller must exclude other host writers during publication; POSIX cannot atomically compare an entire tree against arbitrary noncooperating writers. Recovery errors return `Unknown` and preserve recovery trees. No new execution success is derived from reconciliation: it only proves the deterministic job container is absent.

Every source entry is considered, with no snapshot exclusions. Supported source metadata is relative path, regular file bytes, ordinary Unix permissions, safe symlink target and empty directory. Unsupported types, special modes and source owner-unreadable files/directories are explicit failures. Runtime limits bound bytes, inodes, entries, metadata, command, output and retained exports. The requested deadline includes staging and execution; bounded teardown may continue afterward. Host filesystem calls cannot be preempted while the OS is blocked. Late or uncertain outcomes are never returned as successful process results.

Run infrastructure tests serially from a Docker-shared checkout:

```sh
ORVEK_EXECUTOR_HELPER="$PWD/target/executor/orvek-executor-linux-aarch64" \
  cargo test -p orvek-harness --test docker_execution -- --ignored --test-threads=1
cargo test -p orvek-harness --lib runtime::transport
cargo clippy -p orvek-harness --lib --test docker_execution -- -D warnings
```

The Docker tests create fixtures only under the checkout's local runtime-test directory; they do not read a real repository workspace, session database or credentials. Docker tests are ignored by default and require the pre-pulled `debian:bookworm-slim` image. Run the memory-pressure case separately from other teams' Docker verification.
