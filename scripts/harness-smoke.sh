#!/usr/bin/env bash
# Smoke-test the claude, codex, and cursor container harnesses via the harness-smoke skill.
# See HARNESS-SMOKE.md for the full walkthrough.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Prefer this repo's own build — the smoke test exercises the harnesses of the checked-out
# code, and an older `scsh` on PATH may not know them. SCSH=… still overrides.
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
  echo "harness-smoke: scsh not found — build with: cargo build --release" >&2
  exit 1
fi

echo "=== harness-smoke probe ==="
CLAUDE_OK=0
CODEX_OK=0
CURSOR_OK=0

# claude: scsh forwards CLAUDE_CODE_OAUTH_TOKEN or ~/.claude/.credentials.json. On macOS the
# token usually lives only in the login keychain, so lift it into the env for this run.
if [ -z "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] && [ ! -f "$HOME/.claude/.credentials.json" ] \
  && command -v security >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
  keychain_token="$(security find-generic-password -s "Claude Code-credentials" -w 2>/dev/null \
    | jq -r '.claudeAiOauth.accessToken // empty' 2>/dev/null || true)"
  if [ -n "$keychain_token" ]; then
    export CLAUDE_CODE_OAUTH_TOKEN="$keychain_token"
    echo "claude: using OAuth token from the macOS keychain"
  fi
fi
if [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || [ -f "$HOME/.claude/.credentials.json" ]; then
  echo "route claude-opus-4-8 (harness-smoke-claude-opus-4-8): ok"
  CLAUDE_OK=1
else
  echo "route claude-opus-4-8 (harness-smoke-claude-opus-4-8): N/A — run \`claude setup-token\` and export CLAUDE_CODE_OAUTH_TOKEN"
fi

if test -f "${CODEX_HOME:-$HOME/.codex}/auth.json" || [ -n "${OPENAI_API_KEY:-}" ]; then
  echo "route codex-gpt-5.5 (harness-smoke-codex-gpt-5.5): ok"
  CODEX_OK=1
else
  echo "route codex-gpt-5.5 (harness-smoke-codex-gpt-5.5): N/A — run \`codex login\` or export OPENAI_API_KEY"
fi

if test -f "$HOME/.config/cursor/auth.json" || test -f "$HOME/.cursor/auth.json" \
  || security find-generic-password -s cursor-access-token -w >/dev/null 2>&1 \
  || [ -n "${CURSOR_API_KEY:-}" ]; then
  echo "route cursor-composer-fast (harness-smoke-cursor-composer-fast): ok"
  CURSOR_OK=1
else
  echo "route cursor-composer-fast (harness-smoke-cursor-composer-fast): N/A — run \`cursor agent login\` or export CURSOR_API_KEY"
fi

ROUTES=$((CLAUDE_OK + CODEX_OK + CURSOR_OK))
echo "harness-smoke routes available: $ROUTES / 3"
if [ "$ROUTES" -eq 0 ]; then
  echo "harness-smoke: FAIL — no claude, codex, or cursor route available on this host" >&2
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

[ "$CLAUDE_OK" -eq 1 ] && check_result "claude-opus-4-8"
[ "$CODEX_OK" -eq 1 ] && check_result "codex-gpt-5.5"
[ "$CURSOR_OK" -eq 1 ] && check_result "cursor-composer-fast"

echo ""
echo "=== summary ==="
echo "predictions passed: $pass / $((pass + fail))"
if [ "$run_exit" -ne 0 ] || [ "$fail" -gt 0 ]; then
  echo "harness-smoke: FAIL"
  exit 1
fi
echo "harness-smoke: PASS"
exit 0
