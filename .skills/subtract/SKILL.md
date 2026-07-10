---
name: subtract
description: "Subtract environment variable D from C (defaulting C=10, D=4), report the difference under the repository's gitignored tmp/, append the result line to subtract_log.txt, and commit that file. Use when the user invokes subtract, /subtract, or asks to run skill subtract (optionally with C=... and D=... set in the environment)."
---

# subtract

You are running the **subtract** skill. It ships its own worker script under `scripts/` - run it; do not reimplement the arithmetic or write a substitute script.

## Steps

1. **Run the script:** `python3 scripts/subtract.py`. It reads `C` and `D` from the environment (defaulting C=10, D=4), computes the difference, appends the line to `subtract_log.txt` at the repository root, **commits that file** (`subtract: <the result line>`), and writes `{"result": "<line>"}` to the result file. For the defaults the line is `10 - 4 = 6`.

   When run through `scsh`, the result path is in `$SCSH_RESULT` (always under `tmp/`). When run on its own, the script defaults to `tmp/subtract_result.json`.

2. **The commit is the script's job — do not commit anything yourself.** `subtract_log.txt` is the deliverable (tracked at the repository root, not under `tmp/`), and the script has already committed it by the time it prints the result. Never stage or commit anything under `tmp/` (gitignored scratch). Each run appends a line and makes a new commit, so running subtract twice produces two commits. Only if the script printed a `could not commit` warning, commit `subtract_log.txt` (and nothing else) yourself.

## tmp/

Throughout this skill, **`tmp/` means the gitignored `tmp/` subdirectory of the repository you are working in** - never the operating system's temp directory. The JSON result file is scratch for `scsh` and belongs under `tmp/`. The committed deliverable is `subtract_log.txt` at the repository root.

## Git

Append commits on top of the current branch; do not rewrite shared history (no rebasing or squashing of commits that already exist upstream). Match this repository's commit-message conventions. Do not add attribution trailers such as `Co-Authored-By`. Never push, open a pull request, or take any other outward-facing action unless the user explicitly asks.

## scsh

When run through `scsh`, C and D are forwarded for you (defaults injected), and `scsh` prints the `result` value after the skill finishes. Because this skill is marked `commits: true` in `.scsh.yml`, `scsh` rebases your commit onto the caller's branch (or, if it cannot apply cleanly, saves it to a `scsh/incoming/subtract-...` branch). Together with `add` (which commits `add_log.txt`), a default run yields two commits from two different steps — each packed by `packdiff` into a browsable diff on the job page.
