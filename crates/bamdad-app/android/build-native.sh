#!/usr/bin/env bash
# Build the Slint Android cdylib and package the proxy cores as executable
# native libraries in jniLibs. Android extracts lib*.so files to
# nativeLibraryDir with the executable bit set, which is how the app starts the
# cores as child processes (the SimpleXray approach).
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$(cd "$PROJECT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$APP_DIR/../.." && pwd)"
JNI_DIR="$PROJECT_DIR/app/src/main/jniLibs"

ABIS=("$@")
if [ "${#ABIS[@]}" -eq 0 ]; then
    ABIS=(arm64-v8a x86_64)
fi

: "${ANDROID_HOME:?set ANDROID_HOME}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"

# Slint's build script compiles and embeds its Java helper.
export ANDROID_JAR="${ANDROID_JAR:-$ANDROID_HOME/platforms/android-35/android.jar}"
export ANDROID_BUILD_TOOLS_VERSION="${ANDROID_BUILD_TOOLS_VERSION:-35.0.0}"

rm -rf "$JNI_DIR"
mkdir -p "$JNI_DIR"

# rustls/ring need the NDK C compiler explicitly when cross-compiling.
NDK_TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
export CC_aarch64_linux_android="$NDK_TOOLCHAIN/aarch64-linux-android26-clang"
export AR_aarch64_linux_android="$NDK_TOOLCHAIN/llvm-ar"
export CC_x86_64_linux_android="$NDK_TOOLCHAIN/x86_64-linux-android26-clang"
export AR_x86_64_linux_android="$NDK_TOOLCHAIN/llvm-ar"

cargo ndk \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    "${ABIS[@]/#/--target=}" \
    --platform 26 \
    -o "$JNI_DIR" \
    build --release --locked -p bamdad-app --lib

"$REPO_ROOT/scripts/fetch-android-cores.sh" "$JNI_DIR" "${ABIS[@]}"
"$REPO_ROOT/scripts/fetch-android-tun2socks.sh" "$JNI_DIR" "${ABIS[@]}"
"$REPO_ROOT/scripts/build-wireguard-proxy.sh" --android "$JNI_DIR" "${ABIS[@]}"

echo "Android native libraries:"
find "$JNI_DIR" -type f -printf '  %P (%s bytes)\n' | sort
