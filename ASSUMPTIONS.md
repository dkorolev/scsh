# Assumptions — Claude Code harness (v1)

Documented decisions made while adding `harness: claude` alongside `harness: opencode`.
Review these before merging or extending.

## Schema

- **Optional `skill:` field** — YAML key is the *invocation name*; `skill:` points at `.skills/<name>/` (default: key). Backward compatible when key == folder name.
- **Demo config** ships five invocations: `add-opencode-gpt`, `add-claude-sonnet-4-6`, `add-opencode-glm-5.2`, `multiply-opencode-gpt`, `multiply-claude-sonnet-4-6`. Models: **`openai/gpt-5.4-mini-fast`** (gpt-5.4-mini-fast), **`sonnet`** (sonnet-4-6), **`nebius-glm/zai-org/GLM-5.2`** (glm-5.2).
- **`commits: true` only on `add-opencode-gpt`** — avoids duplicate git commits when several add routes run in parallel.

## Invocation

- **Claude uses prompt model (B):** `claude -p "Run .skills/<source>/SKILL.md …"` with `--permission-mode bypassPermissions` and `--no-session-persistence`. Not slash-command `/add` (headless skill bugs).
- **OpenCode unchanged:** `opencode run "run skill <source>"`.

## Images

- **Two final images, one Dockerfile:** shared `scsh-base`, then `scsh-opencode` and `scsh-claude` targets. Harness CLI installed last in each stage.
- Tags: `scsh-opencode:latest`, `scsh-claude:latest`. scsh builds only images needed by the selected skills.

## Auth

- **OpenCode:** copy `auth.json` into run dir (existing behavior); only for opencode skills.
- **Claude:** forward host `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) and/or copy `~/.claude/.credentials.json` plus optional `~/.claude` / `~/.claude.json` into the run dir and bind-mount into the container.
- **Preflight:** `scsh run` **skips** skills whose harness is unavailable; it **fails** only when every selected skill is skipped. Claude needs `CLAUDE_CODE_OAUTH_TOKEN` or `~/.claude/.credentials.json` (macOS Keychain login alone is not enough for Linux containers).
- OpenCode still does **not** probe the model API — file presence only (same as before).

## Results

- **`SCSH_RESULT` env var** injected into every container with the invocation's `result:` path so one skill folder serves multiple invocations with different output files.
- Demo scripts (`add.py`, `multiply.py`) honor `SCSH_RESULT`.

## Cache keys

- Include invocation name, skill source, harness, model, skill files, env, and repo tree hash.

## Tests / DEMO

- Integration tests for claude skills run **only when** `claude auth status` succeeds locally; otherwise marked N/A in test output.
- DEMO step 1 probes three routes (gpt-5.4-mini-fast, sonnet-4-6, glm-5.2) via `opencode models` and claude auth; **fails** if none probe ok. Later steps note N/A when a route is missing.

## Install path

- `installskills` copies into `.skills/<skill_source>/` when the manifest entry uses `skill:`.
