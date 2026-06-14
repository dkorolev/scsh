---
name: add
description: >-
  Add environment variables A and B (defaulting A=2, B=3), report the sum, AND record it
  as a git commit. Use when the user invokes add, /add, or asks to "run skill add"
  (optionally with A=… and B=… set in the environment).
---

# add — A + B

You are running the **add** skill. It ships its own worker script — **run it, don't
reimplement the arithmetic.** Do exactly this:

1. **Run the script:** `python3 scripts/add.py`. It reads `A` and `B` from the environment
   (defaulting A=2, B=3), computes the sum, writes the result line `"<A> + <B> = <sum>"` to
   `tmp/add_result.json` (as `{"result": "<line>"}`), and appends that same line to
   `add_log.txt` at the repo root. For the defaults that line is `2 + 3 = 5`.

2. **Record the result as a commit.** `add_log.txt` is tracked (it is *not* under `tmp/`),
   so commit just that file:

   ```sh
   git add add_log.txt
   git commit -m "add: <the result line>"
   ```

   Commit only `add_log.txt` — never `tmp/` (it is gitignored) and nothing else. Each run
   appends a line and makes a new commit, so running `add` twice produces two commits.

Don't compute the sum yourself or write your own script — `scripts/add.py` is the source of
truth. When run through `scsh`, A and B are forwarded for you (defaults injected), and
`scsh` prints the `result` value after the skill finishes. Because this skill is marked
`commits: true` in `.scsh.yml`, `scsh` then **rebases your commit onto the caller's branch**
(or, if it can't apply cleanly, saves it to a `scsh/incoming/add-…` branch).
