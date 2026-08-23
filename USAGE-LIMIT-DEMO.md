# Manual harness: surviving a claude usage limit

This verifies the usage-limit contract end to end **without waiting for a real limit and
without burning an account**: a claude run stopped by a quota window is held rather than
killed, shows up as its own state in the session browser, is handed the keypress the
screens that need one are waiting for, and — if it cannot be resumed in place — is retried
at the reset instant instead of on a backoff that could never reach it.

The trick that makes it testable: **the screen is a file.** `scsh` reads the harness's
terminal from the run's bind-mounted asciicast at `tmp/scsh-run.log.cast`, so appending one
output event to that file is indistinguishable, to every layer above it, from claude having
printed the banner itself. No fake-limit flag exists in the binary and none is needed.

(The decision logic — which prose means what, when the clocks freeze, when the wait is
given up on, how the retry is scheduled — is covered by unit tests in `src/limitwait.rs`,
`src/ui/screen.rs`, `src/main.rs`, and `src/daemon/supervisor.rs`. This harness exercises
the real stack above them.)

Follow the steps in order and check each **Expect** line. Report PASS/FAIL per step.

## What is being verified

| | |
|---|---|
| The wait is armed | every claude container gets `autoContinueAtUsageLimit` in its `settings.json` |
| The CLI is new enough to honour it | `ARG CLAUDE_CODE_VERSION` is pinned, and bumping it rebuilds the image |
| A parked run is not killed | the inactivity and wall-clock clocks freeze while a limit banner is on screen |
| A parked run is legible | the job page shows **Awaiting limits**, in its own colour, with the reset time |
| Stuck screens get unstuck | `Enter` crosses a host-owned, read-only-mounted key channel into the tmux pane |
| A limit that kills a run is not a failure | the retry is scheduled for the reset, not the backoff |

## Setup

From the **`scsh` repo root** after `cargo build --release`. Steps 1–3 need **no container
runtime, no credentials, and no network**; steps 4–6 need a real claude run.

```console
export SCSH_BIN="$PWD/target/release/scsh"
```

## 1. Every claude container arms the wait, on a pinned CLI

```console
"$SCSH_BIN" list --verbose | grep -E "ARG CLAUDE_CODE_VERSION|@anthropic-ai/claude-code"
grep -n "autoContinueAtUsageLimit" src/quota.rs
```

**Expect:** the Dockerfile pins `ARG CLAUDE_CODE_VERSION=<version>` and installs
`"@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"` — never an unpinned `npm install -g`,
which would freeze whatever was latest the day the image was first built and never move
again. And `container_settings_json` writes `"autoContinueAtUsageLimit": true`.

## 2. Bumping the pin actually rebuilds the image

Image rebuilds are keyed on the Dockerfile *text*, so this ARG is the whole mechanism by
which a machine ever picks up a newer claude. With a container runtime up and the claude
image already built:

```console
"$SCSH_BIN" daemon start
curl -s localhost:7274/api/v1/setup | python3 -c 'import json,sys; print([i for i in json.load(sys.stdin)["images"] if i["name"]=="claude"])'
sed -i.bak 's/ARG CLAUDE_CODE_VERSION=.*/ARG CLAUDE_CODE_VERSION=9.9.9/' src/Dockerfile
cargo build --release 2>/dev/null && "$SCSH_BIN" daemon restart
curl -s localhost:7274/api/v1/setup | python3 -c 'import json,sys; print([i for i in json.load(sys.stdin)["images"] if i["name"]=="claude"])'
mv src/Dockerfile.bak src/Dockerfile && cargo build --release 2>/dev/null && "$SCSH_BIN" daemon restart
```

**Expect:** the claude image reads `"status": "ready"` before the edit and `"status":
"stale"` after it — the changed ARG flipped the build fingerprint, so the next run rebuilds.
Restoring the file returns it to `ready` without a rebuild.

(Without a runtime, the same mechanism is asserted by the unit tests
`dockerfile_has_shared_base_and_harness_targets` and
`image_build_fingerprint_is_stable_and_target_specific`.)

## 3. The prose the whole feature hangs on

Claude writes no machine-readable marker for any of this, so the needles are its literal
TUI strings. If they ever drift, the wait silently stops being detected:

```console
cargo test --release --bin scsh limitwait 2>&1 | tail -12
```

**Expect:** all `limitwait::tests` pass — armed banners read as `Waiting`, the post-reset
prompt as `NeedsEnter`, the `/rate-limit-options` dialog as `Blocked`, every give-up
message as `Refused`, and ordinary build output as nothing at all.

## 4. A parked run is held, not killed (real claude run)

Start any claude skill, then **while it is running** find its live recording and append the
banner claude prints when a limit stops it:

```console
cd "$REPO_UNDER_TEST" && "$SCSH_BIN" run &          # any claude skill
sleep 45
CAST=$(ls -t "${TMPDIR:-/tmp}"/scsh-*-run-*/tmp/scsh-run.log.cast | head -1); echo "$CAST"
printf '[999.0, "o", "Usage limit reached \\u00b7 continuing automatically at 8:50am \\u00b7 esc to cancel"]\n' >> "$CAST"
```

**Expect:** within a few seconds the job page marks the task **Awaiting limits** — its own
colour (magenta), its own `⧗` glyph, never the orange of a running task nor the purple of
an abandoned one. The run is **not** killed when its `inactivity_timeout` elapses: the
clock is frozen, not merely widened. The row's note names the wait, and the reset time when
the account's status line has reported one.

## 5. The keypress reaches the pane

Repeat step 4 with the screen that stops dead waiting for a human:

```console
printf '[999.0, "o", "Your usage limit has reset \\u00b7 press enter to continue"]\n' >> "$CAST"
sleep 5
RUN_DIR=$(dirname "$(dirname "$CAST")")
ls -l "$(dirname "$RUN_DIR")/.$(basename "$RUN_DIR").keys-"*/key
grep "forwarded host keys" "$(dirname "$CAST")/scsh-run.log.tuidebug"
```

**Expect:** the host atomically publishes `1 Enter` outside the writable run mount. The
container sees that directory read-only, forwards the record once, and logs `watcher:
forwarded host keys`; the pane receives `Enter`. The same channel un-sticks the
`/rate-limit-options` dialog, whose default choice is the wait `scsh` wants.

## 6. A limit that ends a run schedules its retry for the reset

```console
printf '[999.0, "o", "Automatic continue stopped after repeated usage-limit hits \\u00b7 this task will not resume on its own"]\n' >> "$CAST"
sleep 10
"$SCSH_BIN" failures --last 20 | grep -E "harness_usage_limit|supervisor_scheduled"
```

**Expect:** the attempt ends with reason `harness_usage_limit` — not `container_inactive`,
and not `harness_overloaded`, whose backoff tops out in minutes. The retry row notes
`usage limit reached — retrying at <reset time>`, and the wait is **not** charged to the
route's 30-minute retry budget. If the whole job failed this way, the supervisor's
`supervisor_scheduled` line names a delay reaching the reset instant rather than the
ordinary 5-minute backoff.

## Cleanup

Nothing to clean up: every step either edits a file it restores, or appends to a throwaway
run's recording. Kill the backgrounded `scsh run` if it is still going.
