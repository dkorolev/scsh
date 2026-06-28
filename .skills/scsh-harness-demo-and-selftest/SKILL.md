---
name: scsh-harness-demo-and-selftest
description: >-
  Demo AND self-test `scsh` end to end: follow DEMO.md to take an empty directory to a working,
  committed `scsh` project and run it — init-demo-project scaffolds add/multiply, `add` runs by
  default (with defaults and with forwarded values), `multiply` runs under its profile with X/Y,
  `scsh` refuses multiply when they're unset, and a re-run is served from cache. Report PASS/FAIL
  for each predicted outcome. Use when the user invokes scsh-harness-demo-and-selftest,
  /scsh-harness-demo-and-selftest, asks to "run the `scsh` demo", or wants to verify `scsh` works.
---

# scsh-harness-demo-and-selftest — demo and self-test `scsh` via DEMO.md

You are running the **scsh-harness-demo-and-selftest** skill — scsh's own bundled demo and
self-test. It is two things at once: a faithful, human-readable **demo** of what `scsh` does, and
a **self-test** that confirms each step behaves as documented. It is driven by
**[`DEMO.md`](../../DEMO.md)** — the authoritative, English-language script (at the `scsh` repo
root) — which takes a directory from empty to a working, committed `scsh` project and runs it.

> Run this from a `scsh` checkout (so `DEMO.md` is present). If you only have the installed skill,
> the canonical steps are the checklist below — `DEMO.md` is just the fuller narrative.

## How to run it

1. **Check the environment first (DEMO.md step 0.1)** — git, `scsh`, a container runtime, and
   opencode model auth. It tells you which steps fully run and which degrade to the network-free
   parts.
2. **Pick where the demo repo goes (step 0.2).** The normal case is **any directory that is not yet
   a git repository** — create the UTC-stamped demo dir there. If you happen to be inside the
   `scsh` repo, create it under the gitignored `tmp/` instead, so you never dirty the checkout.
3. **Get an `scsh` binary (step 0.3):** use `scsh` if it's on `PATH`, otherwise
   `cargo build --release` from your `scsh` checkout and use `target/release/scsh`, or substitute
   every `scsh` with `cargo run --release --quiet --`. The demo works either way.
4. **Follow `DEMO.md` top to bottom**, executing each command yourself and **reading `scsh`'s
   output**: `git init`, `scsh init-demo-project` (scaffolds and commits), then the runs.

## Self-test — confirm each predicted outcome (report PASS/FAIL)

Treat every prediction as an assertion; any divergence from `DEMO.md` is a **FAIL** and a finding.

- `scsh init-demo-project` → scaffolds + commits a clean project (`✓ committed the scaffold`).
- `scsh run` → `add` works with defaults: **`2 + 3 = 5`**, and the commit is rebased back.
- `A=10 B=20 scsh run` → `add` forwards your values: **`10 + 20 = 30`**.
- `X=6 Y=7 scsh run --profile multiply` → `multiply` works: **`6 * 7 = 42`** (only multiply runs).
- `scsh run --profile multiply` (no X/Y) → `multiply` is **refused by `scsh` itself**, before any
  container starts (exit non-zero). `scsh list` (network-free) shows multiply requires `env: X, Y`.
- Reset to the scaffold commit and `scsh run` again → the result is served from cache
  (**`(cached)`**, ~0s, no container) and the journaled commit is replayed.

Finish with a one-line verdict: how many predictions passed, and any that failed.

## Notes

- The happy path does a **real run** — it builds a container image and calls a model, so it needs a
  container runtime and opencode configured with a model. If those aren't available, say so and
  still exercise the network-free parts: `init-demo-project`, `scsh list`, the `multiply` refusal,
  and a cache hit.
- `DEMO.md` is the source of truth. If you believe a step is wrong, fix `DEMO.md` rather than
  silently diverging from it.
