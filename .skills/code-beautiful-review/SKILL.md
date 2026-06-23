---
name: code-beautiful-review
description: Runs the scsh `code-review` profile (the reviewer fleet — up to fifteen invocations across GPT, Opus, and GLM-5.2) over the current branch, but first pins the diff base to the freshest real upstream main — so a stale or diverged local main never makes the fleet review the wrong, oversized range — unless the user names an explicit branch to diff against, which is honored verbatim. Probes three model routes first and proceeds when at least one is available. Then reads every reviewer's result, prints a one-row-per-invocation summary table (reviewer, model route, duration, rating, issue count), and clusters the pooled findings into labeled groups A/B/C/D with a two-sentence description each, asking which to go deeper on. Read-only: it never edits code, pushes, or opens a PR. Use when the user invokes code-beautiful-review, /code-beautiful-review, or asks to run the code review and group/cluster the comments.
---

# code-beautiful-review — run the review fleet, then cluster the findings

The contract:

> **Probe three model routes (GPT, Opus, GLM-5.2) and stop only when none are available; pin the review base to the freshest upstream main (or to the branch the user names); run the `code-review` fleet through scsh against it; wait for it; then turn its scattered findings into one summary table and a handful of labeled clusters — and stop there, asking the user which cluster to open. Report only; change nothing.**

## 1. Preconditions — check FIRST; if any fails, stop and say exactly how to fix it

- **scsh is installed:** `command -v scsh`. If it is missing, tell the user to install scsh (see https://github.com/dkorolev/scsh for details) and stop — do not improvise a review by hand.

- **The `code-review` profile exists and is non-empty:** run `scsh check-profile code-review` (exit 0 means the profile is present with at least one skill; this is runtime-free). If it is non-zero, tell the user to install the reviewers with `scsh installskills https://github.com/dimacurrentai/code-review-skills`, then stop.

- **This repo's `origin` is GitHub and local `main` is up to date.** These checks apply to the repository the user is in when they invoke this skill (not to a prepared clone built in step 2). The reviewer fleet diffs `origin/main..HEAD`; step 2 may repoint `main` in a scratch clone, but this checkout must already be a normal GitHub project with a fetched, current `main`:
  - **`origin` points at GitHub.** `git remote get-url origin` must be `https://github.com/…`, `git@github.com:…`, or `ssh://git@github.com/…`. Local filesystem paths and `file://` URLs fail — stop and tell the user to fix `origin` or re-clone from GitHub.
  - **Local `main` exists.** `git show-ref --verify --quiet refs/heads/main` must succeed. If missing — stop; tell the user to fetch/create `main` from the remote (for example `git fetch origin main:main`).
  - **`main` matches `origin/main`.** Run `git fetch origin` (read-only), then `git rev-parse main` must equal `git rev-parse origin/main`. If they differ — stop and tell the user to update local `main` (`git checkout main && git pull --ff-only origin main`, then return to their feature branch) before running the review.

- **At least one review model route is available.** Before spending time on base-pinning or a fleet run, probe the three routes the profile is built for (same idea as `DEMO.md` step 1 in scsh, but for review models):

  ```sh
  [ -f ~/.zshrc ] && . ~/.zshrc 2>/dev/null || true
  REVIEW_ROUTES_AVAILABLE=0
  opencode_auth_ok() { test -f "${XDG_DATA_HOME:-$HOME/.local/share}/opencode/auth.json"; }
  opencode_model_ok() { command -v opencode >/dev/null 2>&1 && opencode models 2>/dev/null | grep -qxF "$1"; }
  claude_route_ok() { [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ] || test -f "$HOME/.claude/.credentials.json"; }

  if opencode_auth_ok && opencode_model_ok "openai/gpt-5.5"; then REVIEW_ROUTES_AVAILABLE=$((REVIEW_ROUTES_AVAILABLE + 1)); fi
  if claude_route_ok; then REVIEW_ROUTES_AVAILABLE=$((REVIEW_ROUTES_AVAILABLE + 1)); fi
  if opencode_auth_ok && opencode_model_ok "nebius-glm/zai-org/GLM-5.2"; then REVIEW_ROUTES_AVAILABLE=$((REVIEW_ROUTES_AVAILABLE + 1)); fi
  echo "review routes available: $REVIEW_ROUTES_AVAILABLE / 3"
  ```

  **Stop here** if `REVIEW_ROUTES_AVAILABLE` is `0` — no GPT, Opus, or GLM route is usable on this host. Otherwise continue; scsh will skip N/A invocations and run the rest in parallel (up to fifteen when all three routes probe ok: five reviewers × three models).

- **Let scsh enforce the rest.** Its own `run` preflight checks git, that the tree is clean, that `.scsh.yml` is valid, that `tmp/` is gitignored, and that a container runtime is up — each failure names the one fix. If `scsh run` fails preflight, surface that message verbatim and stop.

## 2. Pin the review base — the freshest upstream main, or an explicit branch

Each reviewer diffs `origin/main..HEAD` inside its own clone, and scsh builds that clone from *this* repo — so whatever this repo's local `main` points at is the baseline the whole fleet reviews against. If local `main` lags or has diverged from the real remote's main, every reviewer diffs an oversized, wrong range: the run is slow and expensive and its findings are noise. This is the failure to prevent. So fix the base deliberately before running anything. There are two modes — pick by whether the user named a branch.

- **Default — the freshest real upstream main.** Find the upstream remote exactly as `fast-beautiful-forward` does: prefer a remote named `upstream`, else `origin`; its URL MUST be a real remote (`https://`, `git@`, `ssh://`, or `git://`) — a local filesystem path or a `file://` URL does **not** qualify and must be rejected. If none qualifies, stop and ask for the repository's `org/name` (offer to add it via `gh` if `gh auth status` succeeds), then continue. Call it `<remote>`. Then `git fetch <remote>` (a fetch is a read — do it freely) and resolve its default branch — usually `main`, falling back to `master` — via `git rev-parse --abbrev-ref <remote>/HEAD` or `git remote show <remote>`. That freshest tip, `<remote>/<main>`, is the base.

- **Freshness gate (default mode only).** The branch under review must already sit on top of that freshest main: `git merge-base --is-ancestor <remote>/<main> HEAD` must succeed. If it does **not** — the branch is behind or has diverged from the freshest upstream main — **stop before running the fleet.** Do not spend a slow, expensive review on a stale base. Tell the user to run `fast-beautiful-forward` first (it replays the branch's commits onto the freshest upstream main), then re-run `code-beautiful-review`. That is the one and only fix to name; never improvise a merge or rebase here yourself.

- **Override — an explicit branch the user named.** If the user asked to review against a specific branch or ref (for example "review against `develop`", "diff against `release-2.0`"), honor it verbatim: skip the default block entirely — no remote, no fetch, no freshness gate, and never suggest `fast-beautiful-forward`. That named branch is the base, exactly as given.

**Make the fleet see that base as `origin/main`.** The reviewers take no base argument and never learn a branch name — each one always diffs `origin/main..HEAD`, full stop. So you do not tell them the base; you redefine what `origin/main` *is* in the clone they end up reviewing. scsh cannot be told a base either — it re-clones the repo it runs in, and the reviewers read `origin/main` — so you steer the base by steering what `origin/main` resolves to in what scsh clones. If this repo's local `main` already points exactly at the chosen base, just run the fleet in place (next step). Otherwise build a prepared clone and run there: into a fresh scratch dir under the gitignored `tmp/` (for example `tmp/code-beautiful-review-clone-<YYYYMMDD>-<HHMMSS>/`), `git clone` this repo — the clone checks out your branch under its own name. Then point that clone's **local** `main` branch at the base, fetching the base into the clone first if it is not already present: for the default mode, add the real `<remote>` to the clone and `git fetch <remote>`, then `git branch -f main <remote>/<main>`; for the override, the named branch arrived as `origin/<branch>` (its `origin` is this repo), so `git branch -f main origin/<branch>`. Leave your branch checked out as HEAD. When scsh re-clones this prepared dir, a clone surfaces the source's local `main` as the new clone's `origin/main`, so the reviewers' `origin/main..HEAD` becomes exactly base..your-branch. The prepared clone is throwaway scratch — this repo's own working tree and refs are never touched.

## 3. Run the fleet and wait

- Run in the directory you settled on in step 2 — this repo if local `main` already was the base, otherwise the prepared clone. Everything below happens there.

- Record the starting point first: `git rev-parse HEAD` (so you can spot anything the run adds), and note the wall-clock start.

- Run the reviewers and wait for completion, keeping the per-skill run dirs so you can time each one, and teeing the output to your own scratch dir: `SCSH_KEEP_RUNS=1 scsh run code-review 2>&1 | tee tmp/code-beautiful-review-<YYYYMMDD>-<HHMMSS>-<rand>/run.out`. scsh runs every configured invocation in parallel (up to fifteen), each in its own ephemeral container on a clean clone of the branch, diffed against the base you pinned in step 2. Unavailable harnesses or models print `⚠ skipping …` and are not run.

- Do **not** abort if `scsh run` exits non-zero. Every configured invocation is attempted; a non-zero exit means at least one reviewer failed to produce its result, but the others' results are still there. Collect what exists and mark the rest `FAILED` or `SKIPPED` in the table.

- **Success for this skill:** you probed at least one model route in step 1, and after the run at least one reviewer invocation produced its result JSON. That is enough — you do not need all fifteen. If every invocation was skipped or failed, say so plainly and stop before clustering.

## 4. Collect the output

- The authoritative output is each reviewer's **result JSON**, which scsh copies back into the run directory on success (any prior file moved aside to `*.bak.<utc>`). Get the full invocation list and each declared `result` path from `scsh list` (or `.scsh.yml`) for the `code-review` profile — by convention `tmp/code-review-<invocation>.json` (for example `tmp/code-review-conventions-reviewer-opencode-gpt.json`, `tmp/code-review-sanity-reviewer-claude-opus.json`, …), relative to wherever the fleet ran (this repo, or the prepared clone). Each file has the shape `{ result: { grade, issues_found }, issues: [ { commit, file, line, description, suggestion } ] }`, where `grade` is one of `excellent | good | average | poor` and `issues_found` equals `issues.length`.

- If the reviewers are configured for commit delivery (`commits: true` in `.scsh.yml`), the same findings also land as new commits authored by the dedicated review account — but only on the branch in whichever directory the fleet ran. When the fleet ran in place that is your branch (`git log <starting-HEAD>..HEAD` by that author); when it ran in the prepared clone those commits stay in the throwaway clone and never reach your branch, so rely on the JSON. Treat the JSON as the source of truth either way.

- **Per-reviewer duration:** from the kept run dirs (`SCSH_KEEP_RUNS=1` left them in the system temp dir as `scsh-*-run-<skill>/`), read each `tmp/scsh-run.log` and take its first-to-last timestamp. If a log is unavailable, report the overall wall-clock for the run and mark that reviewer's cell `n/a (parallel)`. Remove the kept run dirs once you have read them, and delete the prepared clone (if you made one) once you have collected its results.

## 5. The summary table — one row per invocation

Print a table with exactly these columns, one row per skill invocation in the `code-review` profile (up to fifteen when all three model routes are available):

| Reviewer | Model route | Duration | Rating | Issues |

- **Reviewer** — the base reviewer (`conventions-reviewer`, `justification-reviewer`, …), parsed from the invocation name before the `-opencode-gpt`, `-claude-opus`, or `-opencode-glm-5.2` suffix.

- **Model route** — `GPT` (`…-opencode-gpt`), `Opus` (`…-claude-opus`), or `GLM-5.2` (`…-opencode-glm-5.2`).

- **Duration** — its wall-clock from step 4.

- **Rating** — `result.grade` from its JSON.

- **Issues** — `result.issues_found` from its JSON.

A row whose harness was skipped by scsh is `SKIPPED` — say so rather than guessing. A row that ran but whose result file is absent is `FAILED`.

After the full table, print a short **per-reviewer rollup** for the five base reviewers: for each, list how many model routes ran, the issue counts per route, and the strictest grade seen across routes.

Write the full summary — the route probe line from step 1, this table, the per-reviewer rollup, and the lettered clusters from the next step — to `tmp/code-beautiful-review.md` in **this** repo (not the prepared clone, which is about to be deleted), so the run leaves a persistent report where the user will look for it. State at the top of the report which base the review used: the `<remote>/<main>` tip and its SHA, or the explicit branch the user named.

## 6. Cluster the findings

- Pool every issue from every successful invocation into one list. When tagging an issue for clustering, record which **invocation** raised it (reviewer + model route). Group similar ones into clusters — by shared file and line region, by shared root cause or theme, and by the same problem raised by more than one reviewer or model route (overlap across GPT, Opus, and GLM is expected; collapse those into a single cluster rather than listing each). Label the clusters `A`, `B`, `C`, `D`, and so on.

- For each cluster, give **exactly two sentences**: the first says what the cluster is, the second says where and how it shows up (which files, which reviewers and model routes raised it). Then ask the user which cluster they want to go deeper on — and stop. You do not fix, stage, or resolve anything; this skill reports, and a human decides.

## Safety and scope

- Read and report only. Fetching from the remote is a read; the prepared clone and all run scratch are throwaway under the gitignored `tmp/`; the scsh run is a local, sandboxed read of the branch. You take no outward action — never edit code, never commit findings, never push, never open or comment on a PR. This repo's own working tree and refs are left exactly as you found them.
