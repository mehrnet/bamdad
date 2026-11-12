#!/usr/bin/env bash
# Emulator smoke test for Bamdad: launch, connect, accept VPN consent, verify
# the tun2socks tunnel started and actually carried traffic.
set -euo pipefail

OUT=/tmp/bamdad-smoke
rm -rf "$OUT"
mkdir -p "$OUT"

APK=$(find /home/runner/work -type f -name 'app-debug.apk' -print -quit)
test -f "$APK"
launch_app() {
    local component
    component=$(adb shell cmd package resolve-activity --brief \
        -a android.intent.action.MAIN \
        -c android.intent.category.LAUNCHER com.mehrnet.bamdad 2>/dev/null \
        | tr -d '\r' | tail -1)
    test -n "$component"
    echo "launching $component"
    adb shell am start -W -n "$component"
}

adb install -r "$APK"
# Create the app data directory, then seed two local test nodes so the smoke
# can verify mid-connection core switching.
launch_app >/dev/null 2>&1 || true
sleep 3
adb shell am force-stop com.mehrnet.bamdad || true
DATA_DIR=/data/user/0/com.mehrnet.bamdad/files
adb shell run-as com.mehrnet.bamdad mkdir -p "$DATA_DIR"
adb shell "run-as com.mehrnet.bamdad sh -c 'cat > $DATA_DIR/profiles.json'" < scripts/android-test-profiles.json
adb shell pm grant com.mehrnet.bamdad android.permission.POST_NOTIFICATIONS || true
adb shell settings put global hide_error_dialogs 1 || true
adb shell settings put global anr_show_background 0 || true
adb shell am broadcast -a android.intent.action.CLOSE_SYSTEM_DIALOGS || true
adb logcat -c
launch_app
sleep 8
adb exec-out screencap -p > "$OUT/before.png"
adb shell dumpsys activity activities 2>/dev/null | grep -E 'bamdad|ResumedActivity' > "$OUT/activities-before.txt" || true

echo '--- launcher alias ---'
LAUNCHER=$(adb shell cmd package resolve-activity --brief -a android.intent.action.MAIN -c android.intent.category.LAUNCHER com.mehrnet.bamdad 2>/dev/null | tr -d '\r' | tail -1)
echo "resolved launcher: $LAUNCHER"
case "$LAUNCHER" in
    *LauncherPink*) echo 'launcher-alias-ok' ;;
    *) echo 'launcher-alias-wrong'; adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true; exit 1 ;;
esac
test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')"
echo 'main-activity-alive-after-alias-switch'

SIZE=$(adb shell wm size | tr -d '\r' | awk -F' ' '{print $NF}')
W=$(echo "$SIZE" | cut -dx -f1)
H=$(echo "$SIZE" | cut -dx -f2)
echo "screen ${W}x${H}"

# The connect orb is centered in the bottom navigation. Try a few nearby
# vertical positions in case the safe-area layout shifts it.
CONSENT=$(find /home/runner/work -type f -name 'android-consent.py' -print -quit)
consented=false
for offset in 225 195 255; do
    # If the consent dialog is already showing, consume it before tapping the
    # connect orb again; repeated orb taps can land on the dialog and cancel it.
    for _ in 1 2 3 4 5 6; do
        adb shell uiautomator dump /sdcard/window.xml >/dev/null 2>&1 || true
        adb pull /sdcard/window.xml "$OUT/window.xml" >/dev/null 2>&1 || true
        if python3 "$CONSENT" "$OUT/window.xml" | tee "$OUT/consent.txt" | grep -q 'tapping'; then
            consented=true
            break
        fi
        if grep -q 'com.android.vpndialogs' "$OUT/window.xml"; then
            sleep 2
            continue
        fi
        break
    done
    if $consented; then
        break
    fi
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

sleep 6
adb exec-out screencap -p > "$OUT/after.png"

echo '--- interfaces ---'
adb shell ip addr show tun0 || true
echo '--- connectivity ---'
adb shell dumpsys connectivity | grep -i -A2 -B2 vpn || true

if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'vpn-down-before-traffic-test'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi
NETCHECK=$(find "${NETCHECK_DIR:-netcheck}" -type f -name 'bamdad-netcheck' -print -quit 2>/dev/null || true)
if [ -n "$NETCHECK" ]; then
    adb push "$NETCHECK" /data/local/tmp/bamdad-netcheck >/dev/null
    adb shell chmod 755 /data/local/tmp/bamdad-netcheck
    set +e
    adb shell /data/local/tmp/bamdad-netcheck example.com 80 2>&1 | tee "$OUT/netcheck.txt"
    NETCHECK_STATUS=${PIPESTATUS[0]}
    set -e
    if [ "$NETCHECK_STATUS" -ne 0 ]; then
        echo 'vpn-traffic-failed'
        adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
        exit 1
    fi
    if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
        echo 'vpn-down-during-traffic-test'
        adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
        exit 1
    fi
    echo 'vpn-traffic-ok'
else
    echo 'bamdad-netcheck not found; skipping end-to-end traffic assertion'
fi

echo '--- vpn notification ---'
adb shell cmd statusbar expand-notifications 2>/dev/null || true
sleep 1
adb exec-out screencap -p > "$OUT/notification.png"
adb shell cmd statusbar collapse 2>/dev/null || true
adb shell dumpsys notification --noredact > "$OUT/notification.txt" 2>/dev/null || true
grep -A40 'com.mehrnet.bamdad' "$OUT/notification.txt" | head -60 || true
if grep -q 'Test' "$OUT/notification.txt"; then
    echo 'notification-profile-ok'
else
    echo 'notification-profile-missing'
    adb logcat -d -s BamdadVpn | tail -30 || true
    exit 1
fi

adb logcat -d -v threadtime > "$OUT/full-logcat.txt"
echo '--- vpn log ---'
grep -Ei 'BamdadVpn|tun2socks|VpnService|FATAL EXCEPTION|AndroidRuntime' "$OUT/full-logcat.txt" || true

test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')"
! grep -q 'FATAL EXCEPTION' "$OUT/full-logcat.txt"
grep -q 'VPN tunnel started' "$OUT/full-logcat.txt"
grep -q 'proxy check: SOCKS CONNECT example.com:443 ok' "$OUT/full-logcat.txt"

echo '--- ipv6 captured ---'
if ! adb shell ip -6 addr show tun0 2>/dev/null | grep -q 'inet6 '; then
    echo 'ipv6-not-captured'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi

echo '--- switch core while connected ---'
adb shell input tap $((W / 2)) $((H * 340 / 1000))
sleep 10
if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'vpn-dropped-on-switch'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi
set +e
for attempt in 1 2 3; do
    adb shell /data/local/tmp/bamdad-netcheck example.com 80 2>&1 | tee "$OUT/netcheck-after-switch.txt"
    if grep -q 'HTTP/1' "$OUT/netcheck-after-switch.txt"; then
        SWITCH_TRAFFIC_OK=1
        break
    fi
    sleep 3
done
set -e
if [ "${SWITCH_TRAFFIC_OK:-0}" != "1" ]; then
    echo 'switch-traffic-failed'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi
adb shell run-as com.mehrnet.bamdad cat files/profiles.json > "$OUT/profiles-after-switch.json"
python3 - "$OUT/profiles-after-switch.json" <<'PY'
import json, sys
store = json.load(open(sys.argv[1]))
selected = store.get("selected_id")
profile = next((p for p in store.get("profiles", []) if p.get("id") == selected), None)
if not profile or "sing-box" not in profile.get("name", "").lower():
    print("switch-not-applied")
    sys.exit(1)
print("switch-selected", profile.get("name"))
PY
echo 'switch-ok'

echo '--- close window, keep vpn ---'
adb shell input keyevent KEYCODE_BACK
sleep 4
if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'vpn-stopped-on-window-close'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi
if ! adb shell dumpsys activity services com.mehrnet.bamdad | grep -q 'BamdadVpnService'; then
    echo 'vpn-service-stopped-on-window-close'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi
launch_app >/dev/null
sleep 6
adb exec-out screencap -p > "$OUT/reopened.png"
if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'vpn-stopped-on-reopen'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi
adb shell /data/local/tmp/bamdad-netcheck example.com 80 | tee "$OUT/netcheck-after-reopen.txt"
grep -q 'HTTP/1' "$OUT/netcheck-after-reopen.txt"
echo 'vpn-survived-window-close'

echo '--- disconnect ---'
adb shell input tap $((W * 86 / 100)) $((H - 180))
sleep 4
adb exec-out screencap -p > "$OUT/disconnected.png"
adb logcat -d -v threadtime > "$OUT/full-logcat.txt"
grep -q 'VPN tunnel stopped' "$OUT/full-logcat.txt"
if adb shell dumpsys activity services com.mehrnet.bamdad | grep -q 'BamdadVpnService'; then
    echo 'BamdadVpnService is still running'
    exit 1
fi
if adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'tun0 is still present'
    exit 1
fi
if adb shell dumpsys notification --noredact | grep -q 'Bamdad VPN'; then
    echo 'VPN notification is still present'
    exit 1
fi
test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')"
echo 'disconnect-ok' 
