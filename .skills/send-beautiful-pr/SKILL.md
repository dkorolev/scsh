---
name: send-beautiful-pr
description: "Opens a GitHub pull request for the current branch using PR-DESCRIPTION.md as the body. Requires a clean working tree, that prepare-beautiful-pr has already committed PR-DESCRIPTION.md, that tmp/the-beautiful-loop-fixes.md is not in the branch history, that the branch does not yet exist on the remote, and explicit user approval before reauthoring commits or stripping Co-authored-by trailers. Drops the local Elon Presley notes commit from the pushed history; pushes only commits authored as the authenticated GitHub user, then creates the PR with gh. Use when the user invokes send-beautiful-pr, /send-beautiful-pr, or asks to open or send the pull request."
---

# send-beautiful-pr — fix authorship, push the branch, open the PR

The contract:

> **Audit every commit you are about to publish. Ask before rewriting history. Drop the Elon Presley notes commit from what gets pushed. Push only when authorship matches the GitHub user. Open the PR with PR-DESCRIPTION.md as the body.**

This is the step **after** `/prepare-beautiful-pr`. That skill shapes the commits and writes `PR-DESCRIPTION.md` locally; this one cleans attribution, pushes, and sends the PR.

## 1. Preconditions — check FIRST; if any fails, stop and say exactly how to fix it

- **Inside a git repository.** If not, stop.

- **Working tree clean.** No uncommitted or staged changes (`git status --porcelain` must be empty). If the tree is dirty, stop and tell the user to commit or stash first.

- **Not on the default branch.** Determine the default branch (assume `main`, fall back to `master`). If HEAD is on it, stop.

- **`PR-DESCRIPTION.md` exists at the repo root and is committed.** If it is missing from `HEAD`, stop and complain plainly:

  > `PR-DESCRIPTION.md` is missing. Run `/prepare-beautiful-pr` or `/prepare-beautiful-pr` first — those skills write and commit the PR description; this skill only sends it.

  Do not invent a description on the fly.

- **`gh` is installed and authenticated.** Run `gh auth status`. If it fails, stop.

- **At least one commit on top of the base.** Pick the base — the default branch, preferring `origin/main` when a remote exists, otherwise local `main` — and require at least one commit in `base..HEAD`. If there is nothing to PR, stop.

- **Branch does NOT exist on the remote yet.** With the current branch name `<branch>` and push remote `origin` (prefer `upstream` only when that is the repo's documented push remote; otherwise `origin`):

  ```bash
  git ls-remote --heads origin "<branch>"
  ```

  If this returns any ref, stop with a clear message:

  > Branch `<branch>` already exists on `origin`. This skill only opens a first-time PR from a branch that has never been pushed. Delete the remote branch, rename your local branch, or push manually — then stop.

  Do not force-push. Do not update an existing remote branch.

- **`tmp/the-beautiful-loop-fixes.md` must not be published.** This local fix log from `/the-beautiful-loop` must never reach the remote. If it is staged, stop and unstage it. If any commit in `base..HEAD` adds or modifies `tmp/the-beautiful-loop-fixes.md`, stop:

  > `tmp/the-beautiful-loop-fixes.md` is in the branch history. It is local review scratch and must not be pushed. Remove it from the commits (rewrite local history) or drop those commits, then re-run this skill.

  Also fail if the file exists on disk but is tracked (`git ls-files --error-unmatch tmp/the-beautiful-loop-fixes.md` exits 0) — run `git rm --cached tmp/the-beautiful-loop-fixes.md` only after asking the user, since that rewrites the index.

## 2. Resolve the GitHub pusher identity

- Read the authenticated GitHub login: `gh api user -q .login`.

- The commits that get pushed must be authored (and committed) as this human's GitHub identity. Use an identity the user explicitly supplied for this publication; otherwise use `git config user.name` (falling back to `gh api user -q .name`) and a verified email on the GitHub account.

- Confirm a configured email with `gh api user/emails --jq '.[].email'`, or accept the user's public email from `gh api user -q .email`. If the token cannot read account emails, ask the user to confirm the intended address once; their explicit answer is authoritative. Never substitute an organization-specific or project-specific address based only on the GitHub login.

- Record this pair as `<gh-name>` and `<gh-email>` — every **pushed** commit must end up with this author (and no alien identities).

## 3. Audit commits in `base..HEAD` — read only; do not rewrite yet

Save the PR body now — you will need it even if the notes commit is dropped later:

```bash
git show HEAD:PR-DESCRIPTION.md > tmp/send-beautiful-pr-body.md
```

(If `PR-DESCRIPTION.md` is not at `HEAD`, use the last commit in `base..HEAD` that contains it.)

The notes-only author from `/prepare-beautiful-pr` — **must never be pushed**:

```
NAME  = Elon Presley
EMAIL = dmitry.korolev+elon-presley@gmail.com
```

Scan every commit in `base..HEAD`. For each, record:

1. **Notes commits to drop.** Author matches Elon Presley / `dmitry.korolev+elon-presley@gmail.com` *and* the commit touches only `PR-DESCRIPTION.md` (a pure notes commit). These stay local only; they are omitted from the branch you push. The PR body still comes from the saved file above.

2. **Co-authored-by trailers to remove.** Any `Co-authored-by:` line in the commit message (any author). GitHub and reviewers must not see these on the published branch.

3. **Author mismatches to fix.** Any remaining commit whose `author name` / `author email` is not `<gh-name>` / `<gh-email>`, or whose committer identity is an agent/tool identity inconsistent with the GitHub user.

4. **Fix log must not ship.** No commit in `base..HEAD` may touch `tmp/the-beautiful-loop-fixes.md`. If any does, include it in the audit and do not push until removed from history.

Build a concrete audit list: one line per commit (`<short-sha> <subject> — <issue>`). If the audit is empty — no notes commits, no Co-authored-by lines, every author already matches `<gh-name> <gh-email>` — skip to step 5 (still do not push before step 5 checks).

## 4. Ask explicitly — rewriting requires a yes; no means abort

If step 3 found **anything** to drop, strip, or reauthor, **stop and ask the user one clear question**. Summarize exactly what you will do, for example:

- drop N Elon Presley notes commit(s) from the pushed history (PR body unchanged);
- remove `Co-authored-by:` from commits A, B, …;
- reauthor commits C, D, … to `<gh-name> <gh-email>`.

Then ask:

> May I rewrite the branch locally to fix this authorship before pushing? (yes / no)

- **If the user says no (or does not clearly say yes): abort.** Do not push. Do not open a PR. Say plainly that the mission is aborted and list what blocked it.

- **If the user says yes:** continue to step 5. **Only after this yes** may you rewrite history, push, or create the PR.

If step 3 found no issues, treat that as implicit approval to proceed — still confirm the audit is clean in the report, but do not ask unless there is something to fix.

## 5. Rewrite history — only after step 4 yes, and only on unpushed commits

- Create a backup ref first: `send-beautiful-pr-backup-<YYYYMMDD>-<HHMMSS>` at current `HEAD`.

- Replay `base..HEAD` onto a clean branch tip:

  1. **Drop** every notes-only Elon Presley commit.
  2. **Strip** every `Co-authored-by:` line from remaining commit messages.
  3. **Reauthor** every remaining commit to `<gh-name> <gh-email>` for both author and committer.

  Prefer a non-interactive replay (cherry-pick or `git rebase` with a scripted loop) over manual `-i` editing. The code tree of the pushed commits must stay byte-identical to what it was before dropping notes commits — only `PR-DESCRIPTION.md` may disappear from the pushed history. Commits must not contain `tmp/the-beautiful-loop-fixes.md`.

- Verify:

  ```bash
  git diff <backup> HEAD -- . ':(exclude)PR-DESCRIPTION.md'   # must be empty
  git log --format=%B <base>..HEAD | grep -i '^Co-authored-by:'  # must find nothing
  git log --name-only --format= <base>..HEAD -- tmp/the-beautiful-loop-fixes.md  # must be empty
  ```

  Every commit in `<base>..HEAD` must show author `<gh-name> <gh-email>` and must not be authored by Elon Presley.

- Working tree must be clean when done.

## 6. Derive the PR title

- Read `tmp/send-beautiful-pr-body.md` (saved in step 3). Its full contents are the PR **body** verbatim.

- Derive a concise **title** from that file:
  - Prefer the first bullet under `## Summary` (strip list markers and bracket placeholders).
  - If there is no `## Summary`, use the first non-empty goal-like line.
  - Keep the title under ~72 characters.

- If the user supplied an explicit title in their message, honor it.

## 7. Push — only after step 4 yes (or a clean audit with nothing to fix)

- Re-check the branch is still absent on the remote (`git ls-remote --heads origin "<branch>"` must be empty). If it appeared meanwhile, stop.

- Push once:

  ```bash
  git push -u origin HEAD
  ```

- Never force-push.

## 8. Open the pull request

- Create against the default base branch:

  ```bash
  gh pr create --base <base> --title "<title>" --body-file tmp/send-beautiful-pr-body.md
  ```

- Return the PR URL. That is the primary deliverable.

- If `gh pr create` fails because a PR already exists, print the error and stop — do not edit an existing PR unless the user explicitly asked for that in this conversation.

## 9. Report

- Print: branch name, base branch, GitHub login, author identity used (`<gh-name> <gh-email>`), audit findings, whether history was rewritten, push confirmation, PR URL, and title.

- Write the same report to `tmp/send-beautiful-pr.md`.

## Safety and scope

- **First push only.** Remote branch must not pre-exist. No force-push. No updating an existing remote branch.

- **Elon Presley notes commits never go upstream.** Drop them from pushed history; PR body still comes from the saved `PR-DESCRIPTION.md` content.

- **No Co-authored-by on the published branch.** Strip every instance before push.

- **No push without consent when fixes are needed.** If reauthoring or trailer removal is required, the user must say yes first; otherwise abort.

- **No invented PR text.** Body comes from the prepared `PR-DESCRIPTION.md`, not from fresh generation.

- **`tmp/the-beautiful-loop-fixes.md` never goes upstream.** Local the-beautiful-loop scratch only; verify absent from pushed commits.

- Do not reshape code commits, rebase onto main, or rewrite `PR-DESCRIPTION.md` — those belong to `/prepare-beautiful-pr`, `/prepare-beautiful-pr`, and `/fast-beautiful-forward`. Scratch under gitignored `tmp/`.
