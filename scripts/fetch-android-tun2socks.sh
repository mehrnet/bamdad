#!/usr/bin/env bash
# Build xjasonlyu/tun2socks for Android and package it as a native-library
# executable, so the app can start it with the VpnService fd (`fd://N`).
#
#   scripts/fetch-android-tun2socks.sh <jniLibs-dir> [abi ...]
set -euo pipefail

OUT="${1:?usage: fetch-android-tun2socks.sh <jniLibs-dir> [abi ...]}"
shift || true
ABIS=("$@")
if [ "${#ABIS[@]}" -eq 0 ]; then
    ABIS=(arm64-v8a x86_64)
fi

VERSION="${TUN2SOCKS_VERSION:-v2.7.0}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

git clone -q --depth 1 --branch "$VERSION" \
    https://github.com/xjasonlyu/tun2socks "$tmp/tun2socks"

: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
API="${ANDROID_API:-26}"

for abi in "${ABIS[@]}"; do
    case "$abi" in
        arm64-v8a)
            goarch="arm64"
            triple="aarch64-linux-android"
            ;;
        x86_64)
            goarch="amd64"
            triple="x86_64-linux-android"
            ;;
        *)
            echo "unsupported ABI for tun2socks: $abi" >&2
            exit 1
            ;;
    esac

    mkdir -p "$OUT/$abi"
    echo "Building tun2socks $VERSION for $abi"

    (
        cd "$tmp/tun2socks"
        CGO_ENABLED=1 \
            GOOS=android \
            GOARCH="$goarch" \
            CC="$TOOLCHAIN/${triple}${API}-clang" \
            CXX="$TOOLCHAIN/${triple}${API}-clang++" \
            AR="$TOOLCHAIN/llvm-ar" \
            go build -trimpath -buildmode=pie -ldflags="-s -w" \
            -o "$OUT/$abi/libbamdad_tun2socks.so" .
    )
done

echo "Android tun2socks binaries:"
find "$OUT" -name 'libbamdad_tun2socks.so' -printf '  %P (%s bytes)\n' | sort
