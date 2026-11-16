#!/usr/bin/env bash
# Android smoke test for the IPSec/IKEv2 backend: seed a certificate profile,
# connect to the strongSwan responder from scripts/ipsec-test-server.sh, verify
# the CHILD_SA on both ends, pass traffic, and disconnect.
set -euo pipefail

OUT=/tmp/bamdad-ipsec-smoke
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

# Create the data dir first, then seed the IPSec profile.
launch_app >/dev/null 2>&1 || true
sleep 3
adb shell am force-stop com.mehrnet.bamdad || true
DATA_DIR=/data/user/0/com.mehrnet.bamdad/files
adb shell run-as com.mehrnet.bamdad mkdir -p "$DATA_DIR"
PROFILE=${BAMDAD_IPSEC_PROFILE:-/tmp/bamdad-ipsec/profile.json}
test -f "$PROFILE"
cp "$PROFILE" "$OUT/profiles.json"
adb shell "run-as com.mehrnet.bamdad sh -c 'cat > $DATA_DIR/profiles.json'" < "$OUT/profiles.json"
adb shell pm grant com.mehrnet.bamdad android.permission.POST_NOTIFICATIONS || true
adb shell settings put global hide_error_dialogs 1 || true
adb shell am broadcast -a android.intent.action.CLOSE_SYSTEM_DIALOGS || true
adb logcat -c

SIZE=$(adb shell wm size | tr -d '\r' | awk -F' ' '{print $NF}')
W=$(echo "$SIZE" | cut -dx -f1)
H=$(echo "$SIZE" | cut -dx -f2)
echo "screen ${W}x${H}"

CONSENT=$(find /home/runner/work -type f -name 'android-consent.py' -print -quit)
test -n "$CONSENT"
accept_consent() {
    local attempts=$1
    for _ in $(seq 1 "$attempts"); do
        adb shell uiautomator dump /sdcard/window.xml >/dev/null 2>&1 || true
        adb pull /sdcard/window.xml "$OUT/window.xml" >/dev/null 2>&1 || true
        if python3 "$CONSENT" "$OUT/window.xml" | tee -a "$OUT/consent.txt" | grep -q 'tapping'; then
            return 0
        fi
        sleep 2
    done
    return 1
}

dump_failure() {
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    adb shell ip -o addr show > "$OUT/interfaces.txt" 2>&1 || true
    timeout 20 sudo swanctl --list-sas > "$OUT/swanctl-sas.txt" 2>&1 || true
    sudo journalctl -u strongswan -n 120 --no-pager > "$OUT/swanctl-journal.txt" 2>&1 || true
    sudo cat /tmp/bamdad-ipsec/charon.log > "$OUT/charon.log" 2>/dev/null || true
}

wait_for_state() {
    local deadline=$((SECONDS + $2))
    while [ $SECONDS -lt $deadline ]; do
        if adb logcat -d -s BamdadCharon 2>/dev/null | grep -q "state: $1"; then
            return 0
        fi
        sleep 1
    done
    return 1
}

echo "=== connect $(date -u +%H:%M:%S) ==="
launch_app
sleep 5
accept_consent 6 || true
adb shell input tap $((W * 86 / 100)) $((H - 225))
sleep 5
accept_consent 8 || true
adb exec-out screencap -p > "$OUT/connecting.png"

if ! wait_for_state 1 60; then
    echo 'ipsec-not-connected'
    dump_failure
    exit 1
fi
echo 'ipsec-connected'
adb exec-out screencap -p > "$OUT/connected.png"
adb shell ip -o addr show > "$OUT/interfaces.txt" 2>&1 || true

# Server side must show the same CHILD_SA.
timeout 20 sudo swanctl --list-sas > "$OUT/swanctl-sas.txt" 2>&1 || true
cat "$OUT/swanctl-sas.txt"
grep -q 'ESTABLISHED' "$OUT/swanctl-sas.txt" || {
    echo 'server-sa-missing'
    dump_failure
    exit 1
}
grep -qi 'bamdad' "$OUT/swanctl-sas.txt" || {
    echo 'server-sa-mismatch'
    dump_failure
    exit 1
}
echo 'ipsec-server-sa-ok'

echo "=== disconnect $(date -u +%H:%M:%S) ==="
sleep 6
adb shell input tap $((W * 86 / 100)) $((H - 190))
sleep 6
deadline=$((SECONDS + 30))
while [ $SECONDS -lt $deadline ]; do
    if ! adb shell ip -o addr show 2>/dev/null | grep -q '10\.99\.0\.'; then
        break
    fi
    sleep 1
done
if adb shell ip -o addr show 2>/dev/null | grep -q '10\.99\.0\.'; then
    echo 'ipsec-disconnect-failed'
    dump_failure
    exit 1
fi
echo 'ipsec-disconnected'

adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
! grep -q 'FATAL EXCEPTION' "$OUT/full-logcat.txt"
echo 'ipsec-smoke-ok'
