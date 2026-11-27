#!/usr/bin/env bash
# Regression: a live OpenVPN TCP transport that drops must be re-established
# by the core, not turned into a terminal error by the app.
set -euo pipefail

OUT=/tmp/bamdad-ovpn-reconnect
WORK=/tmp/bamdad-ovpn
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

launch_app >/dev/null 2>&1 || true
sleep 3
adb shell am force-stop com.mehrnet.bamdad || true
DATA_DIR=/data/user/0/com.mehrnet.bamdad/files
adb shell run-as com.mehrnet.bamdad mkdir -p "$DATA_DIR"
CLIENT=${BAMDAD_OVPN_TCP_CLIENT:-$WORK/client-tcp.ovpn}
test -f "$CLIENT"

python3 - "$CLIENT" > "$OUT/profiles.json" <<'PY'
import json, sys
config = open(sys.argv[1]).read()
store = {
    "settings": {
        "socks_port": 10808,
        "http_port": 10809,
        "system_proxy": False,
        "api_addr": "127.0.0.1",
        "accent": "cyan",
        "theme": "dark",
    },
    "profiles": [{
        "id": "ovpn-reconnect",
        "name": "Test \u00b7 OpenVPN reconnect",
        "core": "xray",
        "transport": "openvpn",
        "protocol": "openvpn",
        "address": "10.0.2.2",
        "port": 443,
        "raw_config": config,
        "auth_user": "bamdad",
        "auth_pass": "ovpn-secret",
        "source": None,
    }],
    "selected": "Test \u00b7 OpenVPN reconnect",
    "selected_id": "ovpn-reconnect",
    "default_group_name": "Default",
    "group_order": {},
}
print(json.dumps(store))
PY
adb shell "run-as com.mehrnet.bamdad sh -c 'cat > $DATA_DIR/profiles.json'" \
    < "$OUT/profiles.json"
adb shell pm grant com.mehrnet.bamdad android.permission.POST_NOTIFICATIONS || true
adb shell settings put global hide_error_dialogs 1 || true
adb shell am broadcast -a android.intent.action.CLOSE_SYSTEM_DIALOGS || true
adb logcat -c

launch_app
sleep 8

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

connected_count() {
    adb logcat -d -v threadtime 2>/dev/null \
        | grep -c 'BamdadOpenVpn: event CONNECTED' || true
}

first=false
for _ in $(seq 1 20); do
    sleep 2
    if [ "$(connected_count)" -ge 1 ]; then
        first=true
        break
    fi
done
if ! $first; then
    echo 'openvpn-never-connected'
    adb logcat -d -v threadtime | grep -Ei 'BamdadOpenVpn|BamdadTunnel|FATAL' | tail -80 || true
    exit 1
fi
echo 'openvpn-connected'

# Drop the live TCP transport: the core must enter its own restart cycle.
sudo pkill -f 'server-tcp.conf' || true
sleep 6
adb logcat -d -v threadtime > "$OUT/logcat-drop.txt" || true
if grep -q 'BamdadTunnel: error: Transport error' "$OUT/logcat-drop.txt"; then
    echo 'transport-drop-reported-as-terminal-error'
    exit 1
fi
if ! grep -q 'event TRANSPORT_ERROR' "$OUT/logcat-drop.txt"; then
    echo 'transport-drop-not-seen'
    exit 1
fi

# Bring the listener back; the client retries every few seconds.
sudo truncate -s 0 "$WORK/server-tcp.log" 2>/dev/null || true
sudo openvpn --config "$WORK/server-tcp.conf" --daemon \
    --log "$WORK/server-tcp.log" --writepid "$WORK/server-tcp.pid"
for _ in $(seq 1 20); do
    if sudo grep -q 'Initialization Sequence Completed' "$WORK/server-tcp.log" 2>/dev/null; then
        break
    fi
    sleep 1
done

reconnected=false
for _ in $(seq 1 25); do
    sleep 2
    if [ "$(connected_count)" -ge 2 ]; then
        reconnected=true
        break
    fi
done
adb logcat -d -v threadtime > "$OUT/logcat-after.txt" || true
if ! $reconnected; then
    echo 'openvpn-did-not-reconnect'
    grep -Ei 'BamdadOpenVpn|BamdadTunnel|FATAL' "$OUT/logcat-after.txt" | tail -100 || true
    exit 1
fi

test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')" || {
    echo 'app-died'
    exit 1
}
if grep -q 'FATAL EXCEPTION' "$OUT/logcat-after.txt"; then
    echo 'app-crashed'
    exit 1
fi
if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'openvpn-tun-missing-after-reconnect'
    exit 1
fi

# Repeated stop/start cycles must not leave native state behind: the old
# core's callbacks used to reach the next service and corrupt its transport.
cycles=0
for cycle in 1 2 3; do
    before=$(connected_count)
    adb shell input tap $((W * 86 / 100)) $((H - 225))
    sleep 4
    adb shell input tap $((W * 86 / 100)) $((H - 225))
    cycle_ok=false
    for _ in $(seq 1 20); do
        sleep 2
        if [ "$(connected_count)" -gt "$before" ]; then
            cycle_ok=true
            break
        fi
    done
    if ! $cycle_ok; then
        echo "restart-cycle-$cycle-failed"
        adb logcat -d -v threadtime | grep -Ei 'BamdadOpenVpn|BamdadTunnel|FATAL' | tail -80 || true
        exit 1
    fi
    cycles=$((cycles + 1))
done
echo "restart-cycles-ok ($cycles)"

adb logcat -d -v threadtime > "$OUT/logcat-final.txt" || true
if grep -qE 'TCP_SIZE_ERROR|TLS Keying material export error' "$OUT/logcat-final.txt"; then
    echo 'tls-transport-corruption'
    grep -E 'TCP_SIZE_ERROR|Keying material|TRANSPORT_ERROR' "$OUT/logcat-final.txt" | tail -20 || true
    exit 1
fi
if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'openvpn-tun-missing-after-cycles'
    exit 1
fi

echo '--- logcat ---'
grep -Ei 'BamdadOpenVpn|BamdadTunnel|FATAL' "$OUT/logcat-final.txt" | tail -60 || true
echo 'openvpn-reconnect-ok'
