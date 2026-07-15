#!/usr/bin/env bash
# Thin runner for HARNESS-SMOKE.md — that file is the source of truth; this executes its
# numbered steps and prints PASS/FAIL. scsh itself probes host auth and skips harnesses that
# are not logged in, so there is no auth-probing here: the run succeeds when every AVAILABLE
# harness does, and we validate whatever result files the run produced.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Step 1: prefer this repo's own build (an older `scsh` on PATH may not know these harnesses).
if [[ -n "${SCSH:-}" ]]; then
  :
elif [[ -x "$ROOT/target/release/scsh" && "$ROOT/target/release/scsh" -nt "$ROOT/target/debug/scsh" ]]; then
  SCSH="$ROOT/target/release/scsh"
elif [[ -x "$ROOT/target/debug/scsh" ]]; then
  SCSH="$ROOT/target/debug/scsh"
elif [[ -x "$ROOT/target/release/scsh" ]]; then
  SCSH="$ROOT/target/release/scsh"
elif command -v scsh >/dev/null 2>&1; then
  SCSH="scsh"
else
  echo "harness-smoke: FAIL — scsh not found (build with: cargo build)" >&2
  exit 1
fi

# Step 2: clean tree (scsh clones committed state only).
if [ -n "$(git status --porcelain --ignore-submodules)" ]; then
  echo "harness-smoke: FAIL — working tree is not clean; commit or stash, then re-run" >&2
  exit 1
fi

# Step 3: the profile must exist with its skills.
echo "=== check-profile ==="
"$SCSH" check-profile harness-smoke

# Step 4: run. scsh skips harnesses with no host auth; a skipped route does not fail the run.
echo ""
echo "=== run (profile harness-smoke) using $SCSH ==="
run_exit=0
SCSH_KEEP_RUNS=1 "$SCSH" run --profile harness-smoke || run_exit=$?

# Step 5: validate every result file the run produced (≥1 required, each must be status OK).
echo ""
echo "=== validate result JSON ==="
present=0
fail=0
for route in claude-opus-4-8 codex-luna cursor-composer-fast; do
  file="tmp/harness-smoke-${route}.json"
  [ -f "$file" ] || continue
  present=$((present + 1))
  if ! command -v jq >/dev/null 2>&1; then
    echo "PASS  $file — present (install jq to validate JSON)"
    continue
  fi
  status="$(jq -r '.result.status // empty' "$file" 2>/dev/null || true)"
  if [ "$status" = "OK" ]; then
    echo "PASS  $file — result.status=OK"
  else
    echo "FAIL  $file — expected result.status=OK, got: ${status:-<invalid json>}"
    fail=$((fail + 1))
  fi
done

# Step 6: show the screencasts this run recorded.
echo ""
echo "=== screencasts (gitignored tmp/casts/, timestamped) ==="
ls -1t tmp/casts/harness-smoke-*.cast 2>/dev/null | head -6 || echo "  (none recorded)"

echo ""
echo "=== summary ==="
echo "result files: $present present, $fail failed validation"
if [ "$run_exit" -ne 0 ] || [ "$fail" -gt 0 ] || [ "$present" -eq 0 ]; then
  echo "harness-smoke: FAIL"
  exit 1
fi
echo "harness-smoke: PASS"
