#!/usr/bin/env bash
# Build xjasonlyu/tun2socks for the host platform so the desktop app can feed it
# a tun descriptor from the privileged helper (`-device fd://3`).
#
#   scripts/fetch-desktop-tun2socks.sh <output-dir>
#
# The binary is named bamdad-tun2socks and is expected to live next to the app
# binary (or be pointed at with BAMDAD_TUN2SOCKS).
set -euo pipefail

OUT="${1:?usage: fetch-desktop-tun2socks.sh <output-dir>}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
VERSION="${TUN2SOCKS_VERSION:-v2.7.0}"

ext=""
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) goos=linux; goarch=amd64 ;;
    Linux-aarch64 | Linux-arm64) goos=linux; goarch=arm64 ;;
    Darwin-x86_64) goos=darwin; goarch=amd64 ;;
    Darwin-arm64) goos=darwin; goarch=arm64 ;;
    MINGW* | MSYS* | CYGWIN*)
        goos=windows
        goarch=amd64
        ext=".exe"
        ;;
    *)
        echo "unsupported host: $(uname -s)-$(uname -m)" >&2
        exit 1
        ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

git clone -q --depth 1 --branch "$VERSION" \
    https://github.com/xjasonlyu/tun2socks "$tmp/tun2socks"

mkdir -p "$OUT"
(
    cd "$tmp/tun2socks"
    CGO_ENABLED=0 GOOS="$goos" GOARCH="$goarch" \
        go build -trimpath -ldflags "-s -w" -o "$OUT/bamdad-tun2socks$ext" .
)
chmod +x "$OUT/bamdad-tun2socks$ext"
ls -lh "$OUT/bamdad-tun2socks$ext"
