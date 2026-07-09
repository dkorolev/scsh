# Assumptions — Claude Code harness (v1)

Documented decisions made while adding `harness: claude` alongside `harness: opencode`.
Review these before merging or extending.

## Schema

- **Skill key == folder name** — each `.scsh.yml` key must match `.skills/<name>/`. The legacy `skill:` pointer is rejected.
- **Direct run or matrix** — declare `harness` (+ optional `model`, …) for a single invocation named after the key, *or* an `invocations:` map where each route expands to `{skill}-{route}` at run time.
- **Demo config** ships two matrix skills (`add`, `multiply`) with two routes each. Models: **`openai/gpt-5.4-mini-fast`** and **`sonnet`**.
- **`commits: true` only on the `add` → `opencode-gpt-5.4-mini-fast` route** — avoids duplicate git commits when several add routes run in parallel.
- **`profile:`** can be set per skill; each `invocations:` row may override it.

## Invocation

- **Claude uses prompt model (B):** `claude -p "Run .skills/<source>/SKILL.md …"` with `--permission-mode bypassPermissions` and `--no-session-persistence`. Not slash-command `/add` (headless skill bugs).
- **OpenCode is a recorded interactive TUI:** `opencode -m <model> --prompt "Run the skill defined in .skills/<source>/…"`, submitted with Enter once the TUI is up (not the old headless `opencode run`).

## Images

- **Two final images, one Dockerfile:** shared `scsh-base`, then `scsh-opencode` and `scsh-claude` targets. Harness CLI installed last in each stage.
- Tags: `scsh-opencode:latest`, `scsh-claude:latest`. scsh builds only images needed by the selected skills, **in parallel**, and **skips** a build when the tag already carries a matching `scsh.build.fingerprint` label (sha256 of the embedded Dockerfile + target + uid/gid/tz).

## Auth

- **OpenCode:** copy `auth.json` into run dir (existing behavior); only for opencode skills.
- **Claude:** forward host `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) and/or copy `~/.claude/.credentials.json` plus optional `~/.claude` / `~/.claude.json` into the run dir and bind-mount into the container.
- **Preflight:** `scsh run` **skips** skills whose harness is unavailable or whose explicit opencode `model:` is not listed by host `opencode models`; it **fails** only when every selected skill is skipped. Claude needs `CLAUDE_CODE_OAUTH_TOKEN` or `~/.claude/.credentials.json` (macOS Keychain login alone is not enough for Linux containers). OpenCode model probing runs only for **selected** invocations (profile-scoped, not every route in `.scsh.yml`), calling `opencode models <provider>` per distinct provider among those models — not a full unfiltered `opencode models` sweep.

## Results

- **`SCSH_RESULT` env var** injected into every container with the invocation's `result:` path so one skill folder serves multiple invocations with different output files.
- Demo scripts (`add.py`, `multiply.py`) honor `SCSH_RESULT`.

## Repo sync (push IN, pull OUT — never GitHub from inside the container)

scsh moves git state **only between the host and the run clone** on local disk. Containers never contact GitHub (or any remote).

**Before the container (host pushes IN):**

1. scsh prepares a run dir under `/tmp/scsh-*-run-*`.
2. **Docker / Podman / Linux:** scsh **`git clone`s on the host** from the caller's committed state, materializes `origin/*` as local branches, runs **`git fsck`**, then **bind-mounts** the clone at `/home/agent/repo`.
3. **macOS Apple Container:** scsh **`git push`es** the caller's committed state into a bare `transport.git` in the run dir (plus optional `pull.git` when `commits: true`), starts a short-lived **`git daemon`** on the host, and the container **clones** from `git://…/transport.git`. Only `run_dir/tmp` is bind-mounted — not `.git`.
4. In both paths the skill sees a complete snapshot; it must not `git fetch`, `git pull`, `git push`, or `git clone` to "refresh" it.

**After the container exits (host pulls OUT — externally, on the host):**

1. **Result file:** scsh copies the skill's declared `result` from the run clone back into the caller repo (`collect_skill_result`). Always, for every skill.
2. **Commits (optional):** only when the skill declares `commits: true` *and* the skill actually added commits in its clone (`base..clone-HEAD` non-empty). Then scsh **on the host** fetches those objects from the **local run-clone path** (not from GitHub) and cherry-picks them onto the caller's branch (`integrate_commits`). Reviewer skills do not commit; this path is for skills like demo `add`.
3. scsh never pushes to any remote.

If `origin/main` is wrong or missing in the clone, fix the **host** checkout before `scsh run` — fetching inside the container is forbidden and usually fails (the run clone's `origin` is often a filesystem path).

**`code-beautiful-review`** may `git fetch` on the **host** while pinning the review base (steps 1–2); that prepares what gets pushed into containers and does not authorize skills inside containers to fetch.

## Cache keys

- Include invocation name, skill source, harness, model, skill files, env, and repo tree hash.

## Tests / DEMO

- Integration tests for claude skills run **only when** `claude auth status` succeeds locally; otherwise marked N/A in test output.
- DEMO step 1 probes two routes (gpt-5.4-mini-fast, sonnet-4-6) via `opencode models` and claude auth; **fails** if none probe ok. Later steps note N/A when a route is missing.

## Install path

- `installskills` copies `.skills/<name>/` and merges each skill's YAML block **verbatim** (including `invocations:`). Existing consumer keys are left untouched — scsh warns on conflict.
