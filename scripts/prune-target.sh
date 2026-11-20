#!/usr/bin/env bash
# Keeps target/ from growing without bound.
#
# Test binaries and Slint build-script outputs accumulate one copy per distinct
# cargo fingerprint (different -p/--workspace/--target/env combinations), so a
# busy week can leave dozens of ~100 MB binaries behind. This removes the ones
# that have not been touched for a while and leaves the dependency cache alone
# unless --aggressive is given.
#
#   scripts/prune-target.sh                      # dry run, 14 days
#   scripts/prune-target.sh --apply              # actually delete
#   scripts/prune-target.sh --days 3 --apply
#   scripts/prune-target.sh --aggressive --apply # also stale incremental data
#
# Works with bash 3.2 (macOS) and GNU userland (Linux).
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
DAYS=14
APPLY=false
AGGRESSIVE=false

while [ $# -gt 0 ]; do
    case "$1" in
        --apply) APPLY=true ;;
        --aggressive) AGGRESSIVE=true ;;
        --days)
            shift
            DAYS=${1:?--days needs a number}
            ;;
        -h | --help)
            sed -n "2,15p" "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            exit 1
            ;;
    esac
    shift
done

[ -d "$TARGET" ] || {
    echo "nothing to prune: $TARGET does not exist"
    exit 0
}

size_kb() {
    du -sk "$1" 2>/dev/null | cut -f1
}

before=$(size_kb "$TARGET")
echo "target: $(du -sh "$TARGET" | cut -f1)"

# Directories to search, with the name prefix that belongs to this workspace.
search() {
    dir=$1
    prefix=$2
    [ -d "$dir" ] || return 0
    find "$dir" -maxdepth 1 -name "${prefix}*" -mtime "+$DAYS" 2>/dev/null || true
}

list=$(mktemp)
trap 'rm -f "$list"' EXIT
{
    search "$TARGET/debug/deps" "bamdad"
    search "$TARGET/debug/build" "bamdad-app-"
    search "$TARGET/debug/build" "bamdad-core-"
    search "$TARGET/debug/build" "bamdad-netcheck-"
    search "$TARGET/debug/build" "bamdad-tun-helper-"
    if $AGGRESSIVE; then
        [ -d "$TARGET/debug/incremental" ] && echo "$TARGET/debug/incremental"
        search "$TARGET/debug/deps" "lib"
    fi
} | sort -u > "$list"

if [ ! -s "$list" ]; then
    echo "nothing older than ${DAYS} days"
    exit 0
fi

echo "candidates older than ${DAYS} days:"
while IFS= read -r entry; do
    printf '  %s (%s)\n' "${entry#"$TARGET"/}" "$(du -sh "$entry" | cut -f1)"
done < "$list"

if ! $APPLY; then
    echo "dry run; pass --apply to delete"
    exit 0
fi

while IFS= read -r entry; do
    rm -rf -- "$entry"
done < "$list"

after=$(size_kb "$TARGET")
echo "target: $(du -sh "$TARGET" | cut -f1)"
echo "freed: $(( (before - after) / 1024 )) MiB"
