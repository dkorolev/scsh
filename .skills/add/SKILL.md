---
name: add
description: Add environment variables A and B (defaulting A=2, B=3), report the sum under the repository's gitignored tmp/, append the result line to add_log.txt, and commit that file. Use when the user invokes add, /add, or asks to run skill add (optionally with A=... and B=... set in the environment).
---

# add

You are running the **add** skill. It ships its own worker script under `scripts/` - run it; do not reimplement the arithmetic or write a substitute script.

## Steps

1. **Run the script:** `python3 scripts/add.py`. It reads `A` and `B` from the environment (defaulting A=2, B=3), computes the sum, writes `{"result": "<line>"}` to the result file, and appends the same line to `add_log.txt` at the repository root. For the defaults the line is `2 + 3 = 5`.

   When run through `scsh`, the result path is in `$SCSH_RESULT` (always under `tmp/`). When run on its own, the script defaults to `tmp/add_result.json`.

2. **Record the result as a commit.** `add_log.txt` is the deliverable (tracked at the repository root, not under `tmp/`). Commit only that file:

   ```sh
   git add add_log.txt
   git commit -m "add: <the result line>"
   ```

   Never stage or commit anything under `tmp/` (gitignored scratch). Each run appends a line and makes a new commit, so running add twice produces two commits.

## tmp/

Throughout this skill, **`tmp/` means the gitignored `tmp/` subdirectory of the repository you are working in** - never the operating system's temp directory. The JSON result file is scratch for `scsh` and belongs under `tmp/`. The committed deliverable is `add_log.txt` at the repository root.

## Git

Append commits on top of the current branch; do not rewrite shared history (no rebasing or squashing of commits that already exist upstream). Match this repository's commit-message conventions. Do not add attribution trailers such as `Co-Authored-By`. Never push, open a pull request, or take any other outward-facing action unless the user explicitly asks.

## scsh

When run through `scsh`, A and B are forwarded for you (defaults injected), and `scsh` prints the `result` value after the skill finishes. Because this skill is marked `commits: true` in `.scsh.yml`, `scsh` rebases your commit onto the caller's branch (or, if it cannot apply cleanly, saves it to a `scsh/incoming/add-...` branch).
