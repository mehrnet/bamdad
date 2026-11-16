#!/usr/bin/env bash
# Build strongSwan's charon IKEv2 daemon for Android.
#
# The Android frontend's ndk-build project links libstrongswan, libipsec,
# libcharon, the selected plugins and the libandroidbridge JNI layer into a
# single libcharon.so per ABI. The Kotlin IpSecBackend will load that library;
# nothing in the app depends on it yet.
#
# Usage:
#   ANDROID_NDK_ROOT=~/ndk OPENSSL_SRC=~/openssl-3.5.4 \
#     scripts/build-strongswan-android.sh [abi ...]
#
# Environment:
#   ANDROID_NDK_ROOT  NDK 27+ (required)
#   OPENSSL_SRC       OpenSSL 3 source tree (required, built statically)
#   STRONGSWAN_SRC    strongSwan tree (default: vendored submodule)
#   OUT               output root (default: build/strongswan-android)
#
# Output: $OUT/<abi>/libcharon.so and friends, plus $OUT/ndk-build.log.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
NDK=${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-}}
SRC=${STRONGSWAN_SRC:-"$ROOT/crates/bamdad-app/android/vendor/strongswan"}
OPENSSL_SRC=${OPENSSL_SRC:-}
OUT=${OUT:-"$ROOT/build/strongswan-android"}

if [ $# -gt 0 ]; then
    ABIS=("$@")
else
    ABIS=(arm64-v8a x86_64)
fi

die() {
    echo "build-strongswan-android: $*" >&2
    exit 1
}

[ -n "$NDK" ] && [ -d "$NDK" ] || die "set ANDROID_NDK_ROOT to an NDK directory"
[ -n "$OPENSSL_SRC" ] && [ -d "$OPENSSL_SRC" ] || die "set OPENSSL_SRC to an OpenSSL 3 tree"
[ -d "$SRC/src/frontends/android/app/src/main/jni" ] || die "no Android frontend in $SRC"

# ndk-build wants a comma list, the OpenSSL helper a space-separated one.
abis_csv=$(printf '%s\n' "${ABIS[@]}" | paste -sd, -)
abis_space="${ABIS[*]}"
echo "strongSwan: $SRC"
echo "NDK:        $NDK"
echo "OpenSSL:    $OPENSSL_SRC"
echo "ABIs:       ${ABIS[*]}"

# Work on a copy so Android.common.mk and the static libcrypto can be placed in
# the tree without dirtying the vendored submodule.
STAGE="$OUT/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -a "$SRC" "$STAGE/src"
rm -rf "$STAGE/src/.git"

version=$(sed -n 's/^AC_INIT(\[[^]]*\],\[\([^]]*\)\].*/\1/p' "$STAGE/src/configure.ac" | head -1)
[ -n "$version" ] || die "could not read the strongSwan version from configure.ac"
sed "s:@PACKAGE_VERSION@:$version:" "$STAGE/src/Android.common.mk.in" \
    > "$STAGE/src/Android.common.mk"
echo "strongSwan version: $version"

# The NDK build compiles sources that the autotools build generates: the ASN.1
# OID tables, the static proposal keywords, and the settings parser/lexer. The
# upstream instructions run a full configure+make; generating only those files
# keeps the build short and avoids compiling strongSwan for the host.
echo "=== preparing generated sources ==="
(
    cd "$STAGE/src"
    ./autogen.sh
    ./configure --disable-defaults --prefix=/usr --sysconfdir=/etc
    make -C src/libstrongswan \
        asn1/oid.c asn1/oid.h \
        crypto/proposal/proposal_keywords_static.h \
        crypto/proposal/proposal_keywords_static.c \
        settings/settings_parser.c settings/settings_parser.h \
        settings/settings_lexer.c
) > "$OUT/prepare.log" 2>&1

for generated in \
    src/libstrongswan/asn1/oid.c \
    src/libstrongswan/asn1/oid.h \
    src/libstrongswan/crypto/proposal/proposal_keywords_static.c \
    src/libstrongswan/settings/settings_parser.c \
    src/libstrongswan/settings/settings_lexer.c; do
    [ -f "$STAGE/src/$generated" ] || die "missing generated source $generated (see $OUT/prepare.log)"
done

JNI="$STAGE/src/src/frontends/android/app/src/main/jni"

# libstrongswan links a static libcrypto; the upstream helper builds it for
# every requested ABI and writes an Android.mk next to it.
echo "=== building OpenSSL for $abis_space ==="
NO_DOCKER=1 \
    ANDROID_NDK_ROOT="$NDK" \
    OPENSSL_SRC="$OPENSSL_SRC" \
    ABIS="$abis_space" \
    OUT="$JNI/openssl" \
    "$STAGE/src/src/frontends/android/openssl/build.sh"

echo "=== building charon for $abis_csv ==="
rm -rf "$OUT/obj" "$OUT/libs" "$OUT/$abis_csv"
mkdir -p "$OUT"
(
    cd "$STAGE/src/src/frontends/android/app/src/main"
    "$NDK/ndk-build" \
        APP_ABI="$abis_csv" \
        APP_PLATFORM=android-21 \
        NDK_OUT="$OUT/obj" \
        NDK_LIBS_OUT="$OUT/libs" \
        -j"$(nproc)"
) 2>&1 | tee "$OUT/ndk-build.log"

READELF="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf"
for abi in "${ABIS[@]}"; do
    mkdir -p "$OUT/$abi"
    cp "$OUT/libs/$abi/"*.so "$OUT/$abi/"
    test -f "$OUT/$abi/libcharon.so" || die "libcharon.so missing for $abi"
    machine=$("$READELF" -h "$OUT/$abi/libcharon.so" | sed -n 's/^ *Machine: *//p')
    echo "$abi: $machine"
    "$READELF" -d "$OUT/$abi/libcharon.so" | grep -q 'libdl.so\|libc.so' \
        || die "libcharon.so for $abi has unexpected dependencies"
    ls -lh "$OUT/$abi/"
done

echo "strongSwan Android libraries written to $OUT"
