build *args='':
    cargo build {{args}}

# Build an optimized binary; pass --official to mark it as an official release build.
release *args='':
    #!/usr/bin/env bash
    set -euo pipefail
    release_build=0
    cargo_args=()
    set -- {{args}}
    for arg in "$@"; do
        if [[ "$arg" == "--official" ]]; then
            release_build=1
        else
            cargo_args+=("$arg")
        fi
    done
    if [[ ${#cargo_args[@]} -eq 0 ]]; then
        ORVEK_RELEASE_BUILD="$release_build" cargo build --release --package orvek --bin orvek
    else
        ORVEK_RELEASE_BUILD="$release_build" cargo build --release --package orvek --bin orvek "${cargo_args[@]}"
    fi

check-fmt:
    just fmt --check

fmt *args='':
    cargo +nightly fmt --all -- {{args}}

clippy *args='':
    cargo +stable clippy --all-targets {{args}} -- -D warnings

lint: check-fmt clippy audit-components

audit-components:
    python3 scripts/audit-component-wiring.py

test *args='':
    cargo nextest run {{args}}

# Subprocess fixtures are invoked by their owning isolation tests, not as standalone tests.
test-docker *args='':
    cargo nextest run --workspace --all-features --run-ignored only --test-threads=1 --no-fail-fast -E 'not (test(=process_owner) or test(=poisoned_configuration_child) or test(=environment_fixture_child))' {{args}}

test-docs:
    rustdoc --test README.md --edition 2024

check-docs:
    cargo doc --no-deps

check-features *args='':
    cargo hack check --package orvek-memory --feature-powerset --no-dev-deps {{args}}

bench *args='':
    cargo bench {{args}}

# Install the pinned, development-only Harbor environment.
harbor-bootstrap:
    uv sync --project evals --frozen

_local-docker-context:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "${DOCKER_HOST:-}" ]]; then
        echo 'unset DOCKER_HOST and select a local Docker context explicitly' >&2
        exit 1
    fi
    docker_context=$(docker context show)
    docker_endpoint=$(docker context inspect "$docker_context" --format '{{ "{{.Endpoints.docker.Host}}" }}')
    case "$docker_endpoint" in
        unix://*|/*) printf '%s\n' "$docker_context" ;;
        *)
            echo "Harbor requires a local Docker socket: $docker_endpoint" >&2
            exit 1
            ;;
    esac

_terminal-bench-platform:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
        exit 0
    fi
    settings="$HOME/Library/Group Containers/group.com.docker/settings-store.json"
    rosetta=$(/usr/bin/plutil \
        -extract UseVirtualizationFrameworkRosetta raw -o - "$settings" 2>/dev/null || true)
    if [[ "$rosetta" == "true" ]]; then
        exit 0
    fi
    echo 'Terminal-Bench requires Docker Desktop Rosetta support on Apple Silicon.' >&2
    echo 'Enable Rosetta for amd64 emulation in Docker Desktop, then restart Docker.' >&2
    exit 1

# Export static Linux Orvek and architecture-matched executor binaries.
build-harbor-agent platform='':
    #!/usr/bin/env bash
    set -euo pipefail
    docker_context=$(just --quiet _local-docker-context)
    for source_tree in \
        bin/orvek/src \
        crates/executor/src \
        crates/harness/src \
        crates/harness/benches \
        crates/memory/src; do
        test -z "$(find "$source_tree" -type l -print -quit)" || {
            echo "refusing to build Harbor agent with symlinks below $source_tree/" >&2
            exit 1
        }
        test -z "$(find "$source_tree" -type f ! -name '*.rs' \
            ! -path 'crates/harness/src/context_render/fonts/8x13-ascii.bin' \
            ! -path 'crates/harness/src/context_render/fonts/LICENSE' \
            ! -path 'crates/harness/src/context_render/fonts/README.md' \
            ! -path 'crates/harness/src/context_render/fonts/generate.py' \
            ! -path 'crates/harness/src/interpreter/bootstrap.js' \
            ! -path 'crates/harness/src/runtime/IMPLEMENTATION.md' \
            ! -path 'crates/harness/src/review/README.md' -print -quit)" || {
            echo "refusing to send unrecognized source assets below $source_tree/ to the Harbor build" >&2
            exit 1
        }
    done
    build_context=$(mktemp -d)
    trap 'rm -rf -- "$build_context"' EXIT
    cp Cargo.toml Cargo.lock README.md LICENSE.md "$build_context/"
    mkdir -p "$build_context/bin/orvek"
    cp bin/orvek/Cargo.toml bin/orvek/build.rs "$build_context/bin/orvek/"
    cp -R bin/orvek/src "$build_context/bin/orvek/src"
    mkdir -p "$build_context/crates/executor"
    cp crates/executor/Cargo.toml crates/executor/README.md "$build_context/crates/executor/"
    cp -R crates/executor/src "$build_context/crates/executor/src"
    mkdir -p "$build_context/crates/harness"
    cp crates/harness/Cargo.toml crates/harness/build.rs "$build_context/crates/harness/"
    cp -R crates/harness/src "$build_context/crates/harness/src"
    cp -R crates/harness/benches "$build_context/crates/harness/benches"
    mkdir -p "$build_context/crates/memory"
    cp crates/memory/Cargo.toml crates/memory/README.md "$build_context/crates/memory/"
    cp -R crates/memory/src "$build_context/crates/memory/src"
    if [[ -n "{{platform}}" ]]; then
        docker --context "$docker_context" buildx build \
            --platform "{{platform}}" \
            --file evals/harbor_adapter/orvek.Dockerfile \
            --target artifact \
            --output type=local,dest=.orvek/installed \
            "$build_context"
    else
        docker --context "$docker_context" buildx build \
            --file evals/harbor_adapter/orvek.Dockerfile \
            --target artifact \
            --output type=local,dest=.orvek/installed \
            "$build_context"
    fi

# Validate the adapter and resolved Harbor configuration without running a task.
check-harbor:
    uv sync --project evals --frozen
    PYTHONDONTWRITEBYTECODE=1 uv run --project evals python -m unittest harbor_adapter.test_agent -v
    cargo test --locked --features harbor-evals --bin orvek app::cli::tests
    uv run --project evals harbor run \
        --config evals/terminal-bench.yaml \
        --dataset terminal-bench/terminal-bench-2-1@6 \
        --include-task-name terminal-bench/openssl-selfsigned-cert \
        --print-config >/dev/null

# Run the pinned Terminal-Bench configuration. Additional Harbor flags are forwarded.
harbor-eval *args='':
    #!/usr/bin/env bash
    set -euo pipefail
    just --quiet _local-docker-context >/dev/null
    just --quiet _terminal-bench-platform
    test -x .orvek/installed/orvek || {
        echo 'missing .orvek/installed/orvek; run `just build-harbor-agent`' >&2
        exit 1
    }
    test -x .orvek/installed/orvek-executor-linux-x86_64 \
        || test -x .orvek/installed/orvek-executor-linux-aarch64 || {
        echo 'missing architecture-matched Harbor executor; run `just build-harbor-agent`' >&2
        exit 1
    }
    if [[ -n "${ORVEK_CODEX_AUTH_FILE:-}" ]]; then
        auth_file="$ORVEK_CODEX_AUTH_FILE"
    elif [[ -n "${CODEX_HOME:-}" ]]; then
        auth_file="$CODEX_HOME/auth.json"
    else
        auth_file="$HOME/.codex/auth.json"
    fi
    test -f "$auth_file" || {
        echo "missing Codex auth file: $auth_file" >&2
        echo 'configure Codex file credential storage and log in again' >&2
        exit 1
    }
    uv run --project evals harbor run \
        --config evals/terminal-bench.yaml \
        --dataset terminal-bench/terminal-bench-2-1@6 \
        --agent-kwarg "auth_file=$auth_file" \
        {{args}}

# Browse retained Harbor jobs and Orvek trajectories.
harbor-view *args='':
    uv run --project evals harbor view .orvek/harbor/jobs --jobs {{args}}
