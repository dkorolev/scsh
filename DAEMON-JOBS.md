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
for `A` and `B` prefilled with `2` and `3`. The `fruits`, `code-review`, and `arith`
definitions show a **workflow** badge — `arith` is the default “watch a bundle run” demo:
every param defaulted, so it starts on any opened directory with zero setup.
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

**Expect:** every step appears in the session browser immediately — one row per step, noted
`step k/n` (plus `needs …`), waiting rows included; a step whose gate is false finishes as a
dim ⊘ `skipped` row instead of vanishing. Above the rows, a **Job graph** card shows the DAG
(`categorize` → `sort_fruits` / `sort_vegetables`); node colors track live state, and clicking a
node opens that step's panel (`#task-…`). `categorize` runs first, then `sort_fruits` and
`sort_vegetables` run in parallel;
the session board shows all three step rows. The per-step results land under the gitignored
session dir (`tmp/scsh/<session>/` or `.harness/tmp/scsh/<session>/`), `sort_fruits.json` has a
`sorted` field with the fruits in alphabetical order, and `git -C "$REPO" status` is clean.

## 10. (Optional) The three-harness bundle, with a file artifact

`arith` is the built-in cross-harness demo: **claude** (sonnet) adds A+B, **codex** (gpt-5.5)
multiplies X×Y in parallel, and **grok** (grok-4.5) folds both results into one plain-English
paragraph. Needs all three CLIs logged in on the host.

```console
cd "$REPO" && "$SCSH_BIN" run --def arith
```

**Expect:** the three harness images build FIRST, each as its own tracked row, before any step
starts; then three step rows (`step 1/3` … `step 3/3 · needs add, multiply`) with `add` and
`multiply` running in parallel and `summarize` waiting on both. The **Job graph** card shows the
fan-in DAG (`add` + `multiply` → `summarize`) from first paint. Afterwards the session dir holds
`add.json` (`sum: 5`), `multiply.json` (`product: 20`), `summarize.json` (a `summary` string) —
**and `summary.txt`**, the declared step artifact: a standalone plain-English sentence about
both computations, copied back beside the results. `git -C "$REPO" status` stays clean.

## 11. (Optional) Fake PR workflow (`greet`)

Needs `packdiff` on PATH for the ⇄ commits diff chips. One agent CLI is enough (all three
steps use claude/sonnet by default).

```console
cd "$REPO" && NAME=Ada "$SCSH_BIN" run --def greet
```

**Expect:** the **Job graph** card shows `scaffold → implement → describe`. After the run,
the branch has `greet.py` / `test_greet.py` / `PR-DESCRIPTION.md`; `NAME=Ada python3 test_greet.py`
passes; each step's row has a **⇄ commits diff** chip — open **implement** for the source
fix and **describe** for the Description panel lifted from `PR-DESCRIPTION.md`.

## 12. Job graph — interaction acceptance (manual)

Use an `arith` or `fruits` session URL from steps 9–10. Report PASS/FAIL per bullet.

### Fan-out (`fruits`)

1. Graph shows `categorize → sort_fruits` and `categorize → sort_vegetables`.
2. Both downstream nodes run in parallel without rearranging node coordinates.
3. Clicking every node opens the matching proc panel (`#task-<id>`).

### Fan-in (`arith`)

1. Graph shows `add → summarize` and `multiply → summarize` (plus any `build_*` image nodes).
2. `summarize` stays grey while prerequisites are incomplete.
3. Each root independently becomes green; `summarize` turns purple only after both finish.

### Conditional gate (`code-review`)

1. `review` shows a **when** gate marker; tooltip is generic (“Runs only when its gate passes”)
   — no raw gate literals.
2. When the gate is false, `review` becomes **Skipped**, stays visible, and remains clickable;
   its detail explains the skip.

### Failure / stall

1. Force one step to fail → node is red; other nodes and edges remain; all remain clickable.
2. Kill the run process without deregistering; after `SESSION_STALE_SECS`, incomplete nodes
   become orange **Stalled**. A healthy agent with no output stays purple **Running**.

### Navigation

1. Click three different tasks → URL fragments update; **Back** / **Forward** restore prior
   selections (open + focus + highlight).
2. Copy/reload `#task-summarize` → that panel opens.
3. Click a task before its proc row exists → status says details are not available yet; when
   the row appears, that panel opens once (no focus stealing on later ticks).

### Accessibility / responsive

1. Tab through every graph node; Enter activates; focus lands on the proc `summary`.
2. Enable reduced motion → no running pulse; scroll is instant.
3. Narrow phone width and 200% zoom: every node remains reachable via the scroll region
   (labeled “Job dependency graph”); nodes do not overlap.

### Regression

1. Flat definition / `demo-pr` / build-images sessions still show a **Job graph** when they
   have skills or image builds (builds are nodes with edges into skills).
2. Casts, results, commits diffs, Force stop, and job snapshot export still work.

## Cleanup

```console
"$SCSH_BIN" daemon stop
rm -rf "$REPO" "$NONREPO" "$SCSH_HOME" "$SCSH_HARNESS_HOME"
rm -f "$TMPDIR/scsh-daemon/daemon-7391.pid" "$TMPDIR/scsh-daemon/daemon-7391.mode"
```
