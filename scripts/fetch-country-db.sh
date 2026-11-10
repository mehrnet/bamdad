#!/usr/bin/env bash
# Fetch the pinned MehrNet country database used by the in-app IP lookup.
set -euo pipefail

TAG="${COUNTRY_DB_TAG:-db-2026.11.10-1559-151}"
OUT="${1:-crates/bamdad-app/assets}"
REPO="${COUNTRY_DB_REPO:-mehrnet/bgp-api}"
BASE="https://github.com/${REPO}/releases/download/${TAG}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fL --retry 4 --retry-all-errors -o "$TMP/country.mncc" "$BASE/country.mncc"
curl -fL --retry 4 --retry-all-errors -o "$TMP/SHA256SUMS.txt" "$BASE/SHA256SUMS.txt"

(
    cd "$TMP"
    grep -E ' country\.mncc$' SHA256SUMS.txt | sha256sum -c -
)

mkdir -p "$OUT"
cp "$TMP/country.mncc" "$OUT/country.mncc"
echo "country database ${TAG} -> $OUT/country.mncc"
