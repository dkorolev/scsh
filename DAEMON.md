# Session browser daemon

`scsh` can run a small HTTP server on **localhost** (default port **7274** — `scsh` on a
numeric keypad) that collects events from every `scsh run` and renders them as an expandable
web UI. Terminal scrollback is painful for parallel container output; the session browser is
the easier way to read build logs, harness output, and per-skill results.

## Commands

```console
scsh daemon start      # persistent — runs until scsh daemon stop
scsh daemon stop       # stop the daemon
scsh daemon restart    # stop then start (persistent)
scsh daemon status     # exit 0 when the daemon is listening
```

During `scsh run`, if no persistent daemon is already running, `scsh` auto-starts an
**ephemeral** daemon. That daemon stays up while runs are active (with periodic pings) and
shuts down **five minutes** after the last client disconnects.

Every `scsh run` gets a session id of six random lowercase letters. When the run finishes, the
last line printed is the deep link URL (reachable while a daemon is listening on that port;
start `scsh daemon start` for durable post-run browsing, or rely on persisted state after
`scsh daemon restart`):

```text
http://127.0.0.1:7274/session/abcdef
```

Open it in a browser to see image builds and skills as collapsible sections, with timestamped
harness output and container names.

## What is collected

| Event | Source |
| --- | --- |
| Image build start / output / success / failure | Live board build rows |
| Skill clone / harness phases | Proc notes |
| Container start / stop | Named container around each skill |
| Every stdout/stderr line | Build tail + harness tee (`scsh-run.log` stream) |
| Terminal recording (`.cast`) | asciinema PTY recording of each harness (see below) |

## Terminal recordings (asciinema)

Every harness runs inside a real PTY recorded by asciinema (size from the `terminal:`
block in `.scsh.yml`, default **200×50**). For **claude, codex, and cursor** the recording
is the genuine end-to-end interactive TUI — the same screen a human would see — driven
inside tmux: the skill prompt is the first message, a watcher waits for the skill's
result file, then sends the harness its quit keys (`/exit`, double Ctrl-C) and ends the
session. opencode and grok record their headless output streams. The session page shows two links per skill:

- **▶ watch cast** — `/cast/{session}/{proc}/play`: an in-browser player (vendored
  asciinema-player) with play/pause, timeline scrubbing, speed control, and
  **deep links to timestamps** — append `#t=90` or `#t=1:30` to the player URL. While a
  skill is still running the page shows a live badge and a *Reload recording* button.
- **⬇ download .cast** — `/cast/{session}/{proc}?dl=1`: the raw asciicast v2 file.
  Works **mid-run**: the recording is NDJSON, so the daemon serves the bytes written so
  far (truncated to the last complete line), which is itself a valid partial cast.

While the container runs, the cast is served straight from the run dir
(`<run_dir>/tmp/scsh-run.log.cast`, bind-mounted and growing live). When the skill ends,
`scsh run` copies it into the caller repo's gitignored
**`tmp/casts/<skill>-<YYYYMMDD-HHMMSS>-utc.cast`** — timestamped so past recordings can be
revisited later — and replay keeps working after run-dir pruning. scsh never deletes these
copies; clean `tmp/casts/` whenever you like.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `SCSH_DAEMON_PORT` | `7274` | HTTP listen port (localhost only) |

State and PID files live under the **system temp dir**: `$TMPDIR/scsh-daemon/` (not the
repo's gitignored `tmp/`).

## API (for scripts)

- `GET /` — HTML session index
- `GET /session/{id}` — HTML session detail
- `GET /cast/{session}/{proc}` — asciicast v2 recording (valid partial file mid-run);
  `?dl=1` for a download attachment
- `GET /cast/{session}/{proc}/play` — HTML player page (scrub, pause, `#t=…` deep links)
- `GET /assets/asciinema-player.{js,css}` — vendored player assets
- `GET /api/v1/sessions` — JSON session id list
- `GET /api/v1/session/{id}` — JSON session detail
- `POST /api/v1/session/start`, `/register`, `/deregister`, `/ping`, `/proc/*`, `/container`
  — event ingestion (used by `scsh run`); `/proc/cast` registers a proc's recording path

## Assumptions

- **Assumed:** Port 7274 is acceptable as the default (`scsh` keypad mnemonic); override
  with `SCSH_DAEMON_PORT`.
- **Assumed:** Localhost-only binding is sufficient — no auth layer on the HTTP server.
- **Assumed:** Ephemeral idle timeout is five minutes with no connected `scsh run` clients.
- **Assumed:** Session ids are six lowercase `[a-z]` letters, matching Apple-container
  nonce style.
- **Assumed:** The daemon is best-effort — if it cannot start, `scsh run` still proceeds
  without the browser URL.

## Known limitation: state file growth

The daemon retains up to 200 sessions, each proc keeping up to 5000 output lines, and it
serializes the **entire** store on every dirty WebSocket tick and state persist — while
holding the store lock. After many heavy runs (e.g. review fleets) the state JSON can
reach tens of megabytes, at which point event POSTs from `scsh run` start timing out and
new runs print *"daemon is up but registration failed"* (the run itself still works; it
just doesn't appear in the browser). Observed in practice at a ~67 MB state file.

Workaround until the daemon bounds its state — reset it:

```console
scsh daemon stop
rm "$TMPDIR/scsh-daemon/daemon-${SCSH_DAEMON_PORT:-7274}.json"
scsh daemon start
```

This clears session history only; `.cast` recordings live in each repo's `tmp/casts/`
and are unaffected.

## Demo

```console
./scripts/daemon-demo.sh
```

Or manually:

```console
cargo build --release
./target/release/scsh daemon start
./target/release/scsh daemon status
# open http://127.0.0.1:7274/ after a scsh run
./target/release/scsh daemon stop
```

## Tests

```console
cargo test
```

Integration tests cover `daemon start` / `status` / `restart` / `stop` on localhost. Unit tests cover
the event model, JSON roundtrip, and session id format.

## Manual verification (`scsh run` → browser)

Automated tests do not drive a full attended `scsh run` with browser attach. From the
**`scsh` repo root** after `cargo build`, capture the binary you just built:

```console
export SCSH_BIN="$PWD/target/debug/scsh"
```

The steps below use `$SCSH_BIN` so they work after `cd` into a scratch directory.

1. `$SCSH_BIN daemon stop` (clean slate) then `$SCSH_BIN daemon start`.
2. In a **fresh scratch directory**, scaffold a demo project: `$SCSH_BIN init-demo-project`
   (or use any git repo that already has a `.scsh.yml` with a short profile). Then run
   `$SCSH_BIN run` in that directory and note the session URL printed on stderr
   (or open `http://127.0.0.1:7274/`).
3. Confirm the browser shows the session, proc rows appear as skills run, harness
   output streams into the proc panel, and proc status updates to ✓/✗ on finish.
4. When the run ends, confirm the session moves to “ended” on the index page.
5. `$SCSH_BIN daemon restart` — daemon comes back and `GET /` still serves the index page.
6. `$SCSH_BIN daemon stop` — daemon exits and the port is closed.

For ephemeral mode, skip step 1: a short `$SCSH_BIN run` alone should spawn the
daemon, attach, and shut it down after the run disconnects and the idle timeout elapses.
If idle shutdown does not run, use `$SCSH_BIN daemon stop` as cleanup.

7. Remove the scratch directory and any daemon artifacts under the system temp dir, for example:

   ```console
   rm -rf "$SCRATCH_DIR"
   rm -f "$TMPDIR/scsh-daemon/daemon-${SCSH_DAEMON_PORT:-7274}.json" \
         "$TMPDIR/scsh-daemon/daemon-${SCSH_DAEMON_PORT:-7274}.pid"
   ```
