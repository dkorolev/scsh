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
last line printed is the permanent URL:

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

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `SCSH_DAEMON_PORT` | `7274` | HTTP listen port (localhost only) |

State and PID files live under the **system temp dir**: `$TMPDIR/scsh-daemon/` (not the
repo's gitignored `tmp/`).

## API (for scripts)

- `GET /` — HTML session index
- `GET /session/{id}` — HTML session detail
- `GET /api/v1/sessions` — JSON session id list
- `GET /api/v1/session/{id}` — JSON session detail
- `POST /api/v1/session/start`, `/register`, `/deregister`, `/ping`, `/proc/*`, `/container`
  — event ingestion (used by `scsh run`)

## Assumptions

- **Assumed:** Port 7274 is acceptable as the default (`scsh` keypad mnemonic); override
  with `SCSH_DAEMON_PORT`.
- **Assumed:** Localhost-only binding is sufficient — no auth layer on the HTTP server.
- **Assumed:** Ephemeral idle timeout is five minutes with no connected `scsh run` clients.
- **Assumed:** Session ids are six lowercase `[a-z]` letters, matching Apple-container
  nonce style.
- **Assumed:** The daemon is best-effort — if it cannot start, `scsh run` still proceeds
  without the browser URL.

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
**scsh repo root** after `cargo build`, capture the binary you just built:

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
