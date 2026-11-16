#!/usr/bin/env bash
# Start a strongSwan IKEv2 responder for the Android IPSec smoke and write a
# matching client profile (with the test CA, client certificate, and key
# embedded) to /tmp/bamdad-ipsec/profile.json.
set -euo pipefail

WORK=${BAMDAD_IPSEC_DIR:-/tmp/bamdad-ipsec}
rm -rf "$WORK"
mkdir -p "$WORK"

if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
        strongswan strongswan-swanctl strongswan-pki openssl >/dev/null
fi

SWANCTL_CONF=/etc/swanctl/conf.d/bamdad-smoke.conf
SWANCTL_DIR=/etc/swanctl

echo "=== generating a test CA and certificates ==="
cd "$WORK"
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=Bamdad Test CA" \
    -keyout ca.key -out ca.pem >/dev/null 2>&1

openssl req -newkey rsa:2048 -nodes -subj "/CN=10.0.2.2" \
    -keyout server.key -out server.csr >/dev/null 2>&1
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
    -days 2 -out server.pem -extfile <(printf "subjectAltName=IP:10.0.2.2") >/dev/null 2>&1

openssl req -newkey rsa:2048 -nodes -subj "/CN=bamdad" \
    -keyout client.key -out client.csr >/dev/null 2>&1
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
    -days 2 -out client.pem >/dev/null 2>&1

iface=$(ip route get 1.1.1.1 | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") { print $(i + 1); exit }}')
test -n "$iface"

echo "=== installing swanctl configuration ==="
sudo mkdir -p "$SWANCTL_DIR/conf.d" "$SWANCTL_DIR/x509" "$SWANCTL_DIR/x509ca" \
    "$SWANCTL_DIR/private"
sudo cp server.pem "$SWANCTL_DIR/x509/"
sudo cp ca.pem "$SWANCTL_DIR/x509ca/"
sudo install -m 600 server.key "$SWANCTL_DIR/private/server.key"
sudo cp client.pem "$SWANCTL_DIR/x509/"

sudo tee "$SWANCTL_CONF" >/dev/null <<EOF
connections {
    bamdad {
        version = 2
        proposals = aes256-sha256-modp2048
        local {
            auth = pubkey
            certs = server.pem
            id = 10.0.2.2
        }
        remote {
            auth = pubkey
            cacerts = ca.pem
        }
        children {
            bamdad {
                local_ts = 0.0.0.0/0
                remote_ts = 0.0.0.0/0
                dpd_action = clear
                start_action = none
            }
        }
        pools = bamdad_pool
    }
}
pools {
    bamdad_pool {
        addrs = 10.99.0.0/24
    }
}
secrets {
    private {
        bamdad {
            file = server.key
        }
    }
}
EOF

echo "=== starting strongSwan ==="
swanctl --version 2>&1 | tee "$WORK/version.txt" || true
sudo systemctl restart strongswan 2>&1 | tee "$WORK/systemctl.txt" || true
for _ in $(seq 1 20); do
    sudo swanctl --stats >/dev/null 2>&1 && break
    sleep 1
done
if ! sudo swanctl --stats >/dev/null 2>&1; then
    echo "charon/vici is not reachable; diagnostics follow"
    sudo systemctl status strongswan --no-pager -l 2>&1 | tail -40 || true
    sudo journalctl -u strongswan -n 80 --no-pager 2>&1 | tail -80 || true
    sudo ls -l /usr/lib/ipsec/plugins 2>&1 | head -20 || true
    exit 1
fi
sudo swanctl --load-all 2>&1 | tee "$WORK/swanctl-load.txt"
sudo swanctl --list-conns | tee "$WORK/swanctl-conns.txt"

echo "=== forwarding and NAT for the tunnel pool ==="
sudo sysctl -q -w net.ipv4.ip_forward=1
sudo iptables -A FORWARD -i eth0 -o eth0 -j ACCEPT 2>/dev/null || true
sudo iptables -t nat -A POSTROUTING -s 10.99.0.0/24 -o "$iface" -j MASQUERADE
sudo iptables -A FORWARD -s 10.99.0.0/24 -j ACCEPT
sudo iptables -A FORWARD -d 10.99.0.0/24 -j ACCEPT

echo "=== writing the client profile ==="
python3 - "$WORK" > "$WORK/profile.json" <<'PY'
import json
import pathlib
import sys

work = pathlib.Path(sys.argv[1])
ca = (work / "ca.pem").read_text().strip()
cert = (work / "client.pem").read_text().strip()
key = (work / "client.key").read_text().strip()

xml = f"""<?xml version="1.0" encoding="utf-8"?>
<vpnprofile>
    <name>Test IPSec</name>
    <type>ikev2-cert</type>
    <server>10.0.2.2</server>
    <port>500</port>
    <remote_id>10.0.2.2</remote_id>
    <mtu>1400</mtu>
    <ca>
{ca}
    </ca>
    <cert>
{cert}
    </cert>
    <key>
{key}
    </key>
</vpnprofile>
"""

store = {
    "settings": {
        "socks_port": 10808,
        "http_port": 10809,
        "system_proxy": False,
        "api_addr": "127.0.0.1",
        "accent": "pink",
        "theme": "dark",
    },
    "profiles": [
        {
            "id": "ipsec-test",
            "name": "Test \u00b7 IPSec",
            "core": "sing-box",
            "transport": "ipsec",
            "protocol": "ipsec",
            "address": "10.0.2.2",
            "port": 500,
            "config": None,
            "raw_config": xml,
            "source": None,
        }
    ],
    "selected": "Test \u00b7 IPSec",
    "selected_id": "ipsec-test",
    "default_group_name": "Default",
    "group_order": {},
}
print(json.dumps(store))
PY

echo "IPSec test server ready (profile: $WORK/profile.json)"
