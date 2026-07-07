# Manual harness: open a repo & start a job from the daemon

This verifies the "start a job from the web UI" path end to end. Most steps need **no container
runtime or agent credentials** — they exercise the daemon's validation, discovery, parameter
form, and one-job-per-directory guard. A final optional section drives a real agent run.

Follow the steps in order and check each **Expect** line. Report PASS/FAIL per step.

## Setup

From the **`scsh` repo root** after `cargo build`, isolate all state on a test port so nothing
touches a real daemon or `~/.scsh`:

```console
export SCSH_BIN="$PWD/target/debug/scsh"
export SCSH_DAEMON_PORT=7391
export SCSH_HOME="$(mktemp -d)"           # daemon store, off to the side
export SCSH_HARNESS_HOME="$(mktemp -d)"   # empty, so only the built-in definitions appear
REPO="$(mktemp -d)"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@example.com
git -C "$REPO" config user.name t
git -C "$REPO" commit -q --allow-empty -m init
"$SCSH_BIN" daemon start
```

**Expect:** `daemon status` reports the daemon listening on `http://127.0.0.1:7391`.

## 1. Open a clean repository

```console
curl -s -X POST "localhost:7391/api/v1/repos/open" -d "{\"path\":\"$REPO\"}"
```

**Expect:** `"ok":true`, `"clean":true`, and a `defs` array that includes the three built-ins
`doctor`, `add`, and `research`. The `add` definition lists params `A` and `B` (`type":"int"`,
defaults `2`/`3`) and two agent routes (opencode, claude).

## 2. Open a non-repository

```console
NONREPO="$(mktemp -d)"
curl -s -X POST "localhost:7391/api/v1/repos/open" -d "{\"path\":\"$NONREPO\"}"
```

**Expect:** `"ok":false` and an error mentioning `not a git repository`.

## 3. The dashboard carries the panel

```console
curl -s "localhost:7391/" | grep -oE 'id="(repo-path|repo-open|defs-panel|repos-body)"|jobs by repository' | sort -u
```

**Expect:** all four ids and the `jobs by repository` heading are present. (In a real browser at
`http://127.0.0.1:7391/`, Open the repo, pick `add`, and confirm the param form renders inputs
for `A` and `B` prefilled with `2` and `3`. The `fruits` definition shows a **workflow** badge.
Clicking **Pick…** opens the native OS folder chooser on a machine with a display.)

## 4. Missing required parameter is rejected

`research` requires `CITY`; omit it.

```console
curl -s -X POST "localhost:7391/api/v1/jobs/start" \
  -d "{\"repo\":\"$REPO\",\"def\":\"research\",\"params\":{}}"
```

**Expect:** HTTP 400 with an error naming `CITY`; no session is created.

## 5. Start a job, then hit the one-job-per-directory guard

So this step needs no container, restart the daemon with `SCSH_BIN` pointing the job it spawns
at a no-op (`/usr/bin/true`); the shell's `$SCSH_BIN` still runs the real daemon:

```console
"$SCSH_BIN" daemon stop
SCSH_BIN=/usr/bin/true "$SCSH_BIN" daemon start   # the daemon spawns /usr/bin/true for jobs
curl -s -X POST "localhost:7391/api/v1/jobs/start" \
  -d "{\"repo\":\"$REPO\",\"def\":\"add\",\"params\":{\"A\":\"2\",\"B\":\"3\"}}"
curl -s -X POST "localhost:7391/api/v1/jobs/start" \
  -d "{\"repo\":\"$REPO\",\"def\":\"add\",\"params\":{\"A\":\"2\",\"B\":\"3\"}}"
```

**Expect:** the first call returns `"ok":true` with a `session` id; the second returns HTTP 409
with an error containing `already running in this repository`.

> Note: `SCSH_BIN` is read by the daemon to locate the binary it spawns. Setting it to
> `/usr/bin/true` for the daemon process makes "start a job" a no-op that still pre-creates the
> session, so the guard is testable without building images.

## 6. Dirty working tree is refused

```console
echo scratch > "$REPO/uncommitted.txt"
curl -s -X POST "localhost:7391/api/v1/jobs/start" \
  -d "{\"repo\":\"$REPO\",\"def\":\"add\",\"params\":{}}"
git -C "$REPO" checkout -q -- . ; rm -f "$REPO/uncommitted.txt"
```

**Expect:** HTTP 409 with an error mentioning `uncommitted changes`.

## 7. Jobs are grouped by repository

```console
curl -s "localhost:7391/api/v1/repos"
```

**Expect:** a `repos` array containing `$REPO`, with the job(s) started in step 5 listed under it.

## 8. (Optional) A real agent run

With a container runtime up and at least one agent's credentials present, start the real daemon
(no `SCSH_BIN` override) and run the console path directly:

```console
"$SCSH_BIN" daemon stop && "$SCSH_BIN" daemon start   # a real daemon, no SCSH_BIN override
cd "$REPO" && A=2 B=3 "$SCSH_BIN" run --def add
```

**Expect:** the run builds/uses the images, an agent runs `add`, `tmp/add_*.json` has `"sum": 5`,
and `git -C "$REPO" status` is clean afterward (the definition body never dirties the tree). For
`scsh run --def doctor`, expect a preflight report of each agent's image/credential status before
the confirm task.

## 9. (Optional) A real workflow run

```console
cd "$REPO" && WORDS="apple, carrot, pear, onion" "$SCSH_BIN" run --def fruits
```

**Expect:** `categorize` runs first, then `sort_fruits` and `sort_vegetables` run in parallel;
the session board shows all three step rows. The per-step results land under the gitignored
session dir (`tmp/scsh/<session>/` or `.harness/tmp/scsh/<session>/`), `sort_fruits.json` has a
`sorted` field with the fruits in alphabetical order, and `git -C "$REPO" status` is clean.

## Cleanup

```console
"$SCSH_BIN" daemon stop
rm -rf "$REPO" "$NONREPO" "$SCSH_HOME" "$SCSH_HARNESS_HOME"
rm -f "$TMPDIR/scsh-daemon/daemon-7391.pid" "$TMPDIR/scsh-daemon/daemon-7391.mode"
```
