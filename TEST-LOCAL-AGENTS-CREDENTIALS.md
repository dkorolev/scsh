# TEST-LOCAL-AGENTS-CREDENTIALS.md — prove a locally-built `scsh` reaches every authenticated model

This file **is** the test. It is an executable Markdown harness (per the repo's testing
principle, same shape as [`HARNESS-SMOKE.md`](HARNESS-SMOKE.md)): an agent — or a human —
runs the numbered steps in order and checks each **Predict** line. There is no runner
script; the Markdown is the harness.

It proves that an `scsh` built **from this repo** can, and actually does, reach every
model the host is authenticated for, across the four **subscription-first** agents —
Claude Code (claude), Codex CLI (codex), cursor-agent (cursor), and Grok Build (grok) —
via the `local-agents-credentials` profile in [`.scsh.yml`](.scsh.yml). The per-route
workload is the existing minimal skill
[`.skills/harness-smoke/SKILL.md`](.skills/harness-smoke/SKILL.md): each route runs its
real harness in a container and must write a tiny JSON `{"result":{"status":"OK",…}}`
file. `scsh` probes host auth first and **skips** any route whose harness is
unavailable, so the run succeeds as long as every *available* route does.

The four routes (invocation → result file under the repo's gitignored `tmp/`):

| Route | Harness · model | Result file | Availability |
| --- | --- | --- | --- |
| `harness-smoke-creds-claude-opus-4-8` | claude · claude-opus-4-8 | `tmp/harness-smoke-creds-claude-opus-4-8.json` | needs claude auth |
| `harness-smoke-creds-codex-gpt-5.5` | codex · gpt-5.5 | `tmp/harness-smoke-creds-codex-gpt-5.5.json` | needs codex auth |
| `harness-smoke-creds-cursor-composer-fast` | cursor · composer-2.5-fast | `tmp/harness-smoke-creds-cursor-composer-fast.json` | needs cursor auth |
| `harness-smoke-creds-grok-build` | grok · grok-build | `tmp/harness-smoke-creds-grok-build.json` | needs grok auth |

## Prerequisites

1. **A container runtime** — docker, podman, or Apple `container` (macOS), engine running.
2. **Host credentials** for at least one harness, per [`HUMAN-CONFIG.md`](HUMAN-CONFIG.md)
   — that file is the authoritative install-and-login checklist, including the
   one-line-per-tool quick verification table. Missing credentials mean a *skipped* route,
   not a failed run.
3. **`jq`** (optional) — validates the result JSON; without it, check the files by eye.
4. **A cursor login** (optional but recommended) — `scsh` auto-annotates each run's
   recordings *after* the run using `cursor-agent` on the Composer model; the
   `.chapters.json` checks in step 6 depend on it and are skipped-not-failed without it.

## The harness (run these in order)

Run from the repo root. `SCSH` = **this repo's own build** (`./target/release/scsh`),
not any older `scsh` on `PATH` — an older binary may not know these harnesses or this
profile.

1. **Build.** `cargo build --release`
   - **Predict:** exit 0 with zero compiler warnings; `./target/release/scsh` exists.

2. **Clean tree.** `git status --porcelain` — commit or stash first if dirty.
   - **Predict:** empty output. (`scsh run` clones committed state only.)

3. **Profile exists.** `$SCSH check-profile local-agents-credentials`
   - **Predict:** exit 0, prints `profile 'local-agents-credentials' has 4 skills`.

4. **Run** (kept run dirs help post-mortem).
   `SCSH_KEEP_RUNS=1 $SCSH run --profile local-agents-credentials`
   - **Predict:** exit 0. Each *available* route prints
     `✓ <harness>: harness-smoke-creds-<route>`; each unavailable one prints a
     `skipping 'harness-smoke-creds-<route>' — …` warning up front and does **not** fail
     the run. The final line is the session deep link
     (`http://127.0.0.1:7274/session/<id>`) — note the `<id>` for step 7. The run fails
     only if *every* route was skipped (a credential-less host — fix per
     [`HUMAN-CONFIG.md`](HUMAN-CONFIG.md)).

5. **Validate results.** For each of the four `tmp/harness-smoke-creds-<route>.json` files
   that exists: `jq -r .result.status <file>`
   - **Predict:** every present file reads `OK`; a file is present for exactly the routes
     that were not skipped in step 4; **at least one** file is present.

6. **Casts recorded, chapters annotated.** `ls -t ~/.scsh/sessions/*/casts/harness-smoke-creds-*.cast`
   and, for each fresh cast, check its sidecar: `ls <cast-basename>.chapters.json`
   (e.g. `~/.scsh/sessions/<session>/casts/harness-smoke-creds-codex-gpt-5.5-<stamp>-utc-<nonce>.chapters.json`).
   - **Predict:** one fresh `<route>-<YYYYMMDD-HHMMSS>-utc-<nonce>.cast` per succeeded
     route. **If cursor/Composer is available on the host**, the run's tail printed
     `scsh: annotating N cast(s) with cursor · composer-2.5-fast …` and every fresh cast
     has a `.chapters.json` sidecar (a one-sentence summary plus 3–8 timestamped
     chapters). Without cursor, annotation is a silent no-op — mark the sidecar checks
     **SKIPPED**, not failed.

7. **Session browser.** `$SCSH daemon start` (skip if `$SCSH daemon status` says one is
   already listening), then open the step-4 deep link `http://127.0.0.1:7274/session/<id>`.
   - **Predict:** the session page lists the run with one collapsible section per route;
     each succeeded route's terminal recording is **playable**, and annotated casts show
     their **chapter markers** (from the `.chapters.json` sidecars). Stop the daemon
     afterwards with `$SCSH daemon stop` if you started it here.

## Pass / fail

Per route, using step-4/5/6 evidence:

| Route verdict | When |
| --- | --- |
| **PASS** | Ran, printed `✓`, result JSON present with `result.status == "OK"`, fresh cast present |
| **SKIPPED** | `scsh` skipped it up front (host credential absent) — not a failure |
| **FAIL** | Ran but exited non-zero, wrote no result, wrote a non-`OK` result, or left no cast |

Overall:

| Check | PASS when |
| --- | --- |
| Preflight | Clean tree; `check-profile local-agents-credentials` exits 0; a runtime is up |
| Run | Step 4 exits 0 (skipped routes don't fail it) |
| Routes | Every non-skipped route is a per-route **PASS**, and ≥1 route is non-skipped |
| Chapters | Every fresh cast has a `.chapters.json` sidecar — or cursor is unavailable (then SKIPPED) |
| Browser | The session shows playable casts (with chapter markers when annotated) |

**Overall PASS** = all five rows pass. On failure, inspect the kept run dir
(`SCSH_KEEP_RUNS=1` prints the path) and its `tmp/scsh-run.log`, or the persisted log in
`tmp/logs/<stem>.log`. A route that probed available but failed with an auth error at run
time (e.g. `Token refresh failed: 401`) means a stale OAuth token — re-run that tool's
login command per the gotcha note in [`HUMAN-CONFIG.md`](HUMAN-CONFIG.md).

## For agents

Execute the numbered steps above and report the per-route verdict table
(**PASS** / **SKIPPED** / **FAIL** with one line of evidence each) plus the overall
verdict. Run everything in the foreground with full output visible — never pipe `scsh` or
`cargo` through `tail` (see [`CONTRIBUTING.md`](CONTRIBUTING.md)).
