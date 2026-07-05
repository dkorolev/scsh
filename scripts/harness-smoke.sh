#!/usr/bin/env bash
# Smoke-test the grok and cursor container harnesses via the harness-smoke skill.
# See HARNESS-SMOKE.md for the full walkthrough.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SCSH="${SCSH:-scsh}"
if ! command -v "$SCSH" >/dev/null 2>&1; then
  if [[ -x "$ROOT/target/debug/scsh" ]]; then
    SCSH="$ROOT/target/debug/scsh"
  elif [[ -x "$ROOT/target/release/scsh" ]]; then
    SCSH="$ROOT/target/release/scsh"
  else
    echo "harness-smoke: scsh not found — build with: cargo build --release" >&2
    exit 1
  fi
fi

echo "=== harness-smoke probe ==="
GROK_OK=0
CURSOR_OK=0

if { test -f "$HOME/.grok/auth.json" || [ -n "${XAI_API_KEY:-}" ]; } && command -v grok >/dev/null 2>&1; then
  echo "route grok-build (harness-smoke-grok-build): ok"
  GROK_OK=1
else
  echo "route grok-build (harness-smoke-grok-build): N/A — run \`grok login\` or export XAI_API_KEY"
fi

if { test -f "$HOME/.config/cursor/auth.json" || test -f "$HOME/.cursor/auth.json" \
  || security find-generic-password -s cursor-access-token -w >/dev/null 2>&1 \
  || [ -n "${CURSOR_API_KEY:-}" ]; } && command -v cursor >/dev/null 2>&1; then
  echo "route cursor-composer (harness-smoke-cursor-composer): ok"
  CURSOR_OK=1
else
  echo "route cursor-composer (harness-smoke-cursor-composer): N/A — run \`cursor agent login\` or export CURSOR_API_KEY"
fi

ROUTES=$((GROK_OK + CURSOR_OK))
echo "harness-smoke routes available: $ROUTES / 2"
if [ "$ROUTES" -eq 0 ]; then
  echo "harness-smoke: FAIL — no grok or cursor route available on this host" >&2
  exit 1
fi

echo ""
echo "=== preflight ==="
if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "harness-smoke: FAIL — not inside a git repository" >&2
  exit 1
fi
if [ -n "$(git status --porcelain --ignore-submodules)" ]; then
  echo "harness-smoke: FAIL — working tree is not clean (scsh run requires committed state)" >&2
  echo "  commit or stash your changes, then re-run: $ROOT/scripts/harness-smoke.sh" >&2
  exit 1
fi

"$SCSH" check-profile harness-smoke

echo ""
echo "=== run (profile harness-smoke) ==="
echo "Using: $SCSH"
SCSH_KEEP_RUNS=1 "$SCSH" run --profile harness-smoke
run_exit=$?

echo ""
echo "=== validate result JSON ==="
pass=0
fail=0

check_result() {
  local route="$1"
  local file="tmp/harness-smoke-${route}.json"
  if [ ! -f "$file" ]; then
    echo "FAIL  $file — missing"
    fail=$((fail + 1))
    return
  fi
  if ! command -v jq >/dev/null 2>&1; then
    echo "PASS  $file — present (install jq to validate JSON schema)"
    pass=$((pass + 1))
    return
  fi
  local status
  status="$(jq -r '.result.status // empty' "$file" 2>/dev/null || true)"
  if [ "$status" = "OK" ]; then
    echo "PASS  $file — result.status=OK"
    pass=$((pass + 1))
  else
    echo "FAIL  $file — expected result.status=OK, got: ${status:-<invalid json>}"
    fail=$((fail + 1))
  fi
}

[ "$GROK_OK" -eq 1 ] && check_result "grok-build"
[ "$CURSOR_OK" -eq 1 ] && check_result "cursor-composer"

echo ""
echo "=== summary ==="
echo "predictions passed: $pass / $((pass + fail))"
if [ "$run_exit" -ne 0 ] || [ "$fail" -gt 0 ]; then
  echo "harness-smoke: FAIL"
  exit 1
fi
echo "harness-smoke: PASS"
exit 0
