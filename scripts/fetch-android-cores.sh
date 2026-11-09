#!/usr/bin/env bash
# Download Android proxy cores and expose them as libbamdad_*.so so Android
# extracts them to nativeLibraryDir.
#
#   scripts/fetch-android-cores.sh <jniLibs-dir> [abi ...]
#
# Xray publishes android-arm64-v8a and android-amd64. sing-box publishes
# android-arm64 and android-amd64 (also arm/386, unused here).
set -euo pipefail

OUT="${1:?usage: fetch-android-cores.sh <jniLibs-dir> [abi ...]}"
shift || true
ABIS=("$@")
if [ "${#ABIS[@]}" -eq 0 ]; then
    ABIS=(arm64-v8a x86_64)
fi

xray_version="${XRAY_VERSION:-latest}"
singbox_version="${SINGBOX_VERSION:-latest}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

download() {
    local url="$1" dest="$2"
    echo "Downloading $url"
    curl -fL --retry 5 --retry-all-errors --connect-timeout 20 -o "$dest" "$url"
}

xray_base="https://github.com/XTLS/Xray-core/releases"
if [[ "$xray_version" == "latest" ]]; then
    xray_url_prefix="$xray_base/latest/download"
else
    xray_tag="$xray_version"
    [[ "$xray_tag" == v* ]] || xray_tag="v$xray_tag"
    xray_url_prefix="$xray_base/download/$xray_tag"
fi

singbox_tag="$singbox_version"
if [[ "$singbox_tag" == "latest" ]]; then
    singbox_tag="$(curl -fsSL --retry 3 https://api.github.com/repos/SagerNet/sing-box/releases/latest \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
    if [[ -z "$singbox_tag" ]]; then
        echo "could not determine the latest sing-box version" >&2
        exit 1
    fi
fi
[[ "$singbox_tag" == v* ]] || singbox_tag="v$singbox_tag"
singbox_ver="${singbox_tag#v}"

for abi in "${ABIS[@]}"; do
    mkdir -p "$OUT/$abi"

    case "$abi" in
        arm64-v8a)
            xray_asset="Xray-android-arm64-v8a.zip"
            sb_arch="arm64"
            ;;
        x86_64)
            xray_asset="Xray-android-amd64.zip"
            sb_arch="amd64"
            ;;
        armeabi-v7a)
            # Xray does not publish an arm32 Android binary.
            xray_asset=""
            sb_arch="arm"
            ;;
        *)
            echo "unsupported ABI: $abi" >&2
            exit 1
            ;;
    esac

    if [[ -n "$xray_asset" ]]; then
        xray_zip="$tmp/$abi-xray.zip"
        download "$xray_url_prefix/$xray_asset" "$xray_zip"
        mkdir -p "$tmp/$abi-xray"
        unzip -q -o "$xray_zip" xray -d "$tmp/$abi-xray"
        cp "$tmp/$abi-xray/xray" "$OUT/$abi/libbamdad_xray.so"
        chmod 755 "$OUT/$abi/libbamdad_xray.so"
    else
        echo "skipping Xray for $abi (no upstream Android binary)" >&2
    fi

    sb_asset="sing-box-${singbox_ver}-android-${sb_arch}.tar.gz"
    sb_tar="$tmp/$abi-singbox.tar.gz"
    download "https://github.com/SagerNet/sing-box/releases/download/${singbox_tag}/${sb_asset}" "$sb_tar"
    mkdir -p "$tmp/$abi-singbox"
    tar -xzf "$sb_tar" -C "$tmp/$abi-singbox"
    sb_bin="$(find "$tmp/$abi-singbox" -type f -name 'sing-box' | head -1)"
    if [[ -z "$sb_bin" ]]; then
        echo "could not find sing-box in $sb_asset" >&2
        exit 1
    fi
    cp "$sb_bin" "$OUT/$abi/libbamdad_singbox.so"
    chmod 755 "$OUT/$abi/libbamdad_singbox.so"
done

echo "Android cores installed under $OUT"
