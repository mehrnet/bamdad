#!/usr/bin/env bash
# Download a matching Xray-core release and install it into Bamdad's data dir.
#
#   ./scripts/fetch-xray.sh              # latest release
#   XRAY_VERSION=v25.9.11 ./scripts/fetch-xray.sh
#
# The binary is placed in <data>/bin/, geo assets in <data>/assets/.
set -euo pipefail

version="${XRAY_VERSION:-latest}"
os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)
    case "$arch" in
      x86_64) asset="Xray-linux-64.zip" ;;
      aarch64 | arm64) asset="Xray-linux-arm64-v8a.zip" ;;
      armv7l) asset="Xray-linux-arm32-v7a.zip" ;;
      *) echo "unsupported Linux arch: $arch" >&2; exit 1 ;;
    esac
    data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/bamdad"
    ;;
  Darwin)
    case "$arch" in
      x86_64) asset="Xray-macos-64.zip" ;;
      arm64) asset="Xray-macos-arm64-v8a.zip" ;;
      *) echo "unsupported macOS arch: $arch" >&2; exit 1 ;;
    esac
    data_dir="$HOME/Library/Application Support/bamdad"
    ;;
  MINGW* | MSYS* | CYGWIN*)
    case "$arch" in
      x86_64) asset="Xray-windows-64.zip" ;;
      aarch64 | arm64) asset="Xray-windows-arm64-v8a.zip" ;;
      *) echo "unsupported Windows arch: $arch" >&2; exit 1 ;;
    esac
    data_dir="${APPDATA:-$HOME/AppData/Roaming}/bamdad"
    ;;
  *)
    echo "unsupported OS: $os" >&2
    exit 1
    ;;
esac

tag="$version"
if [[ "$tag" == "latest" ]]; then
  url="https://github.com/XTLS/Xray-core/releases/latest/download/$asset"
else
  [[ "$tag" == v* ]] || tag="v$tag"
  url="https://github.com/XTLS/Xray-core/releases/download/$tag/$asset"
fi

echo "Downloading $url"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if command -v curl >/dev/null 2>&1; then
  curl -fL --retry 3 -o "$tmp/$asset" "$url"
else
  wget -O "$tmp/$asset" "$url"
fi

mkdir -p "$tmp/xray"
if command -v unzip >/dev/null 2>&1; then
  unzip -q "$tmp/$asset" -d "$tmp/xray"
else
  tar -xf "$tmp/$asset" -C "$tmp/xray"
fi

core_root="${BAMDAD_CORE_DIR:-$data_dir}"
mkdir -p "$core_root/bin" "$core_root/assets"

binary="xray"
[[ "$os" == MINGW* || "$os" == MSYS* || "$os" == CYGWIN* ]] && binary="xray.exe"

cp "$tmp/xray/$binary" "$core_root/bin/$binary"
chmod +x "$core_root/bin/$binary" 2>/dev/null || true
for asset_file in geoip.dat geosite.dat; do
  if [[ -f "$tmp/xray/$asset_file" ]]; then
    cp "$tmp/xray/$asset_file" "$core_root/assets/$asset_file"
  fi
done

echo "Installed Xray-core into $core_root"
"$core_root/bin/$binary" version | head -n 1
