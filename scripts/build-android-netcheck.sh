#!/usr/bin/env bash
# Build the tiny Android network checker pushed by the emulator smoke test.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/netcheck-out}"

: "${ANDROID_HOME:?set ANDROID_HOME}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"

rm -rf "$OUT"
mkdir -p "$OUT"

cargo ndk \
    --manifest-path "$ROOT/Cargo.toml" \
    --target=x86_64 \
    --platform 26 \
    -o "$OUT" \
    build --release --locked -p bamdad-netcheck

echo "Android netcheck binaries:"
find "$OUT" -type f -printf '  %P (%s bytes)\n' | sort
