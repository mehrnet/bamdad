#!/usr/bin/env bash
# Exercises the privileged desktop TUN helper inside a throwaway network
# namespace: it must create the device, route the default through it, keep the
# endpoint reachable over the physical link, and restore everything on SIGTERM.
set -euo pipefail

NS=bamdad-tun-test
HELPER=${HELPER:-$PWD/target/debug/bamdad-tun-helper}
LOG=/tmp/bamdad-tun-helper.log
STATE=/run/bamdad-tun-test.state
PIDFILE=/tmp/bamdad-tun-helper.pid

[ -x "$HELPER" ] || {
    echo "helper not found: $HELPER (cargo build -p bamdad-tun-helper)" >&2
    exit 1
}

cleanup() {
    sudo ip netns del "$NS" 2>/dev/null || true
    sudo rm -f "$STATE" "$PIDFILE"
}
trap cleanup EXIT

sudo ip netns del "$NS" 2>/dev/null || true
sudo ip netns add "$NS"
sudo ip -n "$NS" link set lo up
sudo ip -n "$NS" link add dummy0 type dummy
sudo ip -n "$NS" addr add 192.0.2.1/24 dev dummy0
sudo ip -n "$NS" link set dummy0 up
sudo ip -n "$NS" route add default via 192.0.2.1 dev dummy0

echo '--- starting the helper ---'
sudo rm -f "$LOG"
sudo bash -c "ip netns exec '$NS' '$HELPER' up --name bamdad0 --address 10.255.0.1/24 \
    --mtu 1400 --bypass 203.0.113.7/32 --state '$STATE' >'$LOG' 2>&1 & echo \$! > '$PIDFILE'"
PID=$(cat "$PIDFILE")
for _ in $(seq 1 50); do
    grep -q '^ready' "$LOG" 2>/dev/null && break
    sleep 0.1
done
if ! grep -q '^ready' "$LOG"; then
    echo 'helper-did-not-start'
    cat "$LOG" || true
    exit 1
fi

ROUTES=$(sudo ip -n "$NS" route)
echo "$ROUTES"
grep -q '^default dev bamdad0' <<<"$ROUTES" || {
    echo 'default-route-not-on-tun'
    exit 1
}
grep -q '203.0.113.7 via 192.0.2.1 dev dummy0' <<<"$ROUTES" || {
    echo 'bypass-route-missing'
    exit 1
}
sudo ip -n "$NS" link show bamdad0 | grep -q 'mtu 1400' || {
    echo 'tun-mtu-wrong'
    exit 1
}
grep -q '203.0.113.7' "$STATE" || {
    echo 'state-file-incomplete'
    exit 1
}
echo 'helper-up-ok'

echo '--- stopping the helper ---'
sudo kill -TERM "$PID"
for _ in $(seq 1 50); do
    sudo kill -0 "$PID" 2>/dev/null || break
    sleep 0.1
done
if sudo kill -0 "$PID" 2>/dev/null; then
    echo 'helper-did-not-exit'
    exit 1
fi

ROUTES=$(sudo ip -n "$NS" route)
echo "$ROUTES"
grep -q 'default via 192.0.2.1 dev dummy0' <<<"$ROUTES" || {
    echo 'default-route-not-restored'
    exit 1
}
if grep -q '203.0.113.7' <<<"$ROUTES"; then
    echo 'bypass-route-not-removed'
    exit 1
fi
if sudo ip -n "$NS" link show bamdad0 >/dev/null 2>&1; then
    echo 'tun-device-still-present'
    exit 1
fi
[ -f "$STATE" ] && {
    echo 'state-file-not-removed'
    exit 1
}
echo 'helper-down-ok'
echo 'tun-helper-ok'
