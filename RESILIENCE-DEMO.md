# Manual harness: job resilience — "left overnight, it eventually pushes through"

This verifies the job-supervision contract end to end **without containers or agent
credentials**: every job is presumed worth finishing — one that keeps dying is restarted by
the daemon's supervisor with backoff up to its restart budget (25 by default), the whole
chain is linked and logged, the identical-failure breaker gives up loudly, and a human's
stop always wins. (Task-level retry counters, time budgets, backoff, and failure
classification are covered by unit tests in `src/failure.rs` and `src/main.rs` — this harness exercises the
daemon layer above them.)

Follow the steps in order and check each **Expect** line. Report PASS/FAIL per step.

## Setup

From the **`scsh` repo root** after `cargo build`, isolate all state on a test port. The
supervisor's first restart normally waits ~5 minutes; `SCSH_JOB_BACKOFF_INITIAL_SECS=5`
shrinks it so this harness runs in about two minutes. The `STUB` wrapper keeps daemon
commands real but makes every spawned JOB die instantly — a stand-in for "the run keeps
failing terminally, whatever the reason":

```console
export SCSH_BIN="$PWD/target/debug/scsh"
export SCSH_DAEMON_PORT=7392
export SCSH_HOME="$(mktemp -d)"
export SCSH_HARNESS_HOME="$(mktemp -d)"
REPO="$(mktemp -d)"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@example.com
git -C "$REPO" config user.name t
printf 'tmp/\n' > "$REPO/.gitignore"
mkdir -p "$REPO/tmp"
git -C "$REPO" add .gitignore && git -C "$REPO" commit -qm init
STUB="$(mktemp -t scsh-stub)"
printf '#!/bin/sh\ncase "$1" in daemon|__daemon-serve) exec "%s" "$@";; *) echo boom >&2; exit 1;; esac\n' "$SCSH_BIN" > "$STUB"
chmod +x "$STUB"
SCSH_JOB_BACKOFF_INITIAL_SECS=5 SCSH_BIN="$STUB" "$SCSH_BIN" daemon start
```

**Expect:** the daemon reports listening on `http://127.0.0.1:7392`.

## 1. A job that keeps dying is restarted by the daemon

```console
SID=$(curl -s -X POST "localhost:7392/api/v1/jobs/start" \
  -d "{\"repo\":\"$REPO\",\"def\":\"greet\"}" | sed 's/.*"session":"\([a-z]*\)".*/\1/')
echo "first session: $SID"
sleep 25
curl -s "localhost:7392/api/v1/session/$SID" | python3 -c 'import json,sys; s=json.load(sys.stdin); print(s["lifecycle"], s.get("supervisor"))'
```

**Expect:** lifecycle `failed`, and a `supervisor` object with `"retries": 25` (supervision
is the default — nothing opted in), `"job_attempt": 1`, and either a `next_retry_at`
timestamp or (if the restart already fired) `"restarted_as"` naming a new session id.

## 2. The chain runs to the identical-failure breaker and gives up loudly

Every restart dies the same way (`run failed to start`), so after three consecutive
identical failures the supervisor must stop — restarting further would burn tokens on a
deterministic failure. Wait for the chain to settle (three restarts at ~5–10s backoff,
supervisor tick every 15s ⇒ under two minutes), then walk it:

```console
sleep 100
for ID in $(curl -s "localhost:7392/api/v1/sessions" | python3 -c 'import json,sys; print(" ".join(json.load(sys.stdin)["sessions"]))'); do
  curl -s "localhost:7392/api/v1/session/$ID" | python3 -c '
import json, sys
s = json.load(sys.stdin)
sup = s.get("supervisor") or {}
if sup.get("retries"):
    print(s["started_at"], s["id"], "attempt", sup.get("job_attempt"), "restarted_as", sup.get("restarted_as"), "gave_up", sup.get("gave_up"))
'
done | sort -n
```

**Expect:** a chain of sessions with `attempt` 1, 2, 3, 4 — each non-final one carrying
`restarted_as` pointing at the next — and the FINAL session showing `gave_up` containing
`consecutive runs failed identically`. No session after the gave-up one appears, and
waiting longer adds none.

## 3. The story is in the failures log

```console
"$SCSH_BIN" failures --last 30 | grep -E "supervisor_(scheduled|restart|gave_up)" | head
```

**Expect:** `supervisor_scheduled` lines naming `restart K/25 … starting in Ns`,
`supervisor_restart` lines linking each restart, and one `supervisor_gave_up` line with the
breaker's reason — the morning-after timeline, reconstructable from the log alone.

## 4. The job page says what the supervisor is doing

```console
LAST=$(for ID in $(curl -s "localhost:7392/api/v1/sessions" | python3 -c 'import json,sys; print(" ".join(json.load(sys.stdin)["sessions"]))'); do
  curl -s "localhost:7392/api/v1/session/$ID" | python3 -c '
import json, sys
s = json.load(sys.stdin)
if (s.get("supervisor") or {}).get("gave_up"): print(s["started_at"], s["id"])
'
done | sort -n | tail -1 | cut -d" " -f2)
curl -s "localhost:7392/job/$LAST" | grep -o "Job restarts</dt><dd[^<]*>[^<]*" | head -1
```

**Expect:** the meta list shows a `Job restarts` row reading `3 of 25 · gave up — 4
consecutive runs failed identically at …` (attempt number per your observed chain).

## 5. A human's stop cancels supervision permanently

```console
SID2=$(curl -s -X POST "localhost:7392/api/v1/jobs/start" \
  -d "{\"repo\":\"$REPO\",\"def\":\"greet\"}" | sed 's/.*"session":"\([a-z]*\)".*/\1/')
curl -s -X POST "localhost:7392/api/v1/session/stop" -d "{\"session\":\"$SID2\"}"
sleep 40
curl -s "localhost:7392/api/v1/session/$SID2" | python3 -c 'import json,sys; s=json.load(sys.stdin); print(s["lifecycle"], s["supervisor"].get("gave_up"), s["supervisor"].get("restarted_as"))'
```

**Expect:** `gave_up` says `stopped from the session browser`, `restarted_as` stays `None`,
and no new session for `$REPO` ever appears — the supervisor obeys the human. (The stub job
dies so fast the stop may answer `already_ended:true`; supervision is cancelled either
way — a stop that arrives after the job settled still means stop.)

## 6. (Optional, real runtime) The overnight scenario, for real

With a container runtime and agent credentials, run a real workflow and observe the full
stack — five task retries within a 30m budget, and the supervisor behind them:

```console
"$SCSH_BIN" daemon stop && "$SCSH_BIN" daemon start     # real daemon, no stub
cd "$REPO" && "$SCSH_BIN" run --def greet
```

**Expect:** the job page shows no restart row until supervision acts. If a task fails
transiently mid-run, its retry row notes `retrying in ~Ns … (retry K of 5, … left)`; if
the whole run is killed (`kill -9` its PID), the daemon restarts the job with
`--resume-from`, restoring completed steps instantly and re-running only the frontier.

## Cleanup

```console
"$SCSH_BIN" daemon stop
rm -rf "$REPO" "$SCSH_HOME" "$SCSH_HARNESS_HOME" "$STUB"
rm -f "$TMPDIR/scsh-daemon/daemon-7392.pid" "$TMPDIR/scsh-daemon/daemon-7392.mode"
```
