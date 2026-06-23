---
name: scsh-harness-demo-and-selftest
description: >-
  Demo AND self-test `scsh` end to end: follow DEMO.md to take an empty directory to a working,
  committed `scsh` project and run it — init-demo-project scaffolds add/multiply, `add` runs by
  default on three routes (opencode+GPT, claude+Sonnet, opencode+GLM) with probing first, `multiply`
  runs under its profile with X/Y, `scsh` refuses multiply when they're unset, and a re-run is
  served from cache. Report PASS/FAIL for each predicted outcome. Use when the user invokes
  scsh-harness-demo-and-selftest, /scsh-harness-demo-and-selftest, asks to "run the `scsh` demo",
  or wants to verify `scsh` works.
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

Follow **`DEMO.md` steps 1–13** in order (step 14 is optional cleanup). The setup block is steps 1–4; the demo body is steps 5–13.

1. **Probe the environment and three demo routes** — git, `scsh`, container runtime, and whether each route is available:
   - **`add-opencode-gpt`** — opencode + `openai/gpt-5.4-mini-fast`
   - **`add-claude-sonnet`** — claude + `sonnet` (sonnet-4-6; needs `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token` or `~/.claude/.credentials.json`)
   - **`add-opencode-glm-5.2`** — opencode + `nebius-glm/zai-org/GLM-5.2`
   **FAIL immediately** if `demo routes available: 0 / 3`.
2. **Pick where the demo repo goes.** The normal case is **any directory that is not yet a git repository** — create the UTC-stamped demo dir there. If you happen to be inside the `scsh` repo, create it under the gitignored `tmp/` instead, so you never dirty the checkout.
3. **Get an `scsh` binary:** use `scsh` if it's on `PATH`, otherwise `cargo build --release` from your `scsh` checkout and use `target/release/scsh`, or substitute every `scsh` with `cargo run --release --quiet --`. The demo works either way.
4. **Initialize the demo directory** as a git repo (`git init`, demo user config).
5. **Follow `DEMO.md` from step 5 onward**, executing each command yourself and **reading `scsh`'s output**: `scsh init-demo-project` (scaffolds and commits), then the runs through step 13.

## Self-test — confirm each predicted outcome (report PASS/FAIL)

Treat every prediction as an assertion; any divergence from `DEMO.md` is a **FAIL** and a finding.

- Step 1 → at least one of three routes probes **ok**; **FAIL** if `0 / 3`.
- Step 5 `scsh init-demo-project` → scaffolds + commits a clean project (`✓ committed the scaffold`).
- Step 6 `scsh run` → every route that probed **ok** reports **`2 + 3 = 5`**; unavailable routes are **skipped** (`⚠ skipping …`), not fatal.
- Step 7 `A=10 B=20 scsh run` → available routes report **`10 + 20 = 30`**.
- Step 8 `X=6 Y=7 scsh run --profile multiply` → `multiply` works: **`6 * 7 = 42`** (only multiply runs).
- Step 9 `scsh run --profile multiply` (no X/Y) → `multiply` is **refused by `scsh` itself**, before any container starts (exit non-zero). `scsh list` (network-free) shows multiply requires `env: X, Y`.
- Step 11 reset to the scaffold commit and `scsh run` again → the result is served from cache (**`(cached)`**, ~0s, no container) and the journaled commit is replayed.
- Step 13 harness report → lists all three routes with probed ok/N/A and which result files exist.

Finish with a one-line verdict: how many predictions passed, which routes were available, and any that failed.

## Notes

- The happy path does a **real run** — it builds a container image and calls a model for each available route. Routes that probe N/A are skipped by `scsh run`.
- `DEMO.md` is the source of truth. If you believe a step is wrong, fix `DEMO.md` rather than silently diverging from it.
