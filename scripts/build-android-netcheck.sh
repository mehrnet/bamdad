#!/usr/bin/env bash
# Build the tiny Android network checker pushed by the emulator smoke test.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/netcheck-out}"

: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"

TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
LINKER="$TOOLCHAIN/x86_64-linux-android26-clang"
test -x "$LINKER"

export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$LINKER"
export CC_x86_64_linux_android="$LINKER"

cargo build \
    --manifest-path "$ROOT/Cargo.toml" \
    --release --locked \
    --target x86_64-linux-android \
    -p bamdad-netcheck

rm -rf "$OUT"
mkdir -p "$OUT/x86_64"
cp "$ROOT/target/x86_64-linux-android/release/bamdad-netcheck" "$OUT/x86_64/bamdad-netcheck"

echo "Android netcheck binaries:"
find "$OUT" -type f -printf '  %P (%s bytes)\n' | sort
