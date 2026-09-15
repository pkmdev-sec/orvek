# Hard-quota runtime handoff

Completed in `/Users/pkmdev/orvek-harness-inference` on 2026-09-13. Parent integration, controller recovery tests and frontend packaging are separate work. No original or parent worktree files were edited.

## Integration files

- `crates/harness/src/runtime.rs`
- `crates/harness/src/runtime/transport.rs`
- `crates/harness/src/runtime/workspace.rs`
- `crates/harness/src/runtime/IMPLEMENTATION.md`
- `crates/harness/tests/docker_execution.rs`
- `crates/executor/Cargo.toml`
- `crates/executor/src/{lib.rs,main.rs,supervisor.rs}`
- `crates/executor/build-linux.sh` (executable)
- `crates/executor/README.md`

Harness manifest addition: `orvek-executor = { path = "../executor", default-features = false }`. The existing rustix dependency needs both `fs` and `process` features. The executor protocol uses workspace serde/serde_json; the Linux binary's `stub` feature adds libc `0.2.189`. The root workspace's existing `crates/*` membership includes the new crate. Regenerate the parent lockfile; this worktree's complete lockfile includes earlier tasks and is not an integration patch.

## Workspace tools and protected verification

Task workspace reads, writes and commands are available before contract admission and
while a follow-up awaits admission. Contract-first work is guidance, not a file or
command permission gate. Writes still invalidate candidate evidence and acquire a
journaled writer job. Completion still requires the admitted contract and host checks.

`run()` and `environment()` use `ExecutionPolicy::Protected`: no network access.
Writable task tools and manual shell commands use `run_with_policy(...,
ExecutionPolicy::Workspace, ...)` and record `environment_for` with the same policy.
Workspace commands use Docker bridge networking. The environment identity records
the network mode and executable tmpfs options. Sessions bound to an older environment
may require a new session after restarting the Host; do not bypass admission mismatches.
Read-only auxiliary and child tools
keep the protected policy. Neither policy mounts the host socket or credentials.

Both policies run commands as a non-root UID with a read-only container root and
quota-limited writable `/workspace`, `/cache`, `/tmp` and `/dev/shm`. These mounts
allow execution so built binaries and user-installed tools can run; `nosuid` and
`nodev` remain set. User-level package
installs and downloaded tools must target these writable paths. Only validated
`/workspace` exports persist between commands. Cache, temporary files and running
processes do not persist. System package installation (for example `apt install`)
is not supported. Required verification dependencies must be in the pinned image or
workspace; verification cannot download them. Bridge networking is outbound-capable,
not an egress allowlist, and can reach services routable from the Docker network.

## Public contract

`ExecutionRequest`, `ExecutionResult`, `ExecutionStatus` and `RuntimeError` retain their prior fields/variants. `DockerExecutor::connect(image)`, `environment()`, `image_id()`, `run(request, cancellation)` and `reconcile(job_id)` remain available.

Added:

```rust
DockerExecutor::connect_with_helper(
    image: &str,
    helper: &Path,
    limits: ExecutionLimits,
) -> Result<DockerExecutor, RuntimeError> // async
DockerExecutor::retained_guest(
    &self, request: &ExecutionRequest,
) -> Result<Option<RetainedGuest>, RuntimeError>
DockerExecutor::reconcile_job(
    &self, task_id: Uuid, generation: u64, job_id: Uuid,
) -> Result<ExecutionFence, RuntimeError> // async
DockerExecutor::reconcile_jobs(
    &self, task_id: Uuid, generation: u64, jobs: &[Uuid],
) -> Result<Vec<ExecutionFence>, RuntimeError> // async; at most 128 jobs
```

`ExecutionLimits` controls memory/PIDs, workspace/cache/temporary byte and inode budgets, and retained-guest count. `/tmp` and `/dev/shm` each receive the temporary budget. Defaults are 512 MiB memory, 64 PIDs, 256 MiB/32768 workspace inodes, 64 MiB/8192 cache inodes, 32 MiB/4096 temporary inodes, eight retained guests. `ExecutionEnvironment` adds helper digest, architecture, exact quotas and source transport; its protocol version is 2. `RetainedGuest` contains job ID, tree path, baseline/guest snapshots and both identities. `ExecutionFence` contains caller-supplied task/generation/job identity, deterministic container name and `observed_absent=true`. Fencing grants no reusable execution success. Parent must include verification child IDs in the supplied job list.

Only immutable RO source/helper binds enter the container. Mutable storage has native tmpfs byte and inode quotas verified by the helper. Code runs with no capabilities at a nonzero source-owner UID. Root PID 1 kills/reaps all descendants before any export. Every source entry is validated with no exclusions; export never uses tar extraction. Normal known exits can publish complete changes only after container absence and baseline checks. Failed/partial/unknown outcomes never produce successful results. A host conflict preserves the original host tree and a bounded validated guest artifact.

## Build and verification evidence

```sh
crates/executor/build-linux.sh aarch64
ORVEK_EXECUTOR_HELPER="$PWD/target/executor/orvek-executor-linux-aarch64" \
  cargo test -p orvek-harness --test docker_execution -- --ignored --test-threads=1 \
  --skip memory_exhaustion_cannot_return_success_or_publish_partial_source
cargo test -p orvek-harness --lib runtime::transport
cargo clippy -p orvek-harness --lib --test docker_execution -- -D warnings
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$(rustc --print sysroot)/lib/rustlib/aarch64-apple-darwin/bin/rust-lld" \
  cargo clippy -p orvek-executor --features stub --target aarch64-unknown-linux-musl -- -D warnings
```

- Final packaged-helper serial Docker suite: **12 passed**, 0 failed, one memory probe deliberately filtered out; `/tmp/tact-runtime-final-docker.log` (17.43 seconds).
- Earlier full serial quota suite: **11 passed**, including the real memory-exhaustion probe; `/tmp/tact-runtime-quota-tests.log`. OOM was tested before the final root-mode/IPC/receipt/reader-cleanup refinements. It was not repeated alongside parent Docker work.
- Final source formatting/diagnostic-only edit: Docker isolation/write-roundtrip smoke **1 passed**; `/tmp/tact-runtime-final-smoke.log`.
- Malformed/partial transport tests: **3 passed**; `/tmp/tact-runtime-protocol-tests.log`.
- Host and Linux-helper Clippy with `-D warnings`: **passed**; `/tmp/tact-runtime-clippy.log`, `/tmp/orvek-executor-clippy.log`.
- Static arm64 build: **passed**; `/tmp/orvek-executor-build.log`. Final sidecar SHA-256: `3e93fa2366729f1210a68dea269374ac9befeeba9066d0bc19dc1d20672e0f4c`.
- No managed containers remained after the final smoke.

Docker was local Colima, cgroup v2, builtin seccomp, memory/swap/PID accounting enabled. The Linux arm64 `debian:bookworm-slim` image ID was `sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171`. A native quota probe confirmed 4 MiB and 64 inodes on each writable mount. All test sources were generated under this checkout's `.orvek/docker-test-workspaces`; no user workspace, session database or provider credentials were read.

The native cases cover host-secret/socket isolation; normal writes/deletes/executable and owner-private modes/safe symlinks/empty directories; readonly verification; output/deadline/cancellation; detached descendants; byte, inode and cache exhaustion; OOM; host conflict plus validated retained guest; unsafe symlinks, FIFOs, hardlinks and root-mode mutation; forged protocol stdout; malicious tar preserved as plain bytes; and capability/user-mount-namespace denial.

## Explicit prerequisites and limits

See `crates/executor/README.md` for build and installation instructions. Require a Unix host and a local non-rootless, non-userns-remapped Docker backend with cgroup v2, builtin seccomp and hard resource controls. Reject remote/DOCKER_HOST routes, dynamic or mismatched helpers, non-Linux images and images declaring writable VOLUMEs. Image healthchecks are disabled. The image must contain `/bin/sh`. Verification dependencies must be in the image or workspace because protected verification disables networking. Workspace commands use bridge networking and can install user-level tools into the workspace; caches remain ephemeral. Only arm64 sidecar execution was verified here; x86_64 build selection is provided but unverified.

The controller must serialize host publication and exclude other host writers; atomic whole-tree compare-and-swap against arbitrary noncooperating writers is not a POSIX primitive. Pre/post exchange validation detects races and preserves recovery trees on uncertainty. Source ownership and the host-managed workspace root mode are not mutable exports. Special files, hardlinks, special mode bits and owner-unreadable source entries are rejected. Host filesystem calls cannot be preempted while the OS is blocked, and bounded teardown can extend past the requested deadline; late success is rejected. Fencing failures retain `Unknown` diagnostics for parent recovery.
