#!/usr/bin/env bash
# Fail if common UTF-8/GBK mojibake appears under src/.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PATTERN='鈥|鈹|脳|â€|Ã—|锟|鈫'

if ! command -v rg >/dev/null 2>&1; then
  echo "error: ripgrep (rg) is required for scripts/check-encoding.sh" >&2
  exit 1
fi

if rg -n "$PATTERN" src/; then
  echo ""
  echo "error: mojibake detected in src/ (UTF-8 text was likely mis-decoded as GBK and saved again)." >&2
  echo "Fix the lines above or restore from git; see AGENTS.md." >&2
  exit 1
fi

echo "ok: no common mojibake under src/"
