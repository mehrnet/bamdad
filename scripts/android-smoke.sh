#!/usr/bin/env bash
# Emulator smoke test for Bamdad: launch, connect, accept VPN consent, verify
# the hev-socks5-tunnel service started.
set -euo pipefail

OUT=/tmp/bamdad-smoke
rm -rf "$OUT"
mkdir -p "$OUT"

APK=$(find /home/runner/work -type f -name 'app-debug.apk' -print -quit)
test -f "$APK"
adb install -r "$APK"
adb shell pm grant com.mehrnet.bamdad android.permission.POST_NOTIFICATIONS || true
adb logcat -c
adb shell am start -W -n com.mehrnet.bamdad/.MainActivity
sleep 6
adb exec-out screencap -p > "$OUT/before.png"

SIZE=$(adb shell wm size | tr -d '\r' | awk -F' ' '{print $NF}')
W=$(echo "$SIZE" | cut -dx -f1)
H=$(echo "$SIZE" | cut -dx -f2)
echo "screen ${W}x${H}"

# The connect orb is centered in the bottom navigation.
adb shell input tap $((W / 2)) $((H - 180))
sleep 4
adb exec-out screencap -p > "$OUT/connect-tap.png"

CONSENT=$(find /home/runner/work -type f -name 'android-consent.py' -print -quit)
for _ in 1 2 3 4 5; do
    adb shell uiautomator dump /sdcard/window.xml >/dev/null 2>&1 || true
    adb pull /sdcard/window.xml "$OUT/window.xml" >/dev/null 2>&1 || true
    if python3 "$CONSENT" "$OUT/window.xml" | tee "$OUT/consent.txt" | grep -q 'tapping'; then
        break
    fi
    sleep 2
done

sleep 6
adb exec-out screencap -p > "$OUT/after.png"

echo '--- interfaces ---'
adb shell ip addr show tun0 || true
echo '--- connectivity ---'
adb shell dumpsys connectivity | grep -i -A2 -B2 vpn || true

adb logcat -d -v threadtime > "$OUT/full-logcat.txt"
echo '--- vpn log ---'
grep -Ei 'BamdadVpn|hev-socks|VpnService|FATAL EXCEPTION|AndroidRuntime' "$OUT/full-logcat.txt" || true

test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')"
! grep -q 'FATAL EXCEPTION' "$OUT/full-logcat.txt"
grep -q 'VPN tunnel started' "$OUT/full-logcat.txt"
