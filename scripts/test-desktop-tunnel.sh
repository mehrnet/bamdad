#!/usr/bin/env bash
# End-to-end check for the desktop tunnel: a network namespace gets a fake
# "internet" on a veth pair, a real SOCKS core runs inside it, and the helper
# brings up the tun with tun2socks feeding that core. curl then has to reach a
# host HTTP server through the tunnel, and stopping the helper must restore the
# original routing.
#
# Requires root (sudo), Linux, and:
#   - target/debug/bamdad-tun-helper
#   - a tun2socks binary (BAMDAD_TUN2SOCKS, default target/debug/bamdad-tun2socks)
#   - a sing-box binary (SINGBOX, default from PATH or target/debug/sing-box)
set -euo pipefail

NS=bamdad-tun-e2e
HOST_IP=10.200.0.1
NS_IP=10.200.0.2
PORT=18080
SOCKS_PORT=11080
WORK=/tmp/bamdad-desktop-tunnel
HELPER=${HELPER:-$PWD/target/debug/bamdad-tun-helper}
TUN2SOCKS=${BAMDAD_TUN2SOCKS:-$PWD/target/debug/bamdad-tun2socks}
SINGBOX=${SINGBOX:-$(command -v sing-box || echo "$PWD/target/debug/sing-box")}
MARKER=bamdad-tunnel-marker

for binary in "$HELPER" "$TUN2SOCKS" "$SINGBOX"; do
    [ -x "$binary" ] || {
        echo "missing binary: $binary" >&2
        exit 1
    }
done

cleanup() {
    sudo ip netns pids "$NS" 2>/dev/null | xargs -r sudo kill 2>/dev/null || true
    sudo ip netns del "$NS" 2>/dev/null || true
    [ -n "${HTTP_PID:-}" ] && kill "$HTTP_PID" 2>/dev/null || true
    sudo rm -rf "$WORK"
}
trap cleanup EXIT

rm -rf "$WORK"
mkdir -p "$WORK"
echo "$MARKER" > "$WORK/index.html"

sudo ip netns del "$NS" 2>/dev/null || true
sudo ip netns add "$NS"

echo '--- wiring the fake internet ---'
sudo ip link add veth-host type veth peer name veth-ns
sudo ip link set veth-ns netns "$NS"
sudo ip addr add "$HOST_IP/24" dev veth-host
sudo ip link set veth-host up
sudo ip -n "$NS" link set lo up
sudo ip -n "$NS" addr add "$NS_IP/24" dev veth-ns
sudo ip -n "$NS" link set veth-ns up
sudo ip -n "$NS" route add default via "$HOST_IP" dev veth-ns

(cd "$WORK" && python3 -m http.server "$PORT" --bind "$HOST_IP" >"$WORK/http.log" 2>&1) &
HTTP_PID=$!
sleep 1
curl --noproxy "*" -fsS "http://$HOST_IP:$PORT/index.html" | grep -q "$MARKER" || {
    echo 'http-server-not-reachable'
    exit 1
}

echo '--- starting the SOCKS core inside the namespace ---'
cat > "$WORK/singbox.json" <<EOF
{
  "log": { "level": "warn" },
  "inbounds": [
    { "type": "socks", "listen": "127.0.0.1", "listen_port": $SOCKS_PORT }
  ],
  "outbounds": [ { "type": "direct" } ]
}
EOF
sudo bash -c "ip netns exec '$NS' '$SINGBOX' run -c '$WORK/singbox.json' >'$WORK/singbox.log' 2>&1 & echo \$! > '$WORK/singbox.pid'"
sleep 2
sudo kill -0 "$(cat "$WORK/singbox.pid")" 2>/dev/null || {
    echo 'socks-core-did-not-start'
    sudo cat "$WORK/singbox.log" || true
    exit 1
}

echo '--- starting the helper with tun2socks ---'
printf 'nameserver 10.200.0.1\n' > "$WORK/resolv.conf"
chmod 644 "$WORK/resolv.conf"
sudo bash -c "ip netns exec '$NS' '$HELPER' up --name bamdad0 --address 10.255.0.1/24 \
    --mtu 1400 --state '$WORK/tun.state' --dns 1.1.1.1 --dns-method file \
    --resolv-conf '$WORK/resolv.conf' --exec '$TUN2SOCKS' \
    -device fd://3 -proxy socks5://127.0.0.1:$SOCKS_PORT -loglevel warn \
    >'$WORK/helper.log' 2>&1 & echo \$! > '$WORK/helper.pid'"
for _ in $(seq 1 100); do
    grep -q '^ready' "$WORK/helper.log" 2>/dev/null && break
    sleep 0.1
done
grep -q '^ready' "$WORK/helper.log" || {
    echo 'helper-did-not-start'
    sudo cat "$WORK/helper.log" || true
    exit 1
}

echo '--- DNS points at the tunnel ---'
grep -q 'nameserver 1.1.1.1' "$WORK/resolv.conf" || {
    echo 'resolv-conf-not-updated'
    cat "$WORK/resolv.conf" || true
    exit 1
}
[ -f "$WORK/resolv.conf.bamdad-backup" ] || {
    echo 'resolv-conf-backup-missing'
    exit 1
}
echo 'desktop-tunnel-dns-ok'

echo '--- traffic through the tunnel ---'
ok=false
for _ in $(seq 1 20); do
    if sudo ip netns exec "$NS" curl --noproxy "*" -fsS --max-time 5 \
        "http://$HOST_IP:$PORT/index.html" >"$WORK/curl.txt" 2>"$WORK/curl.err"; then
        ok=true
        break
    fi
    sleep 1
done
if ! $ok; then
    echo 'tunnel-traffic-failed'
    sudo cat "$WORK/curl.err" || true
    sudo cat "$WORK/helper.log" || true
    sudo ip -n "$NS" route || true
    exit 1
fi
grep -q "$MARKER" "$WORK/curl.txt" || {
    echo 'tunnel-returned-wrong-content'
    exit 1
}
echo 'desktop-tunnel-traffic-ok'

echo '--- stopping restores routing ---'
sudo kill -TERM "$(cat "$WORK/helper.pid")"
for _ in $(seq 1 50); do
    sudo kill -0 "$(cat "$WORK/helper.pid")" 2>/dev/null || break
    sleep 0.1
done
ROUTES=$(sudo ip -n "$NS" route)
echo "$ROUTES"
grep -q "default via $HOST_IP dev veth-ns" <<<"$ROUTES" || {
    echo 'default-route-not-restored'
    exit 1
}
sudo ip -n "$NS" link show bamdad0 >/dev/null 2>&1 && {
    echo 'tun-device-still-present'
    exit 1
}
grep -q 'nameserver 10.200.0.1' "$WORK/resolv.conf" || {
    echo 'resolv-conf-not-restored'
    cat "$WORK/resolv.conf" || true
    exit 1
}
[ -f "$WORK/resolv.conf.bamdad-backup" ] && {
    echo 'resolv-conf-backup-left-behind'
    exit 1
}
echo 'desktop-tunnel-dns-restored' 
echo 'desktop-tunnel-restored'
echo 'desktop-tunnel-ok'
