#!/usr/bin/env bash
# Start an OpenVPN server on the CI host for the Android OpenVPN smoke test.
# The emulator reaches it at 10.0.2.2:1194 (host loopback).
set -euo pipefail

WORK=${BAMDAD_OVPN_DIR:-/tmp/bamdad-ovpn}
PORT=${BAMDAD_OVPN_PORT:-1194}

sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openvpn easy-rsa >/dev/null

# Make sure a tun device exists on the runner.
if [[ ! -c /dev/net/tun ]]; then
  sudo modprobe tun || true
  sudo mkdir -p /dev/net
  sudo mknod /dev/net/tun c 10 200 || true
  sudo chmod 666 /dev/net/tun
fi

rm -rf "$WORK"
mkdir -p "$WORK"
echo "Creating easy-rsa PKI in $WORK"
make-cadir "$WORK/easy-rsa"
cd "$WORK/easy-rsa"

./easyrsa --batch --req-cn=bamdad-test build-ca nopass
./easyrsa --batch --req-cn=server gen-req server nopass
./easyrsa --batch sign-req server server
./easyrsa --batch --req-cn=client gen-req client nopass
./easyrsa --batch sign-req client client

if ! openvpn --genkey secret "$WORK/ta.key" 2>/dev/null; then
  openvpn --genkey --secret "$WORK/ta.key"
fi

PKI="$WORK/easy-rsa/pki"
cat > "$WORK/server.conf" <<EOF
port $PORT
proto udp
dev tun
topology subnet
ca $PKI/ca.crt
cert $PKI/issued/server.crt
key $PKI/private/server.key
dh none
ecdh-curve prime256v1
tls-crypt $WORK/ta.key
server 10.8.0.0 255.255.255.0
push "redirect-gateway def1 bypass-dhcp"
push "dhcp-option DNS 1.1.1.1"
keepalive 10 60
persist-key
persist-tun
verb 3
EOF

{
  cat <<EOF
client
dev tun
proto udp
remote 10.0.2.2 $PORT
resolv-retry infinite
nobind
persist-key
persist-tun
remote-cert-tls server
verb 3
data-ciphers AES-256-GCM:AES-128-GCM
<ca>
EOF
  cat "$PKI/ca.crt"
  echo '</ca>'
  echo '<cert>'
  cat "$PKI/issued/client.crt"
  echo '</cert>'
  echo '<key>'
  cat "$PKI/private/client.key"
  echo '</key>'
  echo '<tls-crypt>'
  cat "$WORK/ta.key"
  echo '</tls-crypt>'
} > "$WORK/client.ovpn"

sudo openvpn --config "$WORK/server.conf" --daemon --log "$WORK/server.log" \
  --writepid "$WORK/server.pid"

for _ in $(seq 1 30); do
  if sudo grep -q 'Initialization Sequence Completed' "$WORK/server.log" 2>/dev/null; then
    echo "OpenVPN test server ready on udp/$PORT"
    exit 0
  fi
  sleep 1
done

echo "OpenVPN server failed to start" >&2
sudo cat "$WORK/server.log" >&2 || true
exit 1
