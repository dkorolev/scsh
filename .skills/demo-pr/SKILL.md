---
name: demo-pr
description: "Write a tiny feature note and PR-DESCRIPTION.md, commit both (two commits), and report ok under tmp/. Use when the user invokes demo-pr, /demo-pr, or asks for a minimal fake-PR / packdiff Description-panel demo (optionally with TITLE=...)."
---

# demo-pr

You are running the **demo-pr** skill. It ships its own worker script under `scripts/` — run it; do not reimplement the file writes or commits yourself.

## Steps

1. **Run the script:** `python3 scripts/demo_pr.py`. It reads optional `TITLE` from the environment (default `Hello from demo-pr`), writes `demo_pr_note.txt` and `PR-DESCRIPTION.md` at the repository root, **commits each in its own commit**, and writes `{"ok": true, "title": "…", "files": […]}` to the result file.

   When run through `scsh`, the result path is in `$SCSH_RESULT` (always under `tmp/`). When run on its own, the script defaults to `tmp/demo_pr_result.json`.

2. **The commits are the script's job — do not commit anything yourself.** Never stage or commit anything under `tmp/` (gitignored scratch). Only if the script printed a `could not commit` warning, commit `demo_pr_note.txt` and `PR-DESCRIPTION.md` (and nothing else) yourself.

## tmp/

Throughout this skill, **`tmp/` means the gitignored `tmp/` subdirectory of the repository you are working in** — never the operating system's temp directory.

## Git

Append commits on top of the current branch; do not rewrite shared history. Do not add attribution trailers such as `Co-Authored-By`. Never push or open a pull request unless the user explicitly asks.

## scsh

When run through `scsh`, `TITLE` is forwarded for you (default injected), and `scsh` prints the result after the skill finishes. Because this skill is marked `commits: true` in `.scsh.yml`, `scsh` rebases those commits onto the caller's branch (and packdiff packs them into a ⇄ commits diff with a Description panel).
