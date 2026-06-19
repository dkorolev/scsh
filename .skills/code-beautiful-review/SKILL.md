---
name: code-beautiful-review
description: Runs the scsh `code-review` profile (the reviewer fleet) over the current branch, but first pins the diff base to the freshest real upstream main — so a stale or diverged local main never makes the fleet review the wrong, oversized range — unless the user names an explicit branch to diff against, which is honored verbatim. Then reads every reviewer's result, prints a one-row-per-reviewer summary table (reviewer, duration, rating, issue count), and clusters the pooled findings into labeled groups A/B/C/D with a two-sentence description each, asking which to go deeper on. Read-only: it never edits code, pushes, or opens a PR. Use when the user invokes code-beautiful-review, /code-beautiful-review, or asks to run the code review and group/cluster the comments.
---

# code-beautiful-review — run the review fleet, then cluster the findings

The contract:

> **Pin the review base to the freshest upstream main (or to the branch the user names), run the `code-review` reviewer fleet through scsh against it, wait for it, then turn its scattered findings into one summary table and a handful of labeled clusters — and stop there, asking the user which cluster to open. Report only; change nothing.**

## 1. Preconditions — check FIRST; if any fails, stop and say exactly how to fix it

- **scsh is installed:** `command -v scsh`. If it is missing, tell the user to install scsh (see https://github.com/dkorolev/scsh for details) and stop — do not improvise a review by hand.

- **The `code-review` profile exists and is non-empty:** run `scsh check-profile code-review` (exit 0 means the profile is present with at least one skill; this is runtime-free). If it is non-zero, tell the user to install the reviewers with `scsh installskills https://github.com/dkorolev/code-review-skills`, then stop.

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

- Run the reviewers and wait for completion, keeping the per-skill run dirs so you can time each one, and teeing the output to your own scratch dir: `SCSH_KEEP_RUNS=1 scsh run code-review 2>&1 | tee tmp/code-beautiful-review-<YYYYMMDD>-<HHMMSS>-<rand>/run.out`. scsh runs every reviewer in parallel, each in its own ephemeral container on a clean clone of the branch, diffed against the base you pinned in step 2.

- Do **not** abort if `scsh run` exits non-zero. Every skill runs regardless; a non-zero exit means at least one reviewer failed to produce its result, but the others' results are still there. Collect what exists and mark the rest FAILED in the table.

## 4. Collect the output

- The authoritative output is each reviewer's **result JSON**, which scsh copies back into the run directory on success (any prior file moved aside to `*.bak.<utc>`). Get the reviewer list and each declared `result` path from `scsh list` (or `.scsh.yml`) for the `code-review` profile — by convention `tmp/code-review-<skill>.json`, relative to wherever the fleet ran (this repo, or the prepared clone). Each file has the shape `{ result: { grade, issues_found }, issues: [ { commit, file, line, description, suggestion } ] }`, where `grade` is one of `excellent | good | average | poor` and `issues_found` equals `issues.length`.

- If the reviewers are configured for commit delivery (`commits: true` in `.scsh.yml`), the same findings also land as new commits authored by the dedicated review account — but only on the branch in whichever directory the fleet ran. When the fleet ran in place that is your branch (`git log <starting-HEAD>..HEAD` by that author); when it ran in the prepared clone those commits stay in the throwaway clone and never reach your branch, so rely on the JSON. Treat the JSON as the source of truth either way.

- **Per-reviewer duration:** from the kept run dirs (`SCSH_KEEP_RUNS=1` left them in the system temp dir as `scsh-*-run-<skill>/`), read each `tmp/scsh-run.log` and take its first-to-last timestamp. If a log is unavailable, report the overall wall-clock for the run and mark that reviewer's cell `n/a (parallel)`. Remove the kept run dirs once you have read them, and delete the prepared clone (if you made one) once you have collected its results.

## 5. The summary table — one row per reviewer

Print a table with exactly these columns, one row per skill in the `code-review` profile:

| Reviewer | Duration | Rating | Issues |

- **Reviewer** — the skill name.

- **Duration** — its wall-clock from step 4.

- **Rating** — `result.grade` from its JSON.

- **Issues** — `result.issues_found` from its JSON.

A reviewer whose result file is absent is `FAILED` — say so in its row rather than guessing.

Write the full summary — this table plus the lettered clusters from the next step — to `tmp/code-beautiful-review.md` in **this** repo (not the prepared clone, which is about to be deleted), so the run leaves a persistent report where the user will look for it. State at the top of the report which base the review used: the `<remote>/<main>` tip and its SHA, or the explicit branch the user named.

## 6. Cluster the findings

- Pool every issue from every reviewer into one list. Group similar ones into clusters — by shared file and line region, by shared root cause or theme, and by the same problem raised by more than one reviewer (overlap is expected here; collapse those duplicates into a single cluster rather than listing each). Label the clusters `A`, `B`, `C`, `D`, and so on.

- For each cluster, give **exactly two sentences**: the first says what the cluster is, the second says where and how it shows up (which files, which reviewers raised it). Then ask the user which cluster they want to go deeper on — and stop. You do not fix, stage, or resolve anything; this skill reports, and a human decides.

## Safety and scope

- Read and report only. Fetching from the remote is a read; the prepared clone and all run scratch are throwaway under the gitignored `tmp/`; the scsh run is a local, sandboxed read of the branch. You take no outward action — never edit code, never commit findings, never push, never open or comment on a PR. This repo's own working tree and refs are left exactly as you found them.
