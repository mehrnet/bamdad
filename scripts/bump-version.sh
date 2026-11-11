#!/usr/bin/env bash
# Bump Bamdad's odometer-style version: major.minor.patch where patch is a
# single digit. 0.3.9 -> 0.4.0. There is no standard SemVer name for this; it is
# a base-10 / odometer carry scheme.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

current="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"
IFS=. read -r major minor patch <<< "$current"
if [[ ! "$major" =~ ^[0-9]+$ || ! "$minor" =~ ^[0-9]+$ || ! "$patch" =~ ^[0-9]$ ]]; then
    echo "current version '$current' is not in major.minor.patch (0-9) form" >&2
    exit 1
fi
patch=$((10#$patch + 1))
if ((patch > 9)); then
    patch=0
    minor=$((10#$minor + 1))
fi
next="$major.$minor.$patch"

sed -i "s/^version = \"$current\"/version = \"$next\"/" Cargo.toml
sed -i "s/?: \"$current\"/?: \"$next\"/" crates/bamdad-app/android/app/build.gradle.kts
sed -i "s/v$current/v$next/g" README.md
# Keep Cargo.lock's workspace versions in sync so --locked release builds work.
cargo update -w >/dev/null
echo "$current -> $next"
