---
name: fast-beautiful-forward
description: Replays the current branch's local commits on top of the freshest main of a real REMOTE upstream (GitHub or another proper remote — never a local directory), so that a pull request opened LATER would be a clean fast-forward. Auto-resolves only conflicts it is certain about; for any genuine conflict it stops and asks the user one question at a time. Never pushes and never opens the PR. Use when the user invokes fast-beautiful-forward, /fast-beautiful-forward, or asks to fast-forward / rebase their branch onto upstream main so a future PR fast-forwards.
---

# fast-beautiful-forward — make the branch fast-forward onto upstream main

The contract is small and absolute:

> **Replay your work on top of the freshest upstream main, so a future PR is a pure fast-forward. Resolve only the conflicts you are certain about, ask about the rest one at a time, and never push or open the PR.**

## 1. Find a real upstream (a remote, never a local dir)

- You must be inside a git repository. If not, stop.

- Pick the upstream remote: prefer one named `upstream`, otherwise `origin`. Its URL MUST be a real remote — `https://`, `git@`, `ssh://`, or `git://`. A local filesystem path or a `file://` URL does **not** qualify and must be rejected; a local clone is not an upstream.

- If no remote qualifies, stop and ask — this is the one place you must ask up front. If `gh` is installed and authenticated (`gh auth status` succeeds), tell the user you only need the repository's `org/name` and that you will resolve and add it via `gh`. Otherwise, request a proper GitHub (or other) remote URL. Add what they give you as the `upstream` remote, then continue.

## 2. Fetch and locate upstream main

- `git fetch <upstream>` — fetching is a read, do it freely. Then determine the upstream's default branch (usually `main`, falling back to `master`), e.g. from `git remote show <upstream>` or `git rev-parse --abbrev-ref <upstream>/HEAD`. Call it `<upstream>/<main>` below.

## 3. Roll up onto it — the fast-forward

- First make the operation recoverable: record `git rev-parse HEAD` and create a backup branch `fbf-backup-<YYYYMMDD>-<HHMMSS>` pointing at it (the reflog is a second safety net). Any scratch you write goes under the gitignored `tmp/`.

- Rebase your branch onto the upstream tip: `git rebase <upstream>/<main>`. This replays only **your own** not-yet-shared commits and never rewrites commits that already exist on the upstream. Afterward `<upstream>/<main>` is an ancestor of `HEAD`, so a PR opened later fast-forwards.

- If there is nothing to replay (your branch is already based on the tip), say so and stop — already fast-forwardable, done.

## 4. Conflicts — certain ones silently, real ones one question at a time

- Resolve **only** conflicts whose correct result is unambiguous: pure formatting or whitespace, an identical block added on both sides, a regenerated lockfile or build artifact, trivial import reordering. Fix those, then `git rebase --continue`.

- For any **genuine** conflict — where the two sides express different intent and either could legitimately win — STOP. Summarize that one discrepancy: what your side does, what the upstream side does, and why they clash. Then ask the user a single, specific question about how to resolve **that** conflict. Wait for the answer, apply exactly what they chose, and only then move to the next conflict. One question at a time — never batch them, never guess.

- Never `git rebase --skip` to make a conflict disappear, and never resolve by silently dropping a side.

## 5. Safety boundaries

- Local-only. Never push, never force-push, and never open or update a pull request — the PR is explicitly a **later**, separate step that the user triggers.

- Only ever replay your own unshared commits; never rewrite shared history that already exists upstream.

- Match the repository's commit conventions and do not add attribution trailers.

## 6. Report

- Print: the upstream and main branch used, the base SHA, the commits replayed (`git log --oneline <upstream>/<main>..HEAD`), each conflict and how it was resolved, and a confirmation that the branch now fast-forwards — `git merge-base --is-ancestor <upstream>/<main> HEAD` exits clean. Tell the user it is ready and that opening the PR is their next, separate step. Write this same report to `tmp/fast-beautiful-forward.md` so it persists after the run.
