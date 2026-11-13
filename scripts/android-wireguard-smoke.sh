#!/usr/bin/env bash
# Android smoke test for the native WireGuard/AmneziaWG backend.
# Starts the GoBackend from a seeded .conf and verifies the VpnService TUN.
set -euo pipefail

OUT=/tmp/bamdad-wg-smoke
rm -rf "$OUT"
mkdir -p "$OUT"

APK=$(find /home/runner/work -type f -name 'app-debug.apk' -print -quit)
test -f "$APK"
adb install -r "$APK"

launcher_component() {
    adb shell cmd package resolve-activity --brief \
        -a android.intent.action.MAIN \
        -c android.intent.category.LAUNCHER com.mehrnet.bamdad 2>/dev/null \
        | tr -d '\r' | tail -1
}

launch_app() {
    local component
    component=$(launcher_component)
    test -n "$component"
    adb shell am start -W -n "$component"
}

# Create the data dir first, then seed a native WireGuard profile.
launch_app >/dev/null 2>&1 || true
sleep 3
adb shell am force-stop com.mehrnet.bamdad || true
DATA_DIR=/data/user/0/com.mehrnet.bamdad/files
adb shell run-as com.mehrnet.bamdad mkdir -p "$DATA_DIR"
WG_CLIENT=${BAMDAD_WG_CLIENT_CONF:-/tmp/bamdad-wg/client.conf}
if [ -f "$WG_CLIENT" ]; then
    python3 - "$WG_CLIENT" > "$OUT/profiles.json" <<'PY'
import json, sys
conf = open(sys.argv[1]).read()
store = {
    "settings": {
        "socks_port": 10808,
        "http_port": 10809,
        "system_proxy": False,
        "api_addr": "127.0.0.1",
        "accent": "pink",
        "theme": "dark",
    },
    "profiles": [{
        "id": "wg-test",
        "name": "Test \u00b7 WireGuard",
        "core": "sing-box",
        "transport": "wireguard",
        "protocol": "wireguard",
        "address": "10.0.2.2",
        "port": 51820,
        "raw_config": conf,
        "source": None,
    }],
    "selected": "Test \u00b7 WireGuard",
    "selected_id": "wg-test",
    "default_group_name": "Default",
    "group_order": {},
}
print(json.dumps(store))
PY
else
    cp scripts/android-test-wireguard.json "$OUT/profiles.json"
fi
adb shell "run-as com.mehrnet.bamdad sh -c 'cat > $DATA_DIR/profiles.json'" \
    < "$OUT/profiles.json"
adb shell pm grant com.mehrnet.bamdad android.permission.POST_NOTIFICATIONS || true
adb shell settings put global hide_error_dialogs 1 || true
adb shell am broadcast -a android.intent.action.CLOSE_SYSTEM_DIALOGS || true
adb logcat -c

launch_app
sleep 8
adb exec-out screencap -p > "$OUT/before.png"

SIZE=$(adb shell wm size | tr -d '\r' | awk -F' ' '{print $NF}')
W=$(echo "$SIZE" | cut -dx -f1)
H=$(echo "$SIZE" | cut -dx -f2)
echo "screen ${W}x${H}"

CONSENT=$(find /home/runner/work -type f -name 'android-consent.py' -print -quit)
consented=false
for offset in 225 195 255; do
    adb shell input tap $((W * 86 / 100)) $((H - offset))
    sleep 3
    for _ in 1 2 3 4 5 6; do
        adb shell uiautomator dump /sdcard/window.xml >/dev/null 2>&1 || true
        adb pull /sdcard/window.xml "$OUT/window.xml" >/dev/null 2>&1 || true
        if python3 "$CONSENT" "$OUT/window.xml" | tee "$OUT/consent.txt" | grep -q 'tapping'; then
            consented=true
            break
        fi
        sleep 2
    done
    if $consented; then
        break
    fi
done
adb exec-out screencap -p > "$OUT/connect-tap.png"

sleep 8
adb exec-out screencap -p > "$OUT/after.png"
adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true

echo '--- interfaces ---'
adb shell ip addr show tun0 || true
echo '--- logcat ---'
grep -Ei 'BamdadTunnel|BamdadAmnezia|GoBackend|WireGuard|Amnezia|FATAL EXCEPTION|AndroidRuntime' "$OUT/full-logcat.txt" | tail -80 || true

test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')"
! grep -q 'FATAL EXCEPTION' "$OUT/full-logcat.txt"
if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'native-tun-missing'
    exit 1
fi
echo 'native-tun-up'

# Trigger the profile ping from the UI and verify the prober produced a
# latency for a native transport (ICMP, or the TCP RTT fallback).
echo '--- ping ---'
adb shell uiautomator dump /sdcard/window.xml >/dev/null 2>&1 || true
adb pull /sdcard/window.xml "$OUT/ping-window.xml" >/dev/null 2>&1 || true
python3 - "$OUT/ping-window.xml" <<'PINGPY' || true
import re
import subprocess
import sys

try:
    xml = open(sys.argv[1], encoding="utf-8").read()
except OSError:
    print("no ui dump")
    sys.exit(0)

match = re.search(r'text="Ping"[^>]*bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', xml)
if not match:
    print("no ping button")
    sys.exit(0)

x1, y1, x2, y2 = map(int, match.groups())
subprocess.run(
    ["adb", "shell", "input", "tap", str((x1 + x2) // 2), str((y1 + y2) // 2)],
    check=False,
)
print("tapped ping")
PINGPY
sleep 6
adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
if grep -Eq 'ping Test .*\([^)]*\): [0-9]+ ms' "$OUT/full-logcat.txt"; then
    echo 'native-ping-ok'
else
    echo 'native-ping-missing'
fi

NETCHECK=$(find "${NETCHECK_DIR:-netcheck}" -type f -name 'bamdad-netcheck' -print -quit 2>/dev/null || true)
if [ -n "$NETCHECK" ]; then
    adb push "$NETCHECK" /data/local/tmp/bamdad-netcheck >/dev/null
    adb shell chmod 755 /data/local/tmp/bamdad-netcheck
    if adb shell /data/local/tmp/bamdad-netcheck example.com 80 2>&1 | tee "$OUT/netcheck.txt" | grep -q 'HTTP/1'; then
        echo 'native-traffic-ok'
    else
        echo 'native-traffic-failed'
        exit 1
    fi
else
    echo 'bamdad-netcheck not found; skipping native traffic assertion'
fi

# Disconnect and verify the TUN is gone.
adb shell input tap $((W * 86 / 100)) $((H - 180))
sleep 5
adb exec-out screencap -p > "$OUT/disconnected.png"
adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
if adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'native-tun-still-up'
    exit 1
fi
echo 'native-disconnect-ok'
