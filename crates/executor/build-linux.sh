#!/bin/sh
set -eu

# Build a static sidecar for the container image architecture, not the host OS.
case "${1:-}" in
  aarch64|arm64) arch=aarch64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) echo 'usage: crates/executor/build-linux.sh aarch64|x86_64' >&2; exit 64 ;;
esac
repo=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
target="$arch-unknown-linux-musl"
rust_host=$(rustc -vV | sed -n 's/^host: //p')
sysroot=$(rustc --print sysroot)
linker="$sysroot/lib/rustlib/$rust_host/bin/rust-lld"
if [ ! -x "$linker" ]; then
  echo "Rust's bundled rust-lld is required: $linker" >&2
  exit 1
fi
rustup target add "$target"
cd "$repo"
case "$arch" in
  aarch64) CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$linker" cargo build -p orvek-executor --features stub --target "$target" --release ;;
  x86_64) CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$linker" cargo build -p orvek-executor --features stub --target "$target" --release ;;
esac
out="$repo/target/executor/orvek-executor-linux-$arch"
mkdir -p "$repo/target/executor"
cp "$repo/target/$target/release/orvek-executor" "$out.tmp"
chmod 755 "$out.tmp"
mv "$out.tmp" "$out"
printf '%s\n' "$out"
