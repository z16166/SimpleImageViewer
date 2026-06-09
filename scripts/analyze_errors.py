#!/usr/bin/env python3
import collections
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
proc = subprocess.run(["cargo", "check"], cwd=ROOT, text=True, capture_output=True, errors="replace")
out = proc.stdout + proc.stderr
files = re.findall(r"--> (src\\[^\n:]+)", out)
for path, count in collections.Counter(files).most_common(20):
    print(count, path)
print("---")
for code, count in collections.Counter(re.findall(r"(error\[E\d+\])", out)).most_common(15):
    print(count, code)
