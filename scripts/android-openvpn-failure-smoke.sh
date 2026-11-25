#!/usr/bin/env bash
# Regression for the unreachable-server case: the OpenVPN core retries a
# refused transport forever. The app must report a failure, stop the core, and
# stay responsive instead of leaving an unstoppable "Connecting…" bar.
set -euo pipefail

OUT=/tmp/bamdad-ovpn-fail
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
CLIENT=${BAMDAD_OVPN_CLIENT:-/tmp/bamdad-ovpn/client.ovpn}
test -f "$CLIENT"

# Same profile as the happy-path smoke, pointed at a closed local port and
# forced onto TCP so the failure is the same immediate "connection refused"
# loop a dead server produces.
python3 - "$CLIENT" > "$OUT/profiles.json" <<'PY'
import json, re, sys
config = open(sys.argv[1]).read()
config = re.sub(r'(?m)^remote\s+\S+\s+\S+.*$', 'remote 127.0.0.1 9', config)
config = re.sub(r'(?m)^proto\s+\S+.*$', 'proto tcp', config)
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
        "id": "ovpn-fail",
        "name": "Test \u00b7 OpenVPN down",
        "core": "xray",
        "transport": "openvpn",
        "protocol": "openvpn",
        "address": "127.0.0.1",
        "port": 9,
        "raw_config": config,
        "auth_user": "bamdad",
        "auth_pass": "ovpn-secret",
        "source": None,
    }],
    "selected": "Test \u00b7 OpenVPN down",
    "selected_id": "ovpn-fail",
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

# The refused transport retries every ~2 s; the app reports the failure after a
# few of them and must stop the core there.
reported=false
for _ in $(seq 1 15); do
    sleep 2
    adb logcat -d -v threadtime > "$OUT/logcat.txt" || true
    if grep -q 'BamdadTunnel: error: OpenVPN could not connect' "$OUT/logcat.txt"; then
        reported=true
        break
    fi
done
if ! $reported; then
    echo 'openvpn-failure-not-reported'
    grep -Ei 'BamdadOpenVpn|BamdadTunnel|FATAL' "$OUT/logcat.txt" | tail -60 || true
    exit 1
fi

before=$(grep -c 'restarting in' "$OUT/logcat.txt" || true)
if [ "$before" -eq 0 ]; then
    echo 'openvpn-never-retried (unexpected, but the error was reported)'
fi

# Wait long enough for several more retries, then prove the loop is gone.
sleep 10
adb logcat -d -v threadtime > "$OUT/logcat-after.txt" || true
after=$(grep -c 'restarting in' "$OUT/logcat-after.txt" || true)
echo "restart lines: $before -> $after"
if [ "$after" -gt $((before + 1)) ]; then
    echo 'openvpn-retry-loop-not-stopped'
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

echo '--- logcat ---'
grep -Ei 'BamdadOpenVpn|BamdadTunnel|FATAL EXCEPTION|AndroidRuntime' "$OUT/logcat-after.txt" | tail -60 || true
echo 'openvpn-failure-stopped'
