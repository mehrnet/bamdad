#!/usr/bin/env bash
# Build and start a userspace AmneziaWG server for the Android smoke test.
# The emulator reaches it at 10.0.2.2:$PORT; the client config is written to
# /tmp/bamdad-awg/client.conf.
set -euo pipefail

WORK=${BAMDAD_AWG_DIR:-/tmp/bamdad-awg}
PORT=${BAMDAD_AWG_PORT:-51821}
GO_TAG=v3.1.20260828
TOOLS_TAG=v3.1.20260812

# CI runs on Ubuntu; other hosts are expected to have the toolchain already.
if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
      build-essential libmnl-dev git >/dev/null
fi

rm -rf "$WORK"
mkdir -p "$WORK/bin" "$WORK/src"

git clone -q --depth 1 --branch "$GO_TAG" \
  https://github.com/amnezia-vpn/amneziawg-go "$WORK/src/go"
make -C "$WORK/src/go" >/dev/null
cp "$WORK/src/go/amneziawg-go" "$WORK/bin/"

git clone -q --depth 1 --branch "$TOOLS_TAG" \
  https://github.com/amnezia-vpn/amneziawg-tools "$WORK/src/tools"
make -C "$WORK/src/tools/src" >/dev/null
cp "$WORK/src/tools/src/wg" "$WORK/bin/awg"
cp "$WORK/src/tools/src/wg-quick/linux.bash" "$WORK/bin/awg-quick"
chmod +x "$WORK/bin/"*
export PATH="$WORK/bin:$PATH"

awg genkey | tee "$WORK/server.key" | awg pubkey > "$WORK/server.pub"
awg genkey | tee "$WORK/client.key" | awg pubkey > "$WORK/client.pub"
chmod 600 "$WORK"/*.key

iface=$(ip route get 1.1.1.1 | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") { print $(i + 1); exit }}')
test -n "$iface"

# Shared obfuscation parameters; server and client must match exactly.
params() {
  cat <<'PARAMS'
Jc = 4
Jmin = 50
Jmax = 1000
S1 = 20
S2 = 30
S3 = 40
S4 = 50
H1 = 1234567
H2 = 2345678
H3 = 3456789
H4 = 4567890
PARAMS
}

{
  echo "[Interface]"
  echo "Address = 10.77.77.1/24"
  echo "ListenPort = $PORT"
  echo "PrivateKey = $(cat "$WORK/server.key")"
  params
  echo "PostUp = sysctl -w net.ipv4.ip_forward=1"
  echo "PostUp = iptables -A FORWARD -i %i -j ACCEPT"
  echo "PostUp = iptables -A FORWARD -o %i -m state --state RELATED,ESTABLISHED -j ACCEPT"
  echo "PostUp = iptables -t nat -A POSTROUTING -o $iface -j MASQUERADE"
  echo "PostDown = iptables -D FORWARD -i %i -j ACCEPT || true"
  echo "PostDown = iptables -D FORWARD -o %i -m state --state RELATED,ESTABLISHED -j ACCEPT || true"
  echo "PostDown = iptables -t nat -D POSTROUTING -o $iface -j MASQUERADE || true"
  echo
  echo "[Peer]"
  echo "PublicKey = $(cat "$WORK/client.pub")"
  echo "AllowedIPs = 10.77.77.2/32"
} > "$WORK/awg0.conf"
chmod 600 "$WORK/awg0.conf"

{
  echo "[Interface]"
  echo "Address = 10.77.77.2/32"
  echo "PrivateKey = $(cat "$WORK/client.key")"
  echo "DNS = 1.1.1.1"
  echo "MTU = 1280"
  params
  echo
  echo "[Peer]"
  echo "PublicKey = $(cat "$WORK/server.pub")"
  echo "AllowedIPs = 0.0.0.0/0"
  echo "Endpoint = 10.0.2.2:$PORT"
  echo "PersistentKeepalive = 11"
} > "$WORK/client.conf"
chmod 600 "$WORK/client.conf"

sudo env PATH="$WORK/bin:$PATH" WG_QUICK_USERSPACE_IMPLEMENTATION=amneziawg-go \
  awg-quick up "$WORK/awg0.conf"

echo "AmneziaWG test server is up on udp/$PORT"
echo "client config: $WORK/client.conf"
sudo env PATH="$WORK/bin:$PATH" awg show awg0 | head -20
