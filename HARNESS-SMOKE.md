# HARNESS-SMOKE.md — confirm grok and cursor harnesses work

This is a short, copy-pasteable smoke test for the **grok** and **cursor** container harnesses. It uses the markdown skill [`.skills/harness-smoke/SKILL.md`](.skills/harness-smoke/SKILL.md): each harness reads that file and must write a tiny JSON `{"result":{"status":"OK",…}}` file. No scripts inside the skill — the harness itself is under test.

> **One-liner:** from the `scsh` repo root, with a **clean git tree** and grok/cursor auth configured:
>
> ```sh
> ./scripts/harness-smoke.sh
> ```

## What you'll see

- **Probe** — checks host auth for grok (`~/.grok/auth.json` or `XAI_API_KEY`) and cursor (`~/.cursor/cli-config.json` or `CURSOR_API_KEY`).
- **Build** — `scsh-base` first, then `scsh-grok` / `scsh-cursor` if images are stale (first run can take several minutes).
- **Run** — up to two parallel container skills:
  - `harness-smoke-grok-build` → `tmp/harness-smoke-grok-build.json`
  - `harness-smoke-cursor-composer` → `tmp/harness-smoke-cursor-composer.json`
- **Validate** — script checks each expected result file for `"status": "OK"`.

Routes that probe **N/A** are skipped by `scsh run` (same as `DEMO.md`). You need **at least one** of grok or cursor available.

## Prerequisites

1. **Built `scsh`** — on `PATH`, or `./target/debug/scsh` / `./target/release/scsh` from this repo.
2. **Container runtime** — docker, podman, or Apple `container` (macOS), engine running.
3. **Clean git tree** — `scsh run` clones committed state only; commit or stash local edits first.
4. **`/tmp` gitignored** — already true in this repo.
5. **Auth (at least one harness):**
   - **Grok:** `grok login` (writes `~/.grok/auth.json`) **or** `export XAI_API_KEY=…`
   - **Cursor:** `cursor agent login` (writes `~/.cursor/cli-config.json`) **or** `export CURSOR_API_KEY=…`

Optional: `jq` for JSON validation in the script (without it, the script only checks that files exist).

## Run it

```sh
cd /path/to/scsh
git status --porcelain    # must be empty
cargo build               # if scsh is not on PATH yet
./scripts/harness-smoke.sh
```

Or step through manually:

```sh
scsh check-profile harness-smoke
SCSH_KEEP_RUNS=1 scsh run --profile harness-smoke
jq .result.status tmp/harness-smoke-grok-build.json
jq .result.status tmp/harness-smoke-cursor-composer.json
```

## Pass / fail

| Check | PASS when |
| --- | --- |
| Probe | At least one of grok / cursor routes is ok |
| Preflight | Clean tree, profile exists, runtime up |
| Run | `scsh run --profile harness-smoke` exits 0 (skipped routes don't fail the run) |
| Results | Each probed-ok route has `tmp/harness-smoke-<route>.json` with `"result":{"status":"OK",…}` |

On failure, inspect the kept run dir (`SCSH_KEEP_RUNS=1`) — scsh prints paths — and read `tmp/scsh-run.log` inside it.

## Skill contract (for agents)

If an agent runs this test, it should execute `./scripts/harness-smoke.sh` (or the manual steps above) and report **PASS** or **FAIL** per route. The skill under test is intentionally minimal: [.skills/harness-smoke/SKILL.md](.skills/harness-smoke/SKILL.md).
