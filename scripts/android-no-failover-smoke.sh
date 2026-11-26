#!/usr/bin/env bash
# Regression: a local failure must never auto-switch to another config, even
# with "Auto-switch on failure" enabled. Denying the VPN permission is the
# easiest local failure to provoke: it is the device's answer, not the
# profile's, so the selected profile has to stay put.
set -euo pipefail

OUT=/tmp/bamdad-no-failover
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

# Two native profiles in one group, auto-switch explicitly enabled: if the
# local error were treated as a profile failure, the app would select p2 and
# ask for VPN permission again.
python3 - > "$OUT/profiles.json" <<'PY'
import json
wg = """[Interface]
PrivateKey = aGVsbG8=
Address = 10.9.0.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = d29ybGQ=
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = 127.0.0.1:51820
"""
store = {
    "settings": {
        "socks_port": 10808,
        "http_port": 10809,
        "system_proxy": False,
        "api_addr": "127.0.0.1",
        "accent": "cyan",
        "theme": "dark",
        "failover": True,
    },
    "profiles": [
        {
            "id": "p1",
            "name": "First",
            "core": "sing-box",
            "transport": "wireguard",
            "protocol": "wireguard",
            "address": "127.0.0.1",
            "port": 51820,
            "raw_config": wg,
            "source": None,
        },
        {
            "id": "p2",
            "name": "Second",
            "core": "sing-box",
            "transport": "wireguard",
            "protocol": "wireguard",
            "address": "127.0.0.1",
            "port": 51821,
            "raw_config": wg,
            "source": None,
        },
    ],
    "selected": "First",
    "selected_id": "p1",
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

DENY=$(find /home/runner/work -type f -name 'android-consent-deny.py' -print -quit)
test -n "$DENY"
denied=false
for offset in 225 195 255; do
    adb shell input tap $((W * 86 / 100)) $((H - offset))
    sleep 3
    for _ in 1 2 3 4 5 6; do
        adb shell uiautomator dump /sdcard/window.xml >/dev/null 2>&1 || true
        adb pull /sdcard/window.xml "$OUT/window.xml" >/dev/null 2>&1 || true
        if python3 "$DENY" "$OUT/window.xml" | tee "$OUT/deny.txt" | grep -q 'tapping'; then
            denied=true
            break
        fi
        sleep 2
    done
    if $denied; then
        break
    fi
done
if ! $denied; then
    echo 'consent-dialog-not-found'
    exit 1
fi

sleep 10
adb logcat -d -v threadtime > "$OUT/logcat.txt" || true

# A local failure is reported as such and must not start another profile.
if ! grep -q 'failover skipped: this failure is local to the device' "$OUT/logcat.txt"; then
    echo 'local-failure-not-classified'
    grep -Ei 'failover|VPN|BamdadTunnel|FATAL' "$OUT/logcat.txt" | tail -60 || true
    exit 1
fi
if grep -q 'failover: .* trying ' "$OUT/logcat.txt"; then
    echo 'failover-ran-on-local-failure'
    grep -E 'failover' "$OUT/logcat.txt" | tail -10 || true
    exit 1
fi

# The selection is persisted, so it proves the bar did not move either.
adb shell "run-as com.mehrnet.bamdad cat $DATA_DIR/profiles.json" > "$OUT/profiles-after.json" 2>/dev/null || true
python3 - "$OUT/profiles-after.json" <<'PY'
import json, sys
store = json.load(open(sys.argv[1]))
selected = store.get("selected_id")
if selected != "p1":
    print(f"profile-switched: {selected}")
    sys.exit(1)
print(f"selection kept: {selected}")
PY

test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')" || {
    echo 'app-died'
    exit 1
}
if grep -q 'FATAL EXCEPTION' "$OUT/logcat.txt"; then
    echo 'app-crashed'
    exit 1
fi

# Failover would have asked for permission for the second profile; the dialog
# must be gone now.
adb shell uiautomator dump /sdcard/window.xml >/dev/null 2>&1 || true
adb pull /sdcard/window.xml "$OUT/window-after.xml" >/dev/null 2>&1 || true
if grep -q 'text="OK"' "$OUT/window-after.xml" 2>/dev/null &&
    grep -qi 'request\|connection request' "$OUT/window-after.xml" 2>/dev/null; then
    echo 'vpn-dialog-returned (failover prompted again)'
    exit 1
fi

echo '--- logcat ---'
grep -Ei 'failover|BamdadTunnel|FATAL EXCEPTION|AndroidRuntime' "$OUT/logcat.txt" | tail -40 || true
echo 'no-failover-on-local-failure'
