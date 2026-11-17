#!/usr/bin/env bash
# Exercises the Windows helper: the sidecar creates a Wintun adapter, the helper
# configures it and routes a TEST-NET prefix, and `down --state` undoes it all.
# Runs in Git Bash on a Windows runner (administrator), so nothing here touches
# the default route.
set -euo pipefail

HELPER=${HELPER:-$PWD/target/debug/bamdad-tun-helper.exe}
TUN2SOCKS=${BAMDAD_TUN2SOCKS:-$PWD/target/debug/bamdad-tun2socks.exe}
WORK=${TEMP:-/tmp}/bamdad-tun-win
STATE="$WORK/state.txt"
LOG="$WORK/helper.log"
IFACE=bamdad0
TEST_NET=203.0.113.0/24

[ -x "$HELPER" ] || {
    echo "helper not found: $HELPER" >&2
    exit 1
}
[ -x "$TUN2SOCKS" ] || {
    echo "tun2socks not found: $TUN2SOCKS" >&2
    exit 1
}

rm -rf "$WORK"
mkdir -p "$WORK"

cleanup() {
    [ -f "$STATE" ] && "$HELPER" down --state "$STATE" >/dev/null 2>&1 || true
    netsh interface ipv4 delete route "$TEST_NET" "$IFACE" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo '--- starting the helper with tun2socks ---'
"$HELPER" up --name "$IFACE" --address 10.255.0.1/24 --mtu 1400 \
    --route "$TEST_NET" --dns 1.1.1.1 --state "$STATE" \
    --exec "$TUN2SOCKS" -device "tun://$IFACE" -loglevel warn \
    >"$LOG" 2>&1 &
HELPER_PID=$!
for _ in $(seq 1 200); do
    grep -q '^ready' "$LOG" 2>/dev/null && break
    sleep 0.1
done
if ! grep -q '^ready' "$LOG"; then
    echo 'helper-did-not-start'
    cat "$LOG" || true
    exit 1
fi

netsh interface ipv4 show addresses "$IFACE" | grep -q '10.255.0.1' || {
    echo 'adapter-address-missing'
    netsh interface ipv4 show addresses "$IFACE" || true
    exit 1
}
netsh interface ipv4 show route | grep -q '203.0.113.0/24' || {
    echo 'route-missing'
    netsh interface ipv4 show route | head -30 || true
    exit 1
}
grep -q 'route=203.0.113.0/24' "$STATE" || {
    echo 'state-file-incomplete'
    cat "$STATE" || true
    exit 1
}
echo 'windows-helper-up-ok'

echo '--- stopping via down --state ---'
"$HELPER" down --state "$STATE"

for _ in $(seq 1 100); do
    kill -0 "$HELPER_PID" 2>/dev/null || break
    sleep 0.1
done
if netsh interface ipv4 show route | grep -q '203.0.113.0/24'; then
    echo 'route-not-removed'
    netsh interface ipv4 show route | head -30 || true
    exit 1
fi
if netsh interface ipv4 show interfaces "$IFACE" >/dev/null 2>&1; then
    echo 'adapter-still-present'
    exit 1
fi
[ -f "$STATE" ] && {
    echo 'state-file-not-removed'
    exit 1
}
echo 'windows-helper-down-ok'
echo 'windows-tun-helper-ok'
