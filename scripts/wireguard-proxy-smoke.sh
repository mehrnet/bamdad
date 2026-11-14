#!/usr/bin/env bash
# Host smoke test for the userspace WireGuard proxy helper.
# Requires scripts/wireguard-test-server.sh to have run first (it writes the
# client config to /tmp/bamdad-wg/client.conf).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT=/tmp/bamdad-wg-proxy
rm -rf "$OUT"
mkdir -p "$OUT"

BIN="$OUT/bamdad-wg"
"$ROOT/scripts/build-wireguard-proxy.sh" --host "$BIN"

if [[ ! -f /tmp/bamdad-wg/client.conf ]]; then
  echo "missing /tmp/bamdad-wg/client.conf; run scripts/wireguard-test-server.sh" >&2
  exit 1
fi

# The emulator reaches the host at 10.0.2.2; the host reaches the server on
# loopback.
sed 's/Endpoint = 10.0.2.2:51820/Endpoint = 127.0.0.1:51820/' \
  /tmp/bamdad-wg/client.conf > "$OUT/proxy.conf"
cat >> "$OUT/proxy.conf" <<'EOF'

[Socks5]
BindAddress = 127.0.0.1:25344
EOF
chmod 600 "$OUT/proxy.conf"

"$BIN" -c "$OUT/proxy.conf" -s >"$OUT/proxy.log" 2>&1 &
PROXY_PID=$!
trap 'kill "$PROXY_PID" 2>/dev/null || true' EXIT

ready=false
for _ in $(seq 1 60); do
  if timeout 1 bash -c 'exec 3<>/dev/tcp/127.0.0.1/25344' 2>/dev/null; then
    ready=true
    break
  fi
  if ! kill -0 "$PROXY_PID" 2>/dev/null; then
    break
  fi
  sleep 0.25
done

if ! $ready; then
  echo "wireproxy SOCKS listener never opened" >&2
  cat "$OUT/proxy.log" >&2 || true
  exit 1
fi

code=$(timeout 25 curl -s -o /dev/null -w '%{http_code}' \
  --socks5-hostname 127.0.0.1:25344 https://www.gstatic.com/generate_204 || true)
echo "wireguard userspace proxy HTTP status: ${code:-none}"
if [[ "$code" != "204" && "$code" != "200" ]]; then
  echo "wireguard-proxy-failed" >&2
  cat "$OUT/proxy.log" >&2 || true
  exit 1
fi
echo 'wireguard-proxy-ok'
