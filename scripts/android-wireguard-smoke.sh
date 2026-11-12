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
adb shell "run-as com.mehrnet.bamdad sh -c 'cat > $DATA_DIR/profiles.json'" \
    < scripts/android-test-wireguard.json
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
