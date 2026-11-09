#!/usr/bin/env python3
"""Tap the Android VPN consent dialog's positive button in a UI dump."""
import re
import subprocess
import sys

path = sys.argv[1] if len(sys.argv) > 1 else "window.xml"
try:
    xml = open(path, encoding="utf-8").read()
except OSError:
    print(f"could not read {path}")
    sys.exit(0)

for label in ("OK", "Allow", "ALLOW"):
    pattern = (
        r'<node[^>]*text="' + label + r'"[^>]*bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"'
    )
    for match in re.finditer(pattern, xml):
        x1, y1, x2, y2 = map(int, match.groups())
        x, y = (x1 + x2) // 2, (y1 + y2) // 2
        print(f"tapping {label} at {x},{y}")
        subprocess.run(
            ["adb", "shell", "input", "tap", str(x), str(y)],
            check=False,
        )
        sys.exit(0)

print("no VPN consent button found")
