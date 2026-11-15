#!/usr/bin/env bash
# Emulator smoke test for the kill switch:
#   1. with the setting on and no tunnel, the sinkhole must own the VPN slot
#   2. connecting must release the slot to the proxy tunnel and pass traffic
#   3. disconnecting must bring the sinkhole back
set -euo pipefail

OUT=/tmp/bamdad-killswitch-smoke
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

# The kill switch holds 10.255.255.2/32; the proxy tunnel holds 172.19.0.1/30.
# The address on tun0 therefore tells the two apart without parsing logs.
wait_for_address() {
    local needle=$1 timeout=$2
    local deadline=$((SECONDS + timeout))
    while [ $SECONDS -lt $deadline ]; do
        if adb shell ip addr show tun0 2>/dev/null | grep -q "$needle"; then
            return 0
        fi
        sleep 1
    done
    return 1
}

dump_failure() {
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    adb shell dumpsys notification --noredact > "$OUT/notification.txt" 2>&1 || true
    adb shell ip addr show tun0 > "$OUT/tun0.txt" 2>&1 || true
}

# Create the data dir, then seed the same two local test nodes the launch smoke
# uses, with the kill switch enabled.
launch_app >/dev/null 2>&1 || true
sleep 3
adb shell am force-stop com.mehrnet.bamdad || true
DATA_DIR=/data/user/0/com.mehrnet.bamdad/files
adb shell run-as com.mehrnet.bamdad mkdir -p "$DATA_DIR"
python3 - scripts/android-test-profiles.json > "$OUT/profiles.json" <<'PY'
import json
import sys

store = json.load(open(sys.argv[1]))
store.setdefault("settings", {})["kill_switch"] = True
print(json.dumps(store))
PY
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

launch_app
sleep 5
accept_consent 10 || true

echo '--- sinkhole while idle ---'
if ! wait_for_address 10.255.255.2 40; then
    echo 'killswitch-tun-missing'
    dump_failure
    exit 1
fi
echo 'killswitch-tun-up'
adb exec-out screencap -p > "$OUT/killswitch.png"
adb shell dumpsys notification --noredact > "$OUT/notification-idle.txt" 2>&1 || true
if ! grep -q 'Kill switch' "$OUT/notification-idle.txt"; then
    echo 'killswitch-notification-missing'
    dump_failure
    exit 1
fi
echo 'killswitch-notification-ok'

echo '--- connect releases the sinkhole ---'
adb shell input tap $((W * 86 / 100)) $((H - 225))
sleep 5
accept_consent 6 || true
if ! wait_for_address 172.19.0.1 40; then
    echo 'tunnel-tun-missing'
    dump_failure
    exit 1
fi
echo 'tunnel-tun-up'
if adb shell ip addr show tun0 2>/dev/null | grep -q '10.255.255.2'; then
    echo 'killswitch-held-the-slot'
    dump_failure
    exit 1
fi
echo 'killswitch-released'

NETCHECK=$(find "${NETCHECK_DIR:-netcheck}" -type f -name 'bamdad-netcheck' -print -quit 2>/dev/null || true)
if [ -n "$NETCHECK" ]; then
    adb push "$NETCHECK" /data/local/tmp/bamdad-netcheck >/dev/null
    adb shell chmod 755 /data/local/tmp/bamdad-netcheck
    ok=false
    for _ in 1 2 3; do
        if adb shell /data/local/tmp/bamdad-netcheck example.com 80 2>&1 \
            | tee "$OUT/netcheck.txt" | grep -q 'HTTP/1'; then
            ok=true
            break
        fi
        sleep 3
    done
    if ! $ok; then
        echo 'tunnel-traffic-failed'
        dump_failure
        exit 1
    fi
    echo 'tunnel-traffic-ok'
else
    echo 'bamdad-netcheck not found; skipping traffic assertion'
fi
adb shell dumpsys notification --noredact > "$OUT/notification-tunnel.txt" 2>&1 || true
grep -q 'Proxy' "$OUT/notification-tunnel.txt" || {
    echo 'tunnel-notification-missing'
    dump_failure
    exit 1
}
echo 'tunnel-notification-ok'

echo '--- disconnect restores the sinkhole ---'
adb shell input tap $((W * 86 / 100)) $((H - 180))
sleep 5
if ! wait_for_address 10.255.255.2 40; then
    echo 'killswitch-not-restored'
    dump_failure
    exit 1
fi
echo 'killswitch-restored'
adb exec-out screencap -p > "$OUT/restored.png"
adb shell dumpsys activity services com.mehrnet.bamdad > "$OUT/services.txt" 2>&1 || true
if ! grep -q 'KillSwitchService' "$OUT/services.txt"; then
    echo 'killswitch-service-missing'
    dump_failure
    exit 1
fi
echo 'killswitch-service-ok'

adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
! grep -q 'FATAL EXCEPTION' "$OUT/full-logcat.txt"
echo 'killswitch-smoke-ok'
