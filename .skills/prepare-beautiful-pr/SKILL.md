---
name: prepare-beautiful-pr
description: Run after a feature is built (for example with big-beautiful-build) to get the branch PR-ready. Confirms you are on a clean, non-default branch whose commits sit on top of main (and tells you to run /fast-beautiful-forward first if they do not), thoroughly analyzes the commit structure and OFFERS to factor oversized or mixed commits into several focused ones while keeping the final tree byte-identical, then writes PR-DESCRIPTION.md (big picture first, then details) and commits it as the special PR-notes author Elon Presley. It never pushes and never opens the PR. Use when the user invokes prepare-beautiful-pr, /prepare-beautiful-pr, or asks to shape a branch and write its PR description.
---

# prepare-beautiful-pr — shape the commits, then write the PR description

The contract:

> **Get the branch into PR shape — a clean stack of focused commits on top of main, then a PR-DESCRIPTION.md committed by the special notes author — without ever pushing or opening the PR.**

## 1. Preconditions — check FIRST; if any fails, stop and say exactly how to fix it

- **Inside a git repository.** If not, stop.

- **Working tree clean.** No uncommitted or staged changes. If the tree is dirty, stop and tell the user to commit or stash first — this skill reshapes history and must start from a clean state.

- **Not on the default branch.** Determine the default branch (assume `main`, fall back to `master`). If HEAD is on it, stop: a PR needs a feature branch, not main.

- **Commits on top of main.** Pick the base — the default branch, preferring `origin/main` when a remote exists, otherwise local `main` — and require that the base is an ancestor of HEAD with at least one commit in `base..HEAD`. If there are no commits on top of the base, there is nothing to prepare; stop. If the base is NOT an ancestor (the branch has diverged from main or sits behind it), stop and tell the user to run `/fast-beautiful-forward` first, so the branch is rebased into a clean, fast-forwardable stack on top of the freshest main.

## 2. Analyze the commits, and offer to factor them down

- Read every commit in `base..HEAD` thoroughly: how large each one is, and whether it is a single coherent change or bundles logically separate concerns — for example a feature plus an unrelated refactor plus test scaffolding all in one commit, or one giant commit for the entire feature.

- If any commit is oversized, or groups separate things together, you MUST OFFER to factor the change into several focused commits. Lay out concretely how you would split it — the proposed commits, in order, each a single logical unit — and ask the user whether to proceed. If the user declines, leave the commits as they are and go to step 3.

- If the user agrees, split the work into several commits, each a coherent logical unit in a sensible order. The one ironclad rule: **the final state of the code MUST stay byte-identical.** Before you start, save a backup ref (a branch such as `prepare-beautiful-pr-backup-<YYYYMMDD>-<HHMMSS>`). A safe, non-interactive way to re-partition is to soft-reset to the base (`git reset --soft <base>`, which keeps the whole change staged), then unstage and re-commit the change in logical units — by path, or by applying crafted patches for finer-grained splits — until everything is committed again. When done, VERIFY the tree is unchanged: `git diff <backup> HEAD` MUST be empty. Only ever reshape your branch's own not-yet-shared commits; never rewrite commits that already exist on the base, and keep the stack fast-forwardable.

## 3. Write PR-DESCRIPTION.md and commit it as the special author

- The PR definition lives in `PR-DESCRIPTION.md` at the repo root, and is committed by ONE special "notes" author — a deliberate, separate identity, distinct from the code commits:

  ```
  NAME  = Elon Presley
  EMAIL = dmitry.korolev+elon-presley@gmail.com
  ```

- Write `PR-DESCRIPTION.md`, top to bottom:
  1. **Big picture first** — what this change is, in plain terms.
  2. **Then the details** — the specifics, in descending order of importance.

  It must follow cleanly from the commit history and the actual code changes: no contradictions, no surprises. A reader should be able to map every claim back to the commits.

- Commit **only** `PR-DESCRIPTION.md` (the notes — never code) with both the author AND the committer set to the special identity above, so the commit is unmistakably the notes author's and not yours. For example: `git -c user.name="Elon Presley" -c user.email="dmitry.korolev+elon-presley@gmail.com" commit -m "Add PR-DESCRIPTION.md" -- PR-DESCRIPTION.md`, exporting `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` to the same values if your git does not pick them up for the committer. Do not add any attribution trailer.

## 4. Report

- Print the final commit list for `base..HEAD` (one line each), state plainly that the code's final state is unchanged from before any reshaping (the verified empty tree diff), show `PR-DESCRIPTION.md`, and note that it was committed by Elon Presley. Remind the user that nothing was pushed and that opening the PR is a separate, later step they trigger. Write this same report to `tmp/prepare-beautiful-pr.md`.

## Safety and scope

- Local only. Never push, never open or update a pull request — opening the PR is explicitly a later step. Reshaping touches only your branch's own unshared commits, always behind a backup ref and always preserving the final tree. The one intentional identity twist is the special author on the `PR-DESCRIPTION.md` commit; everywhere else, do not add attribution trailers. Any scratch you write goes under the gitignored `tmp/`.
