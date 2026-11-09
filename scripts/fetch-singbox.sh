#!/usr/bin/env bash
# Download a matching sing-box release and install it into Bamdad's data dir.
#
#   ./scripts/fetch-singbox.sh                 # latest release
#   SINGBOX_VERSION=1.11.15 ./scripts/fetch-singbox.sh
set -euo pipefail

repo="SagerNet/sing-box"
version="${SINGBOX_VERSION:-latest}"
os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)
    case "$arch" in
      x86_64) platform="linux-amd64" ;;
      aarch64 | arm64) platform="linux-arm64" ;;
      armv7l) platform="linux-armv7" ;;
      *) echo "unsupported Linux arch: $arch" >&2; exit 1 ;;
    esac
    data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/bamdad"
    archive_ext="tar.gz"
    ;;
  Darwin)
    case "$arch" in
      x86_64) platform="darwin-amd64" ;;
      arm64) platform="darwin-arm64" ;;
      *) echo "unsupported macOS arch: $arch" >&2; exit 1 ;;
    esac
    data_dir="$HOME/Library/Application Support/bamdad"
    archive_ext="tar.gz"
    ;;
  MINGW* | MSYS* | CYGWIN*)
    case "$arch" in
      x86_64) platform="windows-amd64" ;;
      aarch64 | arm64) platform="windows-arm64" ;;
      *) echo "unsupported Windows arch: $arch" >&2; exit 1 ;;
    esac
    data_dir="${APPDATA:-$HOME/AppData/Roaming}/bamdad"
    archive_ext="zip"
    ;;
  *)
    echo "unsupported OS: $os" >&2
    exit 1
    ;;
esac

if [[ "$version" == "latest" ]]; then
  version="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  if [[ -z "$version" ]]; then
    echo "could not determine the latest sing-box version" >&2
    exit 1
  fi
fi

tag="$version"
[[ "$tag" == v* ]] || tag="v$tag"
asset_version="${tag#v}"
asset="sing-box-${asset_version}-${platform}.${archive_ext}"
url="https://github.com/$repo/releases/download/${tag}/${asset}"

echo "Downloading $url"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if command -v curl >/dev/null 2>&1; then
  curl -fL --retry 3 -o "$tmp/$asset" "$url"
else
  wget -O "$tmp/$asset" "$url"
fi

mkdir -p "$tmp/extract"
case "$archive_ext" in
  tar.gz) tar -xzf "$tmp/$asset" -C "$tmp/extract" ;;
  zip)
    if command -v unzip >/dev/null 2>&1; then
      unzip -q "$tmp/$asset" -d "$tmp/extract"
    else
      tar -xf "$tmp/$asset" -C "$tmp/extract"
    fi
    ;;
esac

binary="sing-box"
[[ "$os" == MINGW* || "$os" == MSYS* || "$os" == CYGWIN* ]] && binary="sing-box.exe"

found="$(find "$tmp/extract" -type f -name "$binary" | head -n 1)"
if [[ -z "$found" ]]; then
  echo "could not find $binary in the archive" >&2
  exit 1
fi

core_root="${BAMDAD_CORE_DIR:-$data_dir}"
mkdir -p "$core_root/bin" "$core_root/assets"
cp "$found" "$core_root/bin/$binary"
chmod +x "$core_root/bin/$binary" 2>/dev/null || true
echo "Installed sing-box into $core_root"
"$core_root/bin/$binary" version | head -n 2
