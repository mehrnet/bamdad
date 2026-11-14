#!/usr/bin/env bash
# Build the userspace WireGuard proxy helper (vendored wireproxy, ISC).
#
# The helper runs a WireGuard session entirely in userspace and exposes it as
# a local SOCKS5 proxy, so a WireGuard config can be used as a chain hop
# without occupying Android's single VpnService slot.
#
# Usage:
#   scripts/build-wireguard-proxy.sh                       # host binary
#   scripts/build-wireguard-proxy.sh --host /path/bin      # host binary
#   scripts/build-wireguard-proxy.sh --android <jniDir> arm64-v8a x86_64
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WP="$ROOT/vendor/wireproxy"
VERSION="v1.1.3"

if [[ ! -f "$WP/go.mod" ]]; then
  echo "wireproxy sources missing at $WP" >&2
  echo "Run: git submodule update --init --recursive" >&2
  exit 1
fi

build_host() {
  local out="${1:-$ROOT/target/debug/bin/bamdad-wg}"
  mkdir -p "$(dirname "$out")"
  echo "Building userspace WireGuard proxy for $(go env GOOS)/$(go env GOARCH)"
  (
    cd "$WP"
    CGO_ENABLED=0 go build -trimpath -ldflags "-s -w -X main.version=$VERSION" \
      -o "$out" ./cmd/wireproxy
  )
  echo "WireGuard proxy helper: $out"
}

build_android() {
  local jni_dir="$1"
  shift
  local abis=("$@")
  [[ ${#abis[@]} -gt 0 ]] || abis=(arm64-v8a x86_64)

  local ndk="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
  local toolchain=""
  if [[ -n "$ndk" ]]; then
    local host_tag
    host_tag="$(ls "$ndk/toolchains/llvm/prebuilt" | head -1)"
    toolchain="$ndk/toolchains/llvm/prebuilt/$host_tag/bin"
  fi

  for abi in "${abis[@]}"; do
    local arch cc cgo
    case "$abi" in
      arm64-v8a)
        arch=arm64
        cgo=0
        ;;
      x86_64)
        arch=amd64
        # Go's android/amd64 port requires external (cgo) linking.
        cgo=1
        cc="$toolchain/x86_64-linux-android26-clang"
        if [[ -z "$toolchain" || ! -x "$cc" ]]; then
          echo "NDK clang not found for android/amd64 ($cc); set ANDROID_NDK_HOME" >&2
          exit 1
        fi
        ;;
      *)
        echo "Unsupported ABI: $abi" >&2
        exit 1
        ;;
    esac

    mkdir -p "$jni_dir/$abi"
    echo "Building userspace WireGuard proxy for android/$arch ($abi)"
    (
      cd "$WP"
      CC="${cc:-}" CGO_ENABLED="$cgo" GOOS=android GOARCH="$arch" \
        go build -trimpath -ldflags "-s -w -X main.version=$VERSION" \
        -o "$jni_dir/$abi/libbamdad_wg.so" ./cmd/wireproxy
    )
  done
}

case "${1:-}" in
  --android)
    shift
    jni_dir="${1:?usage: --android <jniDir> <abi...>}"
    shift
    build_android "$jni_dir" "$@"
    ;;
  --host)
    shift
    build_host "${1:-}"
    ;;
  "")
    build_host
    ;;
  *)
    echo "unknown argument: $1" >&2
    exit 1
    ;;
esac
