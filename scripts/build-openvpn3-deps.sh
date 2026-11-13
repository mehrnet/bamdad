#!/usr/bin/env bash
# Cross-build the native dependencies required by the OpenVPN 3 core library
# (pinned at vendor/openvpn3 release/3.11.8) for Android.
#
# Outputs static libraries and headers into:
#   crates/bamdad-app/android/openvpn3-tunnel/deps/<abi>
#
# Usage: scripts/build-openvpn3-deps.sh [abi ...]
#        (default: arm64-v8a x86_64)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
O3="$REPO_ROOT/crates/bamdad-app/android/vendor/openvpn3"
PREFIX_ROOT="$REPO_ROOT/crates/bamdad-app/android/openvpn3-tunnel/deps"
CACHE="${OPENVPN3_DEPS_CACHE:-$REPO_ROOT/.native-deps/openvpn3/cache}"
BUILD="${OPENVPN3_DEPS_BUILD:-$REPO_ROOT/.native-deps/openvpn3/build}"
JOBS="${JOBS:-$(nproc)}"
ANDROID_PLATFORM="${ANDROID_PLATFORM:-29}"

# Versions are taken from the OpenVPN 3 tree (deps/lib-versions).
ASIO_VERSION=asio-1-24-0
ASIO_SHA=cbcaaba0f66722787b1a7c33afe1befb3a012b5af3ad7da7ff0f6b8c9b7a8a5b
LZ4_VERSION=1.8.3
LZ4_SHA=33af5936ac06536805f9745e0b6d61da606a1f8b4cc5c04dd3cbaca3b9b4fc43
MBEDTLS_VERSION=2.28.4
MBEDTLS_SHA=578c4dcd15bbff3f5cd56aa07cd4f850fc733634e3d5947be4f7157d5bfd81ac
JSONCPP_VERSION=1.8.4
JSONCPP_SHA=c49deac9e0933bcb7044f08516861a2d560988540b23de2ac1ad443b219afdb6

SDK="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-$SDK/ndk/27.0.12077973}}"
[[ -f "$NDK/build/cmake/android.toolchain.cmake" ]] || {
  echo "Android NDK not found at '$NDK' (set ANDROID_NDK_HOME)" >&2
  exit 1
}

# Prefer the CMake/Ninja shipped with the Android SDK.
CMAKE="${CMAKE_BIN:-}"
if [[ -z "$CMAKE" ]]; then
  for candidate in "$SDK"/cmake/*/bin/cmake; do
    [[ -x "$candidate" ]] && CMAKE="$candidate"
  done
fi
CMAKE="${CMAKE:-cmake}"
if [[ -x "$(dirname "$CMAKE")/ninja" ]]; then
  export PATH="$(dirname "$CMAKE"):$PATH"
fi
command -v ninja >/dev/null || {
  echo "ninja not found (install it or the Android SDK cmake package)" >&2
  exit 1
}

if [[ $# -gt 0 ]]; then
  ABIS=("$@")
else
  ABIS=(arm64-v8a x86_64)
fi

mkdir -p "$CACHE" "$BUILD"

download() {
  local url="$1" file="$2" sha="$3"
  if [[ -f "$CACHE/$file" ]] && echo "$sha  $CACHE/$file" | sha256sum -c - >/dev/null 2>&1; then
    return 0
  fi
  echo "Downloading $file"
  curl -fL --retry 5 --retry-all-errors -o "$CACHE/$file" "$url"
  echo "$sha  $CACHE/$file" | sha256sum -c -
}

extract() {
  local file="$1" dir="$2"
  if [[ ! -d "$BUILD/$dir" ]]; then
    mkdir -p "$BUILD/$dir"
    tar -xf "$CACHE/$file" -C "$BUILD/$dir" --strip-components=1
  fi
}

download "https://github.com/chriskohlhoff/asio/archive/${ASIO_VERSION}.tar.gz" \
  "${ASIO_VERSION}.tar.gz" "$ASIO_SHA"
download "https://github.com/lz4/lz4/archive/v${LZ4_VERSION}.tar.gz" \
  "lz4-v${LZ4_VERSION}.tar.gz" "$LZ4_SHA"
download "https://github.com/ARMmbed/mbedtls/archive/refs/tags/v${MBEDTLS_VERSION}.tar.gz" \
  "mbedtls-v${MBEDTLS_VERSION}.tar.gz" "$MBEDTLS_SHA"
download "https://github.com/open-source-parsers/jsoncpp/archive/refs/tags/${JSONCPP_VERSION}.tar.gz" \
  "jsoncpp-${JSONCPP_VERSION}.tar.gz" "$JSONCPP_SHA"

extract "${ASIO_VERSION}.tar.gz" asio
extract "lz4-v${LZ4_VERSION}.tar.gz" lz4
extract "mbedtls-v${MBEDTLS_VERSION}.tar.gz" mbedtls
extract "jsoncpp-${JSONCPP_VERSION}.tar.gz" jsoncpp

ASIO_SRC="$BUILD/asio"
if [[ ! -f "$ASIO_SRC/.openvpn-patched" ]]; then
  for patch_file in "$O3"/deps/asio/patches/*.patch; do
    echo "Applying $(basename "$patch_file")"
    patch -p1 -s -d "$ASIO_SRC" < "$patch_file"
  done
  touch "$ASIO_SRC/.openvpn-patched"
fi

for abi in "${ABIS[@]}"; do
  prefix="$PREFIX_ROOT/$abi"
  mkdir -p "$prefix/include" "$prefix/lib"

  case "$abi" in
    arm64-v8a) triple=aarch64-linux-android ;;
    armeabi-v7a) triple=armv7a-linux-androideabi ;;
    x86) triple=i686-linux-android ;;
    x86_64) triple=x86_64-linux-android ;;
    *) echo "Unsupported ABI: $abi" >&2; exit 1 ;;
  esac
  host_tag="$(ls "$NDK/toolchains/llvm/prebuilt" | head -1)"
  toolchain="$NDK/toolchains/llvm/prebuilt/$host_tag"
  clang="$toolchain/bin/${triple}${ANDROID_PLATFORM}-clang"
  llvm_ar="$toolchain/bin/llvm-ar"

  build_mbedtls() {
    local out="$BUILD/out-mbedtls-$abi"
    rm -rf "$out"
    "$CMAKE" -S "$BUILD/mbedtls" -B "$out" -G Ninja \
      -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$ANDROID_PLATFORM" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DENABLE_PROGRAMS=OFF -DENABLE_TESTING=OFF -DENABLE_ZLIB_SUPPORT=OFF \
      -DUSE_STATIC_MBEDTLS_LIBRARY=ON -DUSE_SHARED_MBEDTLS_LIBRARY=OFF >/dev/null
    "$CMAKE" --build "$out" --target install -j "$JOBS" >/dev/null
  }

  build_jsoncpp() {
    local out="$BUILD/out-jsoncpp-$abi"
    rm -rf "$out"
    "$CMAKE" -S "$BUILD/jsoncpp" -B "$out" -G Ninja \
      -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
      -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
      -DANDROID_ABI="$abi" -DANDROID_PLATFORM="android-$ANDROID_PLATFORM" \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_INSTALL_PREFIX="$prefix" \
      -DBUILD_SHARED_LIBS=OFF -DJSONCPP_WITH_TESTS=OFF \
      -DJSONCPP_WITH_POST_BUILD_UNITTEST=OFF >/dev/null
    "$CMAKE" --build "$out" --target install -j "$JOBS" >/dev/null
  }

  build_lz4() {
    # LZ4 1.8.3 has no CMake build; compile the two library sources directly.
    local out="$BUILD/out-lz4-$abi"
    rm -rf "$out"
    mkdir -p "$out"
    "$clang" -O3 -fPIC -I"$BUILD/lz4/lib" -c "$BUILD/lz4/lib/lz4.c" -o "$out/lz4.o"
    "$clang" -O3 -fPIC -I"$BUILD/lz4/lib" -c "$BUILD/lz4/lib/lz4hc.c" -o "$out/lz4hc.o"
    "$llvm_ar" rcs "$prefix/lib/liblz4.a" "$out/lz4.o" "$out/lz4hc.o"
    cp "$BUILD/lz4/lib/lz4.h" "$BUILD/lz4/lib/lz4hc.h" "$prefix/include/"
  }

  if [[ ! -f "$prefix/lib/libmbedtls.a" || "${FORCE_DEPS:-0}" == "1" ]]; then
    build_mbedtls
  fi
  if [[ ! -f "$prefix/lib/libjsoncpp.a" || "${FORCE_DEPS:-0}" == "1" ]]; then
    build_jsoncpp
  fi
  if [[ ! -f "$prefix/lib/liblz4.a" || "${FORCE_DEPS:-0}" == "1" ]]; then
    build_lz4
  fi

  # asio is header-only.
  rm -rf "$prefix/include/asio" "$prefix/include/asio.hpp"
  cp -R "$ASIO_SRC/asio/include/asio" "$prefix/include/"
  cp "$ASIO_SRC/asio/include/asio.hpp" "$prefix/include/"

  echo "OpenVPN 3 deps ready for $abi: $prefix"
done
