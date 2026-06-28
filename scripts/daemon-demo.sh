#!/usr/bin/env bash
# Demo: start the session browser daemon, verify it responds, then stop it.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${CARGO_BIN_EXE_scsh:-./target/debug/scsh}"
if [[ ! -x "$BIN" ]]; then
  cargo build
  BIN="./target/debug/scsh"
fi

PORT="${SCSH_DAEMON_PORT:-}"
if [[ -z "$PORT" ]]; then
  PORT=$(python3 -c "import socket; s=socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1]); s.close()")
fi
export SCSH_DAEMON_PORT="$PORT"

cleanup() {
  "$BIN" daemon stop 2>/dev/null || true
  rm -f "${TMPDIR:-/tmp}/scsh-daemon/daemon-${PORT}.json"         "${TMPDIR:-/tmp}/scsh-daemon/daemon-${PORT}.pid"
}
trap cleanup EXIT

"$BIN" daemon stop 2>/dev/null || true
sleep 0.2

echo "→ starting daemon on port $PORT"
"$BIN" daemon start
"$BIN" daemon status

echo "→ fetching session index"
html=$(curl -sf "http://127.0.0.1:${PORT}/")
if [[ "$html" != *"scsh session browser"* ]]; then
  echo "unexpected index HTML" >&2
  exit 1
fi
echo "→ session index OK"

echo "→ stopping daemon"
"$BIN" daemon stop

echo "✓ daemon demo complete"
