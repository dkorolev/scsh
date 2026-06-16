# Contributing to `scsh`

`scsh` (**Scoped Skills Helper**) is a single, self-contained Rust binary that
preflight-checks a git repository, then builds one **in-memory Dockerfile** and
runs the repo's *scoped skills* — in parallel, each in its own ephemeral container
under its configured harness. This document is
the shared playbook for working in this repository: how it's laid out, the house
style, the conventions every commit so far has followed, and the one piece of
terminology that trips everyone up (see [the `tmp/` rule](#the-tmp-rule) — read
it first).

If you only remember three things:

1. **`tmp/` always means the repo subdirectory, never the system temp dir.**
2. **Pure logic stays separate from side effects, and everything is tested.**
3. **The root crate keeps its logic dependency-free (crates: `crossterm`+`console` for
   the UI, `signal-hook` for catching Ctrl-\/SIGINT/SIGTERM safely); only commit when asked.**

---

## The `tmp/` rule

> **Read this once and never forget it.** In this repository, whenever anyone
> writes or says **`tmp/`** they mean the **`tmp/` subdirectory of this repo**
> (`<repo-root>/tmp/`). We are **never** talking about the operating system's
> system-wide temp directory. These are two different places and conflating them
> causes real bugs.

There are genuinely two distinct things, and they are easy to mix up:

| Notation | What it is | Who uses it |
| --- | --- | --- |
| **`tmp/`** | The **`tmp/` subdirectory of this repository** (`<repo>/tmp/`). | Where `scsh` copies a skill's collected `result` back into your repo. **Gitignored.** |
| "the system temp dir" | The OS scratch area (often `/tmp` on Linux/macOS). | Where `scsh` creates a per-run clone dir, `scsh-YYYYMMDD-HHMMSS-utc-run-<skill>`, and builds the image. |

### Why this is confusing (and why it's still correct)

The repository's `.gitignore` ignores the repo's `tmp/` with a single line:

```gitignore
# scsh uses the system temp dir for container build scratch; never track a local /tmp.
/tmp
```

That line **looks like** an absolute system path, but it is not. In gitignore
syntax a **leading slash anchors the pattern to the repository root**, so `/tmp`
matches **`<repo>/tmp/`** — the repo subdirectory — and has nothing to do with
the OS `/tmp`. Git never ignores files outside the work tree anyway.

So:

- A skill's `result:` (e.g. `tmp/ab_result.json`) is written **into the repo's
  `tmp/`**, where it can't be accidentally committed because that directory is
  gitignored.
- `scsh`'s per-run clone and build scratch live in **the system temp dir**, a
  separate place that git neither sees nor tracks.

### It's verified, and it must stay verified

This isn't theoretical — it's enforced and checked:

- **A real `scsh` run refuses to proceed** unless the repo's `tmp/` is gitignored.
  Just before the container steps, `scsh` runs `git check-ignore` and stops with an
  actionable hint if the guard fails. (`scsh list` never runs, so it skips the guard.)
- **A real run also refuses unless the working tree is clean.** Each skill runs on
  a clone of *committed* state, so any uncommitted change (staged, unstaged, or
  untracked) would be absent from the container; `scsh` lists the offending paths and
  says to commit or stash them. (`scsh list` skips this too.)
- You can confirm it at any time:

  ```console
  $ git check-ignore -v tmp/
  .gitignore:4:/tmp	tmp/

  $ git ls-files | grep -E '(^|/)tmp/'    # nothing tracked under any tmp/
  $
  ```

**When writing docs, comments, or commit messages:** say "the system temp dir"
(not a bare `tmp/`) when you mean the OS scratch area, and reserve **`tmp/`** for
the repo subdirectory. Any path a skill writes *back into the repo* belongs under
`tmp/` precisely because it is gitignored.

---

## Repository layout

This repo holds a few kinds of thing, deliberately kept apart:

```
.
├── Cargo.toml, build.rs, src/, tests/, README.md, rustfmt.toml  # ← the scsh crate (repo root)
├── .gitignore                                                   # /target, /tmp (Cargo.lock IS committed)
├── DEMO.md                                                     # the guided, agent-followed demo
├── .skills/                                                     # canonical agent skills (source of truth)
│   ├── README.md
│   ├── add/ · multiply/                                         # example skills (the init-demo project)
│   ├── scsh-harness-demo-and-selftest/                         # bundled: follows DEMO.md to demo + self-test
└── .claude/skills → ../.skills                                 # symlinked host discovery path
```

### The root crate

The **primary `scsh` binary lives at the repository root** (`src/main.rs`,
`src/config.rs`, `src/runtime.rs`, `src/ui/`, `tests/cli.rs`). The root crate is
the product; everything else supports it.

### `.skills/` — agent skills

`.skills/` is the **single source of truth** for repo skills. Each skill is a
folder containing `SKILL.md` (YAML frontmatter + markdown body) plus optional
`scripts/`, `references/`, `assets/`. The tool-specific discovery paths
(`.claude/skills`, `.cursor/skills`, `.opencode/skills`, `.agents/skills`, …) are
**symlinks** to `.skills/`, so one edit updates every host. See
[`.skills/README.md`](.skills/README.md) for the full table. Rules:

- **Author in `.skills/<name>/`** — never in the symlinked tool paths.
- The **folder name must equal the `name` in the frontmatter.**
- The example skills illustrate the env-spec conventions and are what
  `scsh init-demo-project` scaffolds: `add` sums `A`+`B` (defaults `2`,`3`,
  injected by `scsh`), and `multiply` multiplies `X`·`Y` with **no defaults** —
  it lives in the `multiply` profile and `scsh` refuses it if either `X` or `Y`
  is unset. `scsh-harness-demo-and-selftest` is the agent-followed walkthrough of
  [`DEMO.md`](DEMO.md), and is the one skill `scsh installskills` bundles.

- **Prefer a shipped script over harness-authored code.** When a skill needs a
  deterministic computation or a fixed multi-step operation, write a small script (e.g.
  Python via `#!/usr/bin/env python3`) under the skill's `scripts/` and have its `SKILL.md`
  tell the harness to **run** it — don't ask the harness to write Python or bash on the fly.
  A shipped script is reviewable, testable, and saves the model from re-deriving (and maybe
  getting wrong) the same logic each run. The `add`/`multiply` examples do this
  (`scripts/add.py`, `scripts/multiply.py`).

## Development environment

- **Rust toolchain** (`cargo`) — the root crate targets `edition = "2021"` and its
  dependencies are `crossterm` + `console` (the interactive live board) and `signal-hook`
  (to catch SIGINT/SIGTERM safely — std has no signal API); all of its own logic is
  standard-library only, so the binary stays self-contained.
- **`git`** on `PATH` — required by `scsh` itself and by the integration tests.
- **A container runtime** for real runs and for integration-test preflight:
  Apple `container` → `docker` → `podman` on macOS; `docker` → `podman`
  elsewhere. Override with `SCSH_RUNTIME=<docker|podman|container>`.
- **Network** only for a *real* container run (it pulls the base image and
  installs opencode). Building, `scsh list`, and the whole test suite
  need no network.

---

## Build, demo, and test

```sh
cargo build --release          # binary at target/release/scsh
cargo test                      # unit + integration tests
cargo fmt                       # format per rustfmt.toml (run before committing)
```

To exercise the tool end to end, follow [`DEMO.md`](DEMO.md) — hand it to an agent
from an empty directory and it builds and runs a tiny `scsh` project (see below).

### Formatting

Formatting is governed by [`rustfmt.toml`](rustfmt.toml): **2-space indent**, no
hard tabs, `max_width = 120`, `use_small_heuristics = "Max"`, compressed fn
params. Run `cargo fmt` before you commit — diffs are expected to be
already-formatted.

### Markdown: always backtick `scsh`

In **every** Markdown file in this repo, the tool name **`scsh` must be written in
backticks** (inline code: `` `scsh` ``) — never bare. It is a command, not an English
word, and the monospace makes that obvious at a glance. The same goes for its
subcommands and config in prose (`` `scsh run` ``, `` `.scsh.yml` ``, the skill names
`` `add` ``, …). The only exception is inside fenced code blocks, where everything
is already monospace. New docs and edits must keep this consistent.

### Tests

- **Unit tests** live inline in `src/config.rs`, `src/runtime.rs`, `src/main.rs`,
  `src/json.rs`, `src/sha256.rs`, and the `src/ui/` modules, and cover the pure logic: the
  YAML-subset parser, schema validation, runtime-detection ordering, `which`, Dockerfile
  generation, shell quoting, the smart elapsed clock, output-line cleanup, build-command
  detection, the engine start-command advice, commit integration (rebase / fallback-branch
  / run-twice), SHA-256 vectors, the result cache (key determinism/sensitivity,
  store/lookup/restore), and the Ctrl-\ expander (SIGQUIT bumps the counter; the shared
  output log records every line). They need nothing but `cargo` (and `git`, for the
  commit-integration and cache-key tests).
- **Integration tests** (`tests/cli.rs`) drive the compiled binary through the
  whole flow. They require `git` and a runtime on `PATH`, but **must never use
  the network**: every case stops at `scsh list` (or an earlier guard),
  so no image is pulled and no container is built.
- **Always report the passing test count in your commit body** — every
  substantive commit so far does (e.g. *"78 tests pass (unit + integration)"*).

### Demo

[`DEMO.md`](DEMO.md) is the authoritative, English-language **demo**: an agent (or a
careful human) is handed the file **from an empty directory** and follows it to build a
tiny `scsh` project from scratch and run it — `init-demo-project` scaffolds and commits
`add`/`multiply`, `add` runs by default (defaults and forwarded values), `multiply` runs
under its profile with `X`/`Y`, and `scsh` **refuses** `multiply` when they're unset. The
happy path does a **real run** (container + model); only the refusal and `scsh list` are
network-free, so the demo still teaches even without a runtime. The
[`scsh-harness-demo-and-selftest`](.skills/scsh-harness-demo-and-selftest/SKILL.md) skill is
what an agent invokes to follow it. Don't assert the demo's real-run results in CI — keep
programmatic proof in `cargo test`; the demo is the human-facing, end-to-end story.

---

## Code conventions

These are the design rules the codebase already lives by — match them so new code
reads like it belongs.

- **Logic stays dependency-free in the root crate.** Its crates are
  `crossterm` + `console` (pure-Rust, the live UI) and `signal-hook` — a deliberate,
  called-out dependency for one thing: catching Ctrl-\/SIGINT/SIGTERM *safely* (std has
  no signal API; `signal-hook` wraps the OS bits in a safe API). The Ctrl-\ feature
  also isolates each child in its own process group via the safe `Command::process_group`.
  Everything else — including the `.scsh.yml` config (a small purpose-built parser, *not*
  a general YAML library), the JSON reader, and SHA-256 — is standard-library only.
  Reach for another crate only as a deliberate decision to call out in the commit,
  never a default; prefer std. (A self-contained feature under `features/` may still
  pin its own deps.)
- **Separate pure logic from side effects.** `config.rs` (parse/validate) and
  `runtime.rs` (runtime detection, Dockerfile/command generation) are **pure and
  exhaustively unit-tested**; `main.rs` owns the *only* process spawning (git and
  the container runtime). This split is what lets the suite be thorough without
  mocking a container engine — preserve it. New side-effecting code goes in
  `main.rs`; new logic goes in a pure, testable function.
- **Every failure is actionable.** Preflight and guard failures print exactly
  what's wrong (`✗`) and a concrete fix (`→`). Schema validation reports **all**
  problems at once, not just the first. Hold any new error path to the same bar.
- **Strict, all-at-once validation.** Unknown keys, a missing `skills` block, wrong
  types, empty values, a malformed env spec, a result path that escapes the repo — all
  rejected, all listed together.
- **The skill never touches your working tree.** A run operates on a throwaway
  clone in the system temp dir; the only thing written back into your real repo
  is the collected `result`, and only into the gitignored `tmp/` (existing files
  are backed up to `<name>.bak.YYYYMMDD-HHMMSS-utc`, never clobbered). Don't add
  code paths that write elsewhere into the user's repo.
- **Least privilege.** The container runs as a non-root `agent` user whose
  UID/GID match the host user's, so files it writes in the mount are owned by you.
- **Match the surrounding style.** Follow the naming, comment density, and idiom
  of the file you're editing. Keep the README and `--help` in sync with behavior
  changes (the existing commits always do).

For the full runtime/container design (clone strategy, in-memory Dockerfile, the
opencode install layer, `--userns=keep-id`, result collection), see
[`README.md`](README.md) — don't duplicate it here; update it there.

---

## Output style

`scsh`'s terminal output should be **complete but compact** — show everything that
matters, without one `✓` line per micro-step. The guiding rules:

- **Group related facts onto one line**, joined with ` · `. The repo's git state is
  one line, the backend and its build are one line, credentials are one line.
- **Stay quiet on success, loud on failure.** The preflight checks (git → repo →
  `.scsh.yml` → schema → runtime) print *nothing* individually when they pass; they
  collapse into a single summary line. A failing check still prints its own
  actionable `✗ <what's wrong>` / `→ <how to fix>`, with the literal command to type
  rendered **bold** (`bold()` in `main.rs`).
- **Drop redundant lines.** "/tmp scratch ready" added nothing over "/tmp ignored",
  so it's gone. Don't restate what an adjacent line already implies.
- **Name the backend explicitly** so it's obvious what's running the containers:
  `using docker`, `using podman`, or `using Apple Containers` (Apple's `container`
  runtime — the default on macOS). See `backend_name()`.
- **One line per skill**, carrying its result; the final line is the overall verdict.
  A failed skill adds its run-dir and log pointers (for inspection), nothing more.

A real `scsh` run therefore reads like:

```text
✓ git · repo ~/1 · clean · /tmp ignored
✓ using docker · build 0.6s
✓ opencode creds found (forwarded into each skill)
✓ opencode: add  29s  2 + 3 = 5
✓ add: brought in 1 commit (rebased onto main)
✓ all 1 skill completed successfully
```

`scsh list` (no run-only guards) collapses to one summary line instead —
`✓ git · repo ~/1 · .scsh.yml valid (1 skill: add) · using docker` — then lists the
skills by profile (`--verbose` adds the Dockerfile + plan). Keep new output faithful to
this shape: a reader should
learn the repo state, the backend, the credentials, and each skill's outcome, and
little else.

---

## Commits, branches, and pull requests

### Commit messages

Follow the established style (read `git log` for the canonical examples):

- **Imperative, specific subject line** — *"Add runtime detection and the
  in-memory build/run plan"*, *"Wire up preflight, the real run, and init-demo"*.
  Trivial mechanical changes may use a terse subject and no body (*"fmt"*).
- **A body that explains what changed and why**, usually as bullets, and that
  **states the passing test count**.
- **End with the co-author trailer:**

  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```

### Branches

Commit on top of the mainline (`main` here) rather than spinning up side branches.
There is no configured remote — this is a local-first repository, so once history is
shared it is never rewritten.

### When to commit

**Only commit when explicitly asked.** Default behavior is to leave changes in the
working tree, unstaged, for review. Never push or publish without an explicit
request — those are the universal safety boundaries (along with anything
destructive or irreversible), and they hold regardless of any other instruction.

### Definition of done (PR checklist)

- [ ] `cargo fmt` is clean.
- [ ] `cargo test` passes; the commit body states the count.
- [ ] [`DEMO.md`](DEMO.md) still reflects how `scsh` behaves (it's the human-facing demo).
- [ ] New errors are actionable (`✗` what's wrong / `→` how to fix).
- [ ] README / `--help` updated to match any behavior change.
- [ ] The repo's `tmp/` is still gitignored (`git check-ignore -v tmp/`), and no
      build output, clones, or results are tracked.
- [ ] A new `features/<id>/` stays self-contained; the root crate adds no new deps
      beyond `crossterm`/`console` (UI) and `signal-hook` (signals) unless deliberately
      called out.

---

## A note on terminology, one more time

If you take away nothing else: **`tmp/` is the repo's own gitignored
subdirectory.** The system temp dir is a separate place we call "the system temp
dir." Keep them straight in code, comments, docs, and commit messages.
