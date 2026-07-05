# HUMAN-CONFIG.md — what a human installs and authenticates on the host

`scsh` never performs a login itself. Each harness runs inside an ephemeral container, and
`scsh` **forwards credentials that already exist on the host** — so a human has to install
each CLI on the host once and complete its login flow once. Before a run, `scsh` probes the
host for each harness's credentials (the exact probes are implemented in `src/runtime.rs`)
and **skips** any harness that is not authenticated; the run fails only when *every*
selected skill is skipped.

This file is the per-tool checklist: what to install, how to log in, what artifact the
login leaves behind, and precisely what `scsh` looks for.

---

## claude — Claude Code CLI

- **Install:** `npm install -g @anthropic-ai/claude-code`
- **Log in:** run `claude` once and complete the OAuth login in the browser. For a
  long-lived headless token instead, run `claude setup-token` and export the result as
  `CLAUDE_CODE_OAUTH_TOKEN`.
- **Artifact produced:** on macOS, a login-keychain item named **`Claude Code-credentials`**
  whose value is the *full JSON* credentials blob (the `claudeAiOauth` object:
  `accessToken` + `refreshToken` + `expiresAt` + `scopes`). On Linux, the same JSON at
  `~/.claude/.credentials.json`.
- **What `scsh` probes** (`claude_container_auth_ready()` in `src/runtime.rs`), in order:
  1. Non-empty `CLAUDE_CODE_OAUTH_TOKEN` in the environment.
  2. `~/.claude/.credentials.json` on disk.
  3. The macOS login-keychain item `Claude Code-credentials` — accepted only when it
     contains `claudeAiOauth`, i.e. the full JSON blob. A partial credential (an access
     token without `expiresAt`/`scopes`) is treated as **logged-out** by the interactive
     TUI, which is why `scsh` forwards the entire blob and rejects anything less.

## codex — Codex CLI

- **Install:** `npm install -g @openai/codex`
- **Log in:** `codex login` — pick the ChatGPT-plan browser flow, or the API-key flow
  (`codex login --api-key`). Alternatively, skip the file entirely and export
  `OPENAI_API_KEY` in your shell.
- **Artifact produced:** `$CODEX_HOME/auth.json` (default `~/.codex/auth.json`).
- **What `scsh` probes** (`codex_container_auth_ready()`):
  1. `auth.json` under `$CODEX_HOME` when that variable is set and non-empty, else under
     `~/.codex`.
  2. Otherwise, a non-empty `OPENAI_API_KEY` in the environment.

## cursor-agent — Cursor CLI

- **Install:** `curl https://cursor.com/install -fsS | bash` (the official Cursor Agent
  CLI installer; inside containers `scsh` fetches the same package from
  `downloads.cursor.com`).
- **Log in:** `cursor agent login` (browser OAuth). Alternatively create an API key in the
  Cursor Dashboard → API Keys and export it as `CURSOR_API_KEY`.
- **Artifact produced:** on macOS, two login-keychain items — **`cursor-access-token`**
  and **`cursor-refresh-token`**. On Linux (typical), an `auth.json` at
  `~/.config/cursor/auth.json` or `~/.cursor/auth.json`.
- **What `scsh` probes** (`cursor_container_auth_ready()`):
  1. Non-empty `CURSOR_API_KEY` in the environment.
  2. `~/.config/cursor/auth.json`, then `~/.cursor/auth.json`.
  3. The macOS login-keychain item `cursor-access-token` (the refresh token is forwarded
     too when present).

  Note: a working cursor login also unlocks `scsh`'s **automatic cast annotation** — after
  each run, `scsh` uses `cursor-agent` on the Composer model to write `.chapters.json`
  sidecars next to the recordings (a no-op when cursor is unavailable).

## grok — Grok CLI (Grok Build)

- **Install:** `npm install -g @xai-official/grok` (the npm distribution of Grok Build).
- **Log in:** `grok login` (browser OIDC), or `grok login --device-auth` for the device-code
  flow on a browserless host. Alternatively create an API key at
  [console.x.ai](https://console.x.ai) and export it as `XAI_API_KEY`.
- **Artifact produced:** `$GROK_HOME/auth.json` (default `~/.grok/auth.json`).
- **What `scsh` probes** (`grok_container_auth_ready()`):
  1. `auth.json` under `$GROK_HOME` when that variable is set and non-empty, else under
     `~/.grok`.
  2. Otherwise, a non-empty `XAI_API_KEY` in the environment.

## gemini — Gemini CLI (not yet an `scsh` harness)

> **Honest note:** `gemini` is **not** an `scsh` harness on this branch — the harness set
> in `src/config.rs` is exactly `opencode`, `claude`, `codex`, `grok`, and `cursor`, and
> `scsh` probes nothing Gemini-related. This section is groundwork only: authenticate now
> and a future `gemini` harness can forward the same artifacts.

- **Install:** `npm install -g @google/gemini-cli` (or `brew install gemini-cli`).
- **Log in:** run `gemini` once and complete the Google OAuth login in the browser, or
  export `GEMINI_API_KEY` (from Google AI Studio) for headless use.
- **Artifact produced:** OAuth credentials cached under `~/.gemini/` (or the exported
  `GEMINI_API_KEY`).
- **What `scsh` probes:** nothing, today. `scsh run` will not select, skip, or mention a
  gemini route until a harness exists.

## opencode — optional, for GLM and other routed models

> **This whole section is optional.** The `opencode` harness only matters when a skill
> routes through it — e.g. to reach GLM or another third-party model that none of the
> native CLIs serve.

- **Install:** `npm install -g opencode-ai`
- **Log in:** `opencode auth login` — pick the provider (OpenAI, or a third-party provider
  such as Zhipu or Nebius for GLM) and paste its key.
- **Artifact produced:** `~/.local/share/opencode/auth.json` (honors `$XDG_DATA_HOME`:
  `$XDG_DATA_HOME/opencode/auth.json` when set). Custom providers additionally need an
  opencode config at `~/.config/opencode/opencode.json` (or `.jsonc`; honors
  `$XDG_CONFIG_HOME`) declaring the provider — `auth.json` alone is not enough for them.
  `scsh` forwards both into the container when present.
- **What `scsh` probes** (`opencode_auth_ready()`): the `auth.json` above exists. For
  skills that pin an explicit opencode model (`provider/model`, e.g.
  `nebius-glm/zai-org/GLM-5.2`), `scsh` additionally runs `opencode models <provider>` on
  the host and skips the invocation when the model is not listed.
- **GLM:** GLM is reached *through* opencode via a provider that serves it (e.g. Zhipu, or
  Nebius as `nebius-glm/zai-org/GLM-5.2`). Configure the provider with
  `opencode auth login` plus a provider entry in `opencode.json`, and the model id becomes
  usable in `.scsh.yml` as `harness: opencode` + `model: <provider>/<model>`.

---

## Quick verification

One line per tool; exit 0 means `scsh` will consider that harness available.

| Tool | Host check |
| --- | --- |
| claude | `[ -n "$CLAUDE_CODE_OAUTH_TOKEN" ] \|\| test -f ~/.claude/.credentials.json \|\| security find-generic-password -s "Claude Code-credentials" -w >/dev/null 2>&1` |
| codex | `test -f "${CODEX_HOME:-$HOME/.codex}/auth.json" \|\| [ -n "$OPENAI_API_KEY" ]` |
| cursor | `[ -n "$CURSOR_API_KEY" ] \|\| test -f ~/.config/cursor/auth.json \|\| test -f ~/.cursor/auth.json \|\| security find-generic-password -s cursor-access-token -w >/dev/null 2>&1` |
| grok | `test -f "${GROK_HOME:-$HOME/.grok}/auth.json" \|\| [ -n "$XAI_API_KEY" ]` |
| gemini | *(not probed by `scsh` — no harness yet)* `[ -n "$GEMINI_API_KEY" ] \|\| ls ~/.gemini >/dev/null 2>&1` |
| opencode | `test -f "${XDG_DATA_HOME:-$HOME/.local/share}/opencode/auth.json"` |

The `security …` legs apply on macOS only; on Linux they simply fail and the file/env legs
decide. You rarely need to run these by hand: **`scsh run` preflights all of this
automatically** and prints a `skipping '<skill>' — <harness> harness unavailable …` warning
for each missing credential, running everything else.

## Known gotcha: OAuth tokens expire and rotate

Every browser-login flow above stores a **refresh token**, and refresh tokens can expire or
be rotated server-side. The symptom is a harness that probed as *available* (the artifact
exists) but fails at run time — e.g. a stale opencode OpenAI credential yields
`Token refresh failed: 401`. The fix is always the same: re-run that tool's login command
on the host (`claude`, `codex login`, `cursor agent login`, `grok login`,
`opencode auth login`) and re-run `scsh`.
