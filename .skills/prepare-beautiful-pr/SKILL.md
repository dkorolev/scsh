---
name: prepare-beautiful-pr
description: "Run after a feature is built (for example with big-beautiful-build) to get the branch PR-ready. Confirms you are on a clean, non-default branch whose commits sit on top of main (and tells you to run /fast-beautiful-forward first if they do not), thoroughly analyzes the commit structure and OFFERS to factor oversized or mixed commits into several focused ones while keeping the code tree byte-identical, ensures PR-DESCRIPTION.md is the unique last commit, then writes or updates PR-DESCRIPTION.md using only a BLUF Summary, What This Changes, and Implementation Details shape and commits it as the special PR-notes author Elon Presley. It never pushes and never opens the PR. Use when the user invokes prepare-beautiful-pr, /prepare-beautiful-pr, or asks to shape a branch and write its PR description."
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

- Treat any existing `PR-DESCRIPTION.md` change as notes, not product code. Before proposing or doing any split, inspect whether `PR-DESCRIPTION.md` already exists in the working tree or in any commit in `base..HEAD`; read it and remember it as prior PR-description context for step 3. The final stack must contain exactly one `PR-DESCRIPTION.md` commit, and it must be the last commit in `base..HEAD`.

- If any commit is oversized, or groups separate things together, you MUST OFFER to factor the change into several focused commits. Lay out concretely how you would split it — the proposed commits, in order, each a single logical unit — and ask the user whether to proceed. If the user declines, leave the commits as they are and go to step 3.

- If the user agrees, split the work into several commits, each a coherent logical unit in a sensible order. Also do this reshaping if `PR-DESCRIPTION.md` already appears in the branch history but is absent from the last notes commit, bundled into a code commit, or appears in more than one commit. The one ironclad rule: **the final state of the code MUST stay byte-identical.** Before you start, save a backup ref (a branch such as `prepare-beautiful-pr-backup-<YYYYMMDD>-<HHMMSS>`). A safe, non-interactive way to re-partition is to soft-reset to the base (`git reset --soft <base>`, which keeps the whole change staged), then unstage and re-commit the code change in logical units — by path, or by applying crafted patches for finer-grained splits — while keeping `PR-DESCRIPTION.md` out of every code commit. When done, VERIFY the code tree is unchanged from the backup while ignoring the notes file: `git diff <backup> HEAD -- . ':(exclude)PR-DESCRIPTION.md'` MUST be empty. Only ever reshape your branch's own not-yet-shared commits; never rewrite commits that already exist on the base, and keep the stack fast-forwardable.

## 3. Write PR-DESCRIPTION.md and commit it as the special author

- The PR definition lives in `PR-DESCRIPTION.md` at the repo root, and is committed by ONE special "notes" author — a deliberate, separate identity, distinct from the code commits:

  ```
  NAME  = Elon Presley
  EMAIL = dmitry.korolev+elon-presley@gmail.com
  ```

- If `PR-DESCRIPTION.md` already exists, read it before writing. If it is accurate, use it as the baseline structure or hint and amend/update it instead of jumping to a different organization. If it is partly accurate, preserve the useful structure and replace inaccurate parts. If it is stale or misleading, rewrite it from the actual changes and say why in the report.

- Write `PR-DESCRIPTION.md` as a fresh look at the actual branch changes, using this prompt as the shape of the thinking:

  > take a fresh look at our code changes
  >
  > what's the BLUF / TLDR of the biggest goal of the changes, a few sub-goals you'd highlight, and some implementation details?
  >
  > what was changed/added/removed, and in what components? also separate by language is appropriate and also separate by large code component (py API vs. Rust vs. golang, SQL, etc)

  Prefer this reader-facing shape, modeled on PR #1411:

  ```markdown
  ## Summary
  - [BLUF / TLDR: the biggest goal of the change, in plain terms.]
  - [A second important outcome or sub-goal.]
  - [A third outcome, boundary, or explicit non-goal when useful.]

  ## What This Changes
  [One to three short paragraphs explaining what the code now does differently, in broad strokes first, then a little detail. Write this as an explanation of behavior, not a file-by-file changelog.]

  ## Implementation Details
  - [Concrete mechanism, design choice, or important path through the code.]
  - [Another implementation detail, grouped by component/language when useful.]
  - [Another implementation detail when the change benefits from one.]
  ```

  Write it as a synthesized explanation of what the code does, not as a checklist copied from `git diff`. Start with the user-facing/product or system behavior, then explain how the branch achieves it. Use component/language grouping inside `Implementation Details` when the PR spans large areas such as Python API, Rust, Go, SQL, JavaScript/TypeScript, docs, or infrastructure. The body ends with implementation details; testing stays in the branch and CI.

  It must follow cleanly from the commit history and the actual code changes: no contradictions, no surprises. A reader should be able to map every claim back to the commits.

- Commit **only** `PR-DESCRIPTION.md` (the notes — never code) with both the author AND the committer set to the special identity above, so the commit is unmistakably the notes author's and not yours. The notes commit must be unique and last in `base..HEAD`: if an earlier commit already touched `PR-DESCRIPTION.md`, extract that file from the earlier history, amend/update the content as needed, and recreate it as the final commit. If the final commit is already the sole `PR-DESCRIPTION.md` commit, amend that final commit instead of adding another notes commit. For example: `git -c user.name="Elon Presley" -c user.email="dmitry.korolev+elon-presley@gmail.com" commit -m "Add PR-DESCRIPTION.md" -- PR-DESCRIPTION.md`, exporting `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` to the same values if your git does not pick them up for the committer. Do not add any attribution trailer.

## 4. Report

- Print the final commit list for `base..HEAD` (one line each), state plainly that the code's final state is unchanged from before any reshaping (the verified empty code-tree diff), show `PR-DESCRIPTION.md`, and note that it was committed by Elon Presley as the unique last notes commit. Remind the user that nothing was pushed and that opening the PR is a separate, later step they trigger. Write this same report to `tmp/prepare-beautiful-pr.md`.

## Safety and scope

- Local only. Never push, never open or update a pull request — opening the PR is explicitly a later step. Reshaping touches only your branch's own unshared commits, always behind a backup ref and always preserving the code tree. The one intentional identity twist is the special author on the unique last `PR-DESCRIPTION.md` commit; everywhere else, do not add attribution trailers. Any scratch you write goes under the gitignored `tmp/`.
