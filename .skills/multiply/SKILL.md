---
name: multiply
description: Multiply environment variables X and Y and report the product under the repository's gitignored tmp/. Unlike add there are NO defaults - if X or Y is unset, complain and exit with an error. Use when the user invokes multiply, /multiply, or asks to run skill multiply (with X=... and Y=... set in the environment).
---

# multiply

You are running the **multiply** skill. It ships its own worker script under `scripts/` - run it; do not reimplement the arithmetic or write a substitute script.

## Steps

1. **Run the script:** `python3 scripts/multiply.py`. It reads `X` and `Y` (both **required**, no defaults) from the environment, computes the product, and writes `{"result": "<line>"}` to the result file. For `X=6 Y=7` the line is `6 * 7 = 42`. If `X` or `Y` is unset or not an integer, the script exits non-zero and writes nothing - its absence makes `scsh` mark the run as failed.

   When run through `scsh`, the result path is in `$SCSH_RESULT` (always under `tmp/`). When run on its own, the script defaults to `tmp/multiply_result.json`.

## tmp/

Throughout this skill, **`tmp/` means the gitignored `tmp/` subdirectory of the repository you are working in** - never the operating system's temp directory. The result JSON is scratch and belongs under `tmp/`; respect the repository root `.gitignore`.

## Safety

This skill does not commit. Never push, open a pull request, or take any other outward-facing action unless the user explicitly asks.

## scsh

When run through `scsh`, the `multiply` profile declares X and Y as required (`${X}` / `${Y}`), so `scsh` refuses the skill - before it ever starts - if either is unset.
