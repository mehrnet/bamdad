#!/usr/bin/env bash
# Exercises the macOS desktop helper without touching the runner's default
# route: it creates a utun, sends a TEST-NET prefix into it, then stops and
# asserts the route and device are gone. Uses sudo (utun creation is
# unprivileged, but ifconfig and route are not).
set -euo pipefail

HELPER=${HELPER:-$PWD/target/debug/bamdad-tun-helper}
WORK=/tmp/bamdad-tun-macos
STATE="$WORK/state"
LOG="$WORK/helper.log"
PIDFILE="$WORK/helper.pid"
TEST_NET=203.0.113.0/24

[ -x "$HELPER" ] || {
    echo "helper not found: $HELPER" >&2
    exit 1
}

rm -rf "$WORK"
mkdir -p "$WORK"

cleanup() {
    [ -f "$PIDFILE" ] && sudo kill -TERM "$(cat "$PIDFILE")" 2>/dev/null || true
    sudo rm -rf "$WORK"
}
trap cleanup EXIT

echo '--- starting the helper ---'
sudo bash -c "'$HELPER' up --address 10.255.0.1/24 --route '$TEST_NET' \
    --state '$STATE' >'$LOG' 2>&1 & echo \$! > '$PIDFILE'"
for _ in $(seq 1 100); do
    grep -q '^ready' "$LOG" 2>/dev/null && break
    sleep 0.1
done
if ! grep -q '^ready' "$LOG"; then
    echo 'helper-did-not-start'
    sudo cat "$LOG" || true
    exit 1
fi
DEVICE=$(awk '/^ready/ { print $2 }' "$LOG")
[ -n "$DEVICE" ] || {
    echo 'helper-did-not-report-a-device'
    exit 1
}
echo "device: $DEVICE"

ifconfig "$DEVICE" | grep -q '10.255.0.1' || {
    echo 'tun-address-missing'
    ifconfig "$DEVICE" || true
    exit 1
}
netstat -rn -f inet | grep -q '203.0.113' || {
    echo 'route-missing'
    netstat -rn -f inet | head -20
    exit 1
}
grep -q "route=$TEST_NET" "$STATE" || {
    echo 'state-file-incomplete'
    exit 1
}
grep -qE '^pid=[0-9]+' "$STATE" || {
    echo 'state-file-has-no-pid'
    exit 1
}
echo 'macos-helper-up-ok'

echo '--- stopping the helper ---'
sudo kill -TERM "$(cat "$PIDFILE")"
for _ in $(seq 1 100); do
    sudo kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
    sleep 0.1
done
if netstat -rn -f inet | grep -q '203.0.113'; then
    echo 'route-not-removed'
    netstat -rn -f inet | head -20
    exit 1
fi
if ifconfig "$DEVICE" >/dev/null 2>&1; then
    echo 'tun-device-still-present'
    exit 1
fi
[ -f "$STATE" ] && {
    echo 'state-file-not-removed'
    exit 1
}
echo 'macos-helper-down-ok'
echo 'macos-tun-helper-ok'
