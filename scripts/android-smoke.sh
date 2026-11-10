#!/usr/bin/env bash
# Emulator smoke test for Bamdad: launch, connect, accept VPN consent, verify
# the tun2socks tunnel started and actually carried traffic.
set -euo pipefail

OUT=/tmp/bamdad-smoke
rm -rf "$OUT"
mkdir -p "$OUT"

APK=$(find /home/runner/work -type f -name 'app-debug.apk' -print -quit)
test -f "$APK"
adb install -r "$APK"
adb shell pm grant com.mehrnet.bamdad android.permission.POST_NOTIFICATIONS || true
adb shell settings put global hide_error_dialogs 1 || true
adb shell settings put global anr_show_background 0 || true
adb shell am broadcast -a android.intent.action.CLOSE_SYSTEM_DIALOGS || true
adb logcat -c
adb shell am start -W -n com.mehrnet.bamdad/.MainActivity
sleep 8
adb exec-out screencap -p > "$OUT/before.png"

SIZE=$(adb shell wm size | tr -d '\r' | awk -F' ' '{print $NF}')
W=$(echo "$SIZE" | cut -dx -f1)
H=$(echo "$SIZE" | cut -dx -f2)
echo "screen ${W}x${H}"

# The connect orb is centered in the bottom navigation. Try a few nearby
# vertical positions in case the safe-area layout shifts it.
CONSENT=$(find /home/runner/work -type f -name 'android-consent.py' -print -quit)
consented=false
for offset in 180 150 210; do
    adb shell input tap $((W / 2)) $((H - offset))
    sleep 3
    for _ in 1 2 3 4; do
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
adb shell input tap $((W / 2)) $((H * 1090 / 1920))
sleep 5
if ! adb shell ip addr show tun0 2>/dev/null | grep -q 'inet '; then
    echo 'vpn-dropped-on-switch'
    adb logcat -d -v threadtime > "$OUT/full-logcat.txt" || true
    exit 1
fi
adb shell /data/local/tmp/bamdad-netcheck example.com 80 | tee "$OUT/netcheck-after-switch.txt"
grep -q 'HTTP/1' "$OUT/netcheck-after-switch.txt"
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
adb shell am start -W -n com.mehrnet.bamdad/.MainActivity >/dev/null
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
adb shell input tap $((W / 2)) $((H - 180))
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
