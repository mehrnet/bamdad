#!/usr/bin/env bash
# Start a throwaway WireGuard server for the Android native-tunnel smoke test.
# The emulator reaches the host at 10.0.2.2; the client config is written to
# /tmp/bamdad-wg/client.conf and consumed by android-wireguard-smoke.sh.
set -euo pipefail

sudo apt-get update -qq
sudo apt-get install -y -qq wireguard-tools iptables >/dev/null

out=/tmp/bamdad-wg
rm -rf "$out"
mkdir -p "$out"
chmod 700 "$out"

wg genkey | tee "$out/server.key" | wg pubkey > "$out/server.pub"
wg genkey | tee "$out/client.key" | wg pubkey > "$out/client.pub"
chmod 600 "$out"/*.key

iface=$(ip route get 1.1.1.1 | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") { print $(i + 1); exit }}')
test -n "$iface"

server_ip=10.66.66.1
client_ip=10.66.66.2
sudo mkdir -p /etc/wireguard
sudo tee /etc/wireguard/wg0.conf >/dev/null <<CONF
[Interface]
Address = ${server_ip}/24
ListenPort = 51820
PrivateKey = $(cat "$out/server.key")
PostUp = sysctl -w net.ipv4.ip_forward=1
PostUp = iptables -A FORWARD -i wg0 -j ACCEPT
PostUp = iptables -A FORWARD -o wg0 -m state --state RELATED,ESTABLISHED -j ACCEPT
PostUp = iptables -t nat -A POSTROUTING -o ${iface} -j MASQUERADE
PostDown = iptables -D FORWARD -i wg0 -j ACCEPT || true
PostDown = iptables -D FORWARD -o wg0 -m state --state RELATED,ESTABLISHED -j ACCEPT || true
PostDown = iptables -t nat -D POSTROUTING -o ${iface} -j MASQUERADE || true

[Peer]
PublicKey = $(cat "$out/client.pub")
AllowedIPs = ${client_ip}/32
CONF

sudo wg-quick down wg0 >/dev/null 2>&1 || true
sudo wg-quick up wg0

cat > "$out/client.conf" <<CONF
[Interface]
PrivateKey = $(cat "$out/client.key")
Address = ${client_ip}/32
DNS = 1.1.1.1

[Peer]
PublicKey = $(cat "$out/server.pub")
AllowedIPs = 0.0.0.0/0
Endpoint = 10.0.2.2:51820
PersistentKeepalive = 25
CONF
chmod 600 "$out/client.conf"

echo 'WireGuard test server is up:'
sudo wg show wg0
echo "client config: $out/client.conf"
