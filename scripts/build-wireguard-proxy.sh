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

BUILD_SRC="${WIREPROXY_BUILD_DIR:-$ROOT/target/wireproxy-src}"

prepare_source() {
  if [[ -d "$BUILD_SRC" && -f "$BUILD_SRC/.bamdad-patched" ]]; then
    return 0
  fi
  rm -rf "$BUILD_SRC"
  mkdir -p "$BUILD_SRC"
  tar -cf - -C "$WP" --exclude=.git . | tar -xf - -C "$BUILD_SRC"
  # Android's seccomp policy SIGSYS-kills the landlock syscalls; the helper is
  # unprivileged and does not need that extra sandbox.
  python3 - "$BUILD_SRC/cmd/wireproxy/main.go" <<'PATCH'
import sys
path = sys.argv[1]
text = open(path, encoding="utf-8").read()
needle = "func lock(stage string, roFilles ...string) {\n"
assert needle in text, "wireproxy lock() signature changed"
insert = needle + '\tif os.Getenv("BAMDAD_NO_LANDLOCK") == "1" {\n\t\treturn\n\t}\n'
text = text.replace(needle, insert, 1)
open(path, "w", encoding="utf-8").write(text)
PATCH
  touch "$BUILD_SRC/.bamdad-patched"
}

build_host() {
  local out="${1:-$ROOT/target/debug/bin/bamdad-wg}"
  mkdir -p "$(dirname "$out")"
  echo "Building userspace WireGuard proxy for $(go env GOOS)/$(go env GOARCH)"
  prepare_source
  (
    cd "$BUILD_SRC"
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
        goos=android
        cgo=0
        ;;
      x86_64)
        # Go's android/amd64 port requires cgo linking, which crashed on the
        # emulator; a static linux/amd64 build runs fine on Android x86_64.
        arch=amd64
        goos=linux
        cgo=0
        cc=""
        ;;
      *)
        echo "Unsupported ABI: $abi" >&2
        exit 1
        ;;
    esac

    mkdir -p "$jni_dir/$abi"
    echo "Building userspace WireGuard proxy for android/$arch ($abi)"
    prepare_source
    (
      cd "$BUILD_SRC"
      CC="${cc:-}" CGO_ENABLED="$cgo" GOOS="$goos" GOARCH="$arch" \
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
