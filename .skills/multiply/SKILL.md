---
name: multiply
description: >-
  Multiply environment variables X and Y and report the product. Unlike add there are
  NO defaults: if X or Y is unset, complain and return an error. Use when the user
  invokes multiply, /multiply, or asks to "run skill multiply" (with X=… and Y=… set).
---

# multiply — X × Y

You are running the **multiply** skill. It ships its own worker script — **run it, don't
reimplement the arithmetic.** Do exactly this:

1. **Run the script:** `python3 scripts/multiply.py`. It reads `X` and `Y` (both
   **required**, no defaults) from the environment, computes the product, and writes
   `"<X> * <Y> = <product>"` to `tmp/multiply_result.json` (as `{"result": "<line>"}`).
   For `X=6 Y=7` that is `6 * 7 = 42`. If `X` or `Y` is unset or not an integer, the script
   exits non-zero and writes nothing — its absence makes `scsh` mark the run as failed.

Don't compute the product yourself or write your own script — `scripts/multiply.py` is the
source of truth. When run through `scsh`, the `multiply` profile declares X and Y as
required (`${X}` / `${Y}`), so `scsh` refuses the skill — before it ever starts — if either
is unset.
