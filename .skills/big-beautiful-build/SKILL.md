---
name: big-beautiful-build
description: One-shot feature factory. Asks the user ONCE to describe a feature in full, then delivers it COMPLETELY — working code, runnable demo, README, passing tests — asking NO further questions. Every gap is filled with a documented assumption, never a follow-up. Use when the user invokes big-beautiful-build, /big-beautiful-build, or says "build me the entire feature, no more questions", with "the whole feature" or "the complete feature" being synonymous.
---

# big-beautiful-build — one-shot, complete, no-more-questions feature delivery

A metaskill. The contract with the user is simple and absolute:

> **Ask once. Then deliver everything. Never ask again.**

You collect the feature description in a single intake step, then build a complete, runnable feature — code + demo + README + tests + deps — without ever coming back to the user with a clarifying question. Every ambiguity is resolved by you, with a sensible default that you journal diligently as an assumption. The deliverable is judged on end-to-end completeness: it actually runs, and you have verified it end to end. The assumption should be documented in a human way, and not as a question but as a statement, along the lines of "To speed up delivery, here the assumption is ...".

## The one and only question (step 1)

Send the user **exactly one** message asking them to describe the feature in enough detail to build it end-to-end. Make it a single prose request — not a multiple-choice form — and tell them plainly this is their only chance to add detail. Prompt them to cover, in their own words:

- What it does — the core behavior, in one or two sentences.
- Inputs / outputs — what goes in, what comes out, in what shape.
- Scope boundaries — what is explicitly in, and what is out.
- Acceptance criteria — what desired outcomes should be codified as unit tests or as English instructions to pass.
- Tech / language — if relevant, preferred language, framework, runtime, or other details.
- Any hard constraints — must-use libraries, offline-only, no network, performance, etc.

Close the message with: *"After this I'll build the whole thing and won't ask anything else — anything you leave unspecified, I'll decide and document."*

Then wait for that one reply. That is the entire interactive phase.

## The no-more-questions rule (mandatory, the heart of this skill)

After the intake reply, you do not ask the user anything until the feature is fully delivered. Not "which database?", not "should I add auth?", not "is this okay so far?". For every unknown:

1. Choose the simplest reasonable default that keeps the feature complete and runnable.
2. Record it under Assumptions in the feature's README, phrased as **"Assumed: X"**.
3. Keep going.

Prefer zero-dependency / standard-library choices, local-only behavior, and small scope done fully over large scope done partially. If the request is genuinely huge, deliver a complete vertical slice that runs end-to-end and list what was deferred in the README — never a half-built skeleton, and never a question.

The only allowed exceptions to silence are the universal safety rules: stop and confirm before destructive, irreversible, or outward-facing actions (deleting the user's data, pushing, publishing, spending money). Those are not "feature questions".

## Where it goes

Default: build what was asked, where it naturally belongs — directly in the repo, integrated like any normal change. Don't invent a separate home for it.

If, and only if, the feature requested to build stands aside from the main focus of the repository, it is encouraged to build the feature, or parts of it, in a dedicated sub-directory. Follow the repo's convention for this (there might be some sandbox / experiments / sub-features directory), and ship the feature (or parts of it) there.

## Definition of done (deliver ALL of it)

The feature is not done until every item below exists and you have actually run the demo and tests and seen them pass:

| Deliverable | Requirement |
| --- | --- |
| Working code | Implements the described behavior; runs with no external setup beyond installing pinned deps. |
| Runnable demo | A single obvious entrypoint, PROGRAMMATIC OR AGENTIC, that works end to end. It can be some `demo.sh`, `make demo`, `python demo.py`, `npm run demo`, and it can also be "Ask your coding agent to follow the instructions in FEATURE-DEMO.md". This complements the tests, it does not replace them. |
| Documentation | What it is / how to install / how to run the demo / how to run tests / Assumptions / Design decisions / what's deferred (if anything). In the repo's preferred format, most often as a Markdown file somewhere. |
| Tests (primary proof) | Automated tests that exercise the core behavior and pass, written with the language's standard test runner so they run in CI on every commit. If a behavior can be asserted programmatically, it belongs here — not in a one-off demo script. No extra test-framework dependency if the stdlib has one. If the feature is complex and invoking every step programmatically is error-prone, prefer the AGENTIC path, where instead of a fixed-format runnable script there exists an English-first Markdown file with short code snippets and descriptions of why they should be run and what their result should be. This way the acceptance criteria can be verified both by a diligent human and by an AI agent. |
| Pinned deps | `requirements.txt` / `package.json` / `Cargo.toml` etc., pinned. For a separate feature, scoped to its own dir; when integrating, add to the repo's manifest as a deliberate, called-out change. Empty/none if stdlib-only — say so in the documentation. |
| No stray artifacts | No `__pycache__/`, `.DS_Store`, `node_modules/`, build output, or `work/` committed (already covered by repo `.gitignore`; add a local `.gitignore` if the stack needs more). |

Verify, don't assume. Run the tests and the demo with Bash, paste the real output into your final report, and only then call it done. If something fails, fix it — silently, per the no-questions rule — until it passes.

Prefer a tight, CI-runnable definition, even if it comes in the form of a prose, not a hard-wired script. Whenever a behavior can be pinned down programmatically, encode it as a unit or regression test (or an orchestrated check) — something that runs on every commit — rather than proving it only through an English demo script. Reserve narrative, loosely-followed demos for genuinely interactive or hard-to-assert flows where a tight definition would be more trouble than it's worth.

## Follow the repo's guidelines (keep the code sane)

- Read the repo playbook first — before you build, find and read whatever governing docs the repository provides and hold the work to them: `CONTRIBUTING.md`, agent and model instruction files (`AGENTS.md`, `CLAUDE.md`, including nested ones), and any conventions the repo declares — a constitution and its amendments, development principles, maxims, or style guides. Many repos carry convoluted, interlinked instructions, so follow the links. The repo's own stated conventions — layout, the `tmp/` rule, house style, commit conventions — win over anything here; the points below are just the parts that bear most directly on a one-shot feature.

- Skill and layout conventions — each skill is a `SKILL.md` in its own directory under `.skills/`, with YAML frontmatter whose `name` matches the directory (folder name == `name`). The common convention is a single `.skills/` directory at the repo root, symlinked from the per-harness directories (`.claude/skills`, `.codex/skills`, `.cursor/skills`, etc.); follow that layout if the repo uses it.

- Build in the right place — by default, integrate the work into the repo like any normal change (see "Where it goes" above). Only a separate feature under `features/<feature-id>/` is held to the self-containment rule: its own `Cargo.toml`/`package.json`, pinned deps, tests, demo, README, and a local `.gitignore` if the stack needs one, wired into nothing else — that isolation is what lets it finish in one shot.

- The `tmp/` rule — `tmp/` always means the gitignored `tmp/` subdirectory of the repo you are working in, never the system temp dir. Anything you write back into the repo as scratch goes under `tmp/`; respect the root `.gitignore`.

- House style — match the surrounding code's naming, comment density, and idiom for whatever language you choose; write code that reads like it belongs. Format before you finish (e.g. `cargo fmt`), keep files free of trailing whitespace, and end every file with a newline.

- Git hygiene — commit your work by adding commits on top of the current branch. If this skill was invoked from the repo's main branch (`main` or `master`), commit straight onto it — don't spin up a side branch. Never rewrite history in this mode; only append commits (reordering or rewording your own not-yet-shared commits is fine and encouraged). Match the repository's own commit-message conventions — mirror the subject/body style of recent commits. Never push or open a pull request (or take any other external action) without an explicit request — those remain the universal safety boundaries.

## Workflow (end to end)

1. Ask once (step 1 above). Wait for the single reply.
2. Lock the spec — restate the feature in one line and list the assumptions you're about to make, in your head / your notes; do not send these as questions.
3. Set up the workspace — for a separate feature, scaffold `features/<feature-id>/`; otherwise work in the repo, where the change belongs.
4. Build the implementation, demo, tests, README, and deps.
5. Run the tests (the primary proof) and the demo; fix anything that fails (no questions).
6. Report — print the file tree, the commands to run it, the demo/test output you captured, and the full Assumptions list. Tell the user it's complete and how to try it. Write this same report to `tmp/big-beautiful-build.md` so it persists after the run.

## Anti-patterns

- Do not ask a second question after intake ("just to confirm..."). Decide and document instead.
- Do not just deliver a skeleton, with TODOs, or "next you'd add...". Provide working code.
- Do not declare the process done without running the tests and demo. Self-check yourself and re-run the steps for as long as it is necessary.
- Do not just prove a testable behavior with only a demo script when it could and should be a unit/regression test.
- Do not rewrite history (rebasing/squashing shared commits); only ever add commits on top. Always remain fast-forward-friendly.
- Do not push, open a PR, or perform any external action without an explicit request from the user.
