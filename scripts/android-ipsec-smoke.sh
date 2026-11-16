#!/usr/bin/env bash
# Verifies that the packaged strongSwan libraries load and initialize on the
# emulator. The debug app boots charon once at startup and tears it down again,
# which exercises dlopen of every library, JNI_OnLoad, the plugin list, the
# scheduler, and the JNI callbacks into CharonVpnService.
set -euo pipefail

OUT=/tmp/bamdad-ipsec-smoke
rm -rf "$OUT"
mkdir -p "$OUT"

APK=$(find /home/runner/work -type f -name 'app-debug.apk' -print -quit)
test -f "$APK"
adb install -r "$APK"

# Start from a clean data dir so no stale state affects the boot.
adb shell pm clear com.mehrnet.bamdad >/dev/null 2>&1 || true
adb logcat -c

COMPONENT=$(adb shell cmd package resolve-activity --brief \
    -a android.intent.action.MAIN \
    -c android.intent.category.LAUNCHER com.mehrnet.bamdad 2>/dev/null \
    | tr -d '\r' | tail -1)
test -n "$COMPONENT"
adb shell am start -W -n "$COMPONENT"
adb exec-out screencap -p > "$OUT/launched.png"

for _ in $(seq 1 40); do
    if adb logcat -d -s BamdadCharon 2>/dev/null | grep -q 'ipsec self-test complete'; then
        break
    fi
    sleep 1
done

adb logcat -d -s BamdadCharon > "$OUT/charon.txt" 2>&1 || true
adb logcat -d -v threadtime > "$OUT/full-logcat.txt" 2>&1 || true
cat "$OUT/charon.txt"

grep -q 'charon init: true' "$OUT/charon.txt" || {
    echo 'charon-init-failed'
    exit 1
}
echo 'charon-init-ok'

grep -q 'charon deinit: ok' "$OUT/charon.txt" || {
    echo 'charon-deinit-failed'
    exit 1
}
echo 'charon-deinit-ok'

if grep -q 'FATAL EXCEPTION' "$OUT/full-logcat.txt"; then
    echo 'app-crashed'
    exit 1
fi
test -n "$(adb shell pidof com.mehrnet.bamdad | tr -d '\r')"

# charon writes its own log into the app-private ipsec directory.
adb shell run-as com.mehrnet.bamdad ls -l files/ipsec > "$OUT/ipsec-dir.txt" 2>&1 || true
cat "$OUT/ipsec-dir.txt"
grep -q 'charon.log' "$OUT/ipsec-dir.txt" || {
    echo 'charon-log-missing'
    exit 1
}
echo 'charon-log-ok'
echo 'ipsec-libs-ok'
