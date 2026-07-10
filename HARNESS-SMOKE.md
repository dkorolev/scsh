# HARNESS-SMOKE.md — executable harness for the claude, codex, and cursor containers

This file **is** the test. It is an executable Markdown harness (per the repo's testing
principle): an agent — or a human — runs the numbered steps in order and checks each
**Predict** line. `scripts/harness-smoke.sh` is a thin runner that executes exactly these
steps; running the script and following this doc are equivalent.

It exercises the **claude**, **codex**, and **cursor** container harnesses end to end via the
minimal skill [`.skills/harness-smoke/SKILL.md`](.skills/harness-smoke/SKILL.md): each harness
runs its real interactive TUI (recorded to `~/.scsh/sessions/<session>/casts/`) and must write a tiny JSON
`{"result":{"status":"OK",…}}` file. scsh itself probes host auth and **skips** any harness
that is not logged in, so the run succeeds as long as every *available* harness does.

## Prerequisites

1. **A container runtime** — docker, podman, or Apple `container` (macOS), engine running.
2. **At least one harness logged in on the host** (scsh forwards these into the container;
   missing ones are skipped, not failed):
   - **Claude** — `claude` logged in (macOS keychain), `~/.claude/.credentials.json`, or `CLAUDE_CODE_OAUTH_TOKEN`.
   - **Codex** — `~/.codex/auth.json` (`codex login`) or `OPENAI_API_KEY`.
   - **Cursor** — `cursor-agent` logged in (keychain / `~/.cursor/auth.json`) or `CURSOR_API_KEY`.
3. **`jq`** (optional) — validates the result JSON; without it the runner only checks the file exists.

## The harness (run these in order)

Run from the repo root. `SCSH` = this repo's own build (`./target/debug/scsh` or
`./target/release/scsh`), **not** any older `scsh` on `PATH` — an older binary may not know
these harnesses.

1. **Build.** `cargo build`
   - **Predict:** exit 0; `./target/debug/scsh` exists.

2. **Clean tree.** `git status --porcelain` — commit or stash first if dirty.
   - **Predict:** empty output. (`scsh run` clones committed state only.)

3. **Profile exists.** `$SCSH check-profile harness-smoke`
   - **Predict:** exit 0, prints `profile 'harness-smoke' has 3 skills`.

4. **Run** (kept run dirs help post-mortem). `SCSH_KEEP_RUNS=1 $SCSH run --profile harness-smoke`
   - **Predict:** exit 0. Each *available* harness prints `✓ <harness>: harness-smoke-<route>`.
     Unavailable harnesses print an `N/A`/skip line and do **not** fail the run.

5. **Validate results.** For each `tmp/harness-smoke-<route>.json` that exists
   (`claude-opus-4-8`, `codex-gpt-5.5`, `cursor-composer-fast`): `jq .result.status <file>`
   - **Predict:** every present file reads `"OK"`, and **at least one** file is present.

6. **Screencasts recorded.** `ls -t ~/.scsh/sessions/*/casts/harness-smoke-*.cast`
   - **Predict:** one fresh `<route>-<YYYYMMDD-HHMMSS>-utc-<nonce>.cast` per succeeded route
     (real interactive TUI; replay with `asciinema play <file>` or the session browser).

## One-command runner

```sh
cd /path/to/scsh
./scripts/harness-smoke.sh        # runs steps 1–6 and prints PASS / FAIL
```

## Pass / fail

| Check | PASS when |
| --- | --- |
| Preflight | Clean tree; `check-profile harness-smoke` exits 0; a container runtime is up |
| Run | `scsh run --profile harness-smoke` exits 0 (skipped harnesses don't fail it) |
| Results | Every present `tmp/harness-smoke-<route>.json` has `result.status == "OK"`, ≥1 present |

**Overall PASS** = all three rows pass. On failure, inspect the kept run dir (`SCSH_KEEP_RUNS=1`
prints the path) and its `tmp/scsh-run.log`, or the persisted log in `tmp/logs/<stem>.log`.

## For agents

Execute the numbered steps above (or run `./scripts/harness-smoke.sh`) and report **PASS**
or **FAIL** with the per-route result. The skill under test is intentionally minimal:
[.skills/harness-smoke/SKILL.md](.skills/harness-smoke/SKILL.md).
