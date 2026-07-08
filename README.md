# `scsh` — Scoped Skills Helper

**Run your repository's agent skills in parallel — each in its own throwaway
container, on a clean clone of your repo — with one command.** No Dockerfiles, no
`docker run`, no risk to your working tree.

```sh
scsh run
```

That's the whole interface. `scsh` reads a small `.scsh.yml` at your repo root,
builds one container image, and runs **every skill you listed at once** — each one
isolated, each producing a result file that `scsh` copies back into your repo.

**Understand this first:** `scsh` is a single Rust binary plus a tiny per-repo config.
You describe *what* to run (skills, with the environment they need); `scsh` owns *how* —
the clean clone, the container, running as you, collecting results. Nothing touches your
working tree, and a real run insists on a committed, clean repo so what runs is exactly
what you committed.

**To see it in action:** clone this repo and `cargo build --release`, then — from **any
empty directory that is not yet a git repository** — start your favorite skills-aware
agent and ask it to *follow the steps in [`DEMO.md`](DEMO.md)*. It will build a tiny
`scsh` project right there and run it: the `add` skill computes a sum (with defaults and
with values you pass), and `multiply` runs under its profile when given `X`/`Y` — and is
refused by `scsh` itself when they're missing.

---

## What is a "skill", and why would I want this?

A **skill** is a folder in your repo — `.skills/<name>/SKILL.md` — holding
plain-English instructions for an AI coding agent (the same `SKILL.md` format used by
Claude Code, Cursor, opencode, …). A skill might "summarize every open TODO",
"regenerate the changelog", or anything you can write down.

Skills are easy to *write*. Running them well is the hard part — you want each on a
**real checkout** of your repo (never your actual working tree), several **at once**
without stepping on each other, each **reproducible and disposable**, and running as
**you** (not root). `scsh` is the one command that does all of that: you describe
*what* to run, `scsh` owns *how*.

---

## Quick start

```sh
# 1. Build it (Rust toolchain required). Drop the binary on your PATH, or use it from
#    target/release/scsh, or run it via `cargo run -- <command>`.
cargo build --release

# 2. Inside any git repo, scaffold AND commit a demo project (config + example skills,
#    /tmp gitignored, all committed — leaving a clean, ready repo).
scsh init-demo-project

# 3. See what would happen — no containers, no network.
scsh list                  # list every skill by profile (add --verbose for the Dockerfile + plan)

# 4. Do it for real.
scsh run                   # build the image, run every default skill in parallel
```

Need to discover or gate on profiles from another tool? Two **runtime-free** commands
(just git + a valid `.scsh.yml`, no container runtime) make it scriptable:

```sh
scsh list --json           # {"profiles":[{"name":"default","skills":["add"]}, …]}  → pipe to jq
scsh check-profile multiply  # exit 0 iff that profile exists with ≥1 skill (else non-zero)
```

> **First time?** [`DEMO.md`](DEMO.md) is a guided, English walkthrough that builds a
> tiny `scsh` project from an empty directory and runs it — follow it yourself, or hand
> it to your AI assistant.

A real `scsh run` requires a **clean working tree** (it clones *committed* state into
the container), so commit or stash your changes first — `init-demo-project` does that
for you.

---

## The `.scsh.yml` config

You describe your project and its skills; you never write a container command.

The whole file is just your skills — no `version`/`project`/`image` boilerplate. scsh
builds them on built-in harness images (`scsh-opencode`, `scsh-claude`) from a shared Debian base.

```yaml
skills:                          # each key == .skills/<name>/ folder
  add:                           # direct run OR invocations: matrix (below)
    timeout: 600
    env:
      - A: ${A:-2}
      - B: ${B:-3}
    result: tmp/add_{name}_result.json   # {name} required when invocations: is set
    invocations:
      opencode-gpt-5.4-mini-fast:
        harness: opencode
        model: openai/gpt-5.4-mini-fast
        commits: true
      claude-sonnet-4-6:
        harness: claude
        model: sonnet
      opencode-glm-5.2:
        harness: opencode
        model: nebius-glm/zai-org/GLM-5.2
  multiply:
    profile: multiply
    env:
      - X: ${X}
      - Y: ${Y}
    result: tmp/multiply_{name}_result.json
    invocations:
      opencode-gpt-5.4-mini-fast:
        harness: opencode
        model: openai/gpt-5.4-mini-fast
      claude-sonnet-4-6:
        harness: claude
        model: sonnet
```

At run time, each `invocations:` route expands to an invocation named `{skill}-{route}` (for example `add-opencode-gpt-5.4-mini-fast`). A skill with direct `harness:` / `model:` fields runs as a single invocation named after its key.

### A skill's fields

- **`harness`** *(required for direct run)* — **`opencode`** or **`claude`**. Omit at the skill level when using `invocations:`; each route supplies its own.
- **`invocations:`** *(optional matrix)* — named routes, each with `harness`, optional `model`, optional `profile` (overrides the skill-level default), optional `commits` (overrides the skill-level default). Mutually exclusive with top-level `harness` / `model`.
  Every harness runs as a real interactive TUI recorded via tmux + asciinema (see
  [`DAEMON.md`](DAEMON.md)), pointed at the skill's `SKILL.md` — e.g. claude with
  `--permission-mode bypassPermissions` (host `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`, or
  `~/.claude/.credentials.json`), opencode via `opencode --prompt`, grok via its default Build TUI.
- **`model`** *(optional)* — the model the harness passes to its tool.
- **`result`** *(required)* — a **repo-relative** path the skill must create (keep it
  under the gitignored `tmp/`). A missing result fails the skill. When it appears,
  `scsh` parses it as JSON and prints the message — a `result`/`message` field, or a
  lone single field — on the skill's line (not just the path).
- **`profile`** *(optional)* — groups a skill under a named profile. `scsh run` (no
  `--profile`) runs the reserved **`default`** profile: the skills with no `profile:`.
  `scsh run --profile <name>` runs **only** that profile's skills; pass a
  comma/semicolon list to run several (`--profile default,<name>` adds the default ones
  back in). If every skill is profiled, a bare `scsh run` is an intentional no-op — `scsh`
  prints the available profiles and their skills.
- **`commits`** *(optional, default `false`)* — when `true`, a skill may **commit to
  its clone**, and `scsh` brings those commits back: after the run it **rebases** the
  skill's new commits onto your current branch. Several commit-enabled skills compose
  (each rebases onto the branch the previous one advanced — no fast-forward assumed).
  If a skill's commits don't apply cleanly, `scsh` leaves your branch alone and saves
  them to a distinct **`scsh/incoming/<skill>-<utc>-<short>`** branch for you to inspect
  and merge. Bringing commits in is a real side effect: **run twice and you get the
  commit twice** — `scsh` never dedups or skips it. (The image includes `git`, and each
  clone gets a local commit identity so the skill can commit.) Those commits are authored
  by a deliberately unmistakable bot — **`dkorolev-neon-elon-bot`** — and are **local-only
  by design** (`scsh` rebases, never pushes); if that author ever turns up in a code
  review or a pushed commit list, you pushed something you shouldn't have. See
  `scsh help cache`.
- **`env`** *(optional)* — host variables to forward. `scsh` resolves each value:
  - `${VAR}` / `$VAR` — **require** `VAR`; refuse the skill if it's unset (a bare
    `KEY: VAR` is a *literal* — to forward a variable write `KEY: ${VAR}`).
  - `${VAR:-default}` — forward `VAR`, or inject `default` when unset (`${VAR:-}` =
    empty).
  - `${VAR:?message}` — **require** `VAR`, refusing with your `message`.

`scsh` validates `.scsh.yml` **strictly and all at once** — every problem (unknown keys,
a missing `skills` block, a malformed env spec, a result path that escapes the repo) is
reported together, so you fix them in one pass.

---

## Commands

```
scsh                       Show help (the default — a bare scsh is safe and never runs).
scsh run [--profile X]     Preflight, then build the image & run the selected skills in parallel.
scsh run --def <name>      Run a harness definition (built-in, ~/.harness/, or repo .harness/).
scsh list  (alias: ls)     List every skill by profile — result, commits, env (--verbose: + internals).
scsh init-demo-project     Scaffold AND commit a demo: .scsh.yml + example skills + tmp/ ignore.
scsh installskills [url]   Install skills — bundled, or a git repo's (merges its .scsh.yml).
scsh updateskills  [url]   Reinstall skills, overwriting files — bundled or a git repo's.
scsh help                  Show help (includes the schema).
scsh version               Show the version (with the build's git short hash, +`-dirty`).
scsh daemon start|stop|restart|status
                           Session browser on http://127.0.0.1:7274 (override with SCSH_DAEMON_PORT).
                           scsh run auto-starts an ephemeral daemon and prints a session URL.
```

See [`DAEMON.md`](DAEMON.md) for the session browser API and demo script.

**Harness definitions & starting a job from the browser.** Besides `.scsh.yml` skills, `scsh`
runs **harness definitions** — parameterized jobs in `.harness/<name>.yml` (in the repo or
`~/.harness/`) plus built-ins (`doctor`, `add`, `research`, and the `fruits` workflow). A flat
definition declares a `description`, typed `params` (which become environment variables), a
`task` body, and an `invocations:` agent matrix. A **workflow** definition instead declares
`steps:` — a DAG where each step runs an agent, writes typed `output`, and feeds later steps
whose `inputs` bind to `params.NAME` or `stepid.field` (`needs:` gives the edges, `when:` gates a
step). Run one from the console with `scsh run --def <name>` (params from the environment), or,
when the daemon is up, open a repository in the browser (type/paste a path or use the native
folder picker) and start a job from a rendered parameter form — the daemon runs at most one job
per directory. Here the word "harness" means the runnable definition; the CLI it dispatches to
(claude/codex/opencode/…) is the definition's *agent*. See [`DAEMON.md`](DAEMON.md) and
[`DAEMON-JOBS.md`](DAEMON-JOBS.md).

**Installing skills.** With no arguments, `scsh installskills` drops scsh's one bundled skill —
`scsh-harness-demo-and-selftest`, a demo-and-self-test you run with `/scsh-harness-demo-and-selftest`
— into your repo's `.skills/`, and points you at a real skills repo for anything else. Give it one
or more **git URLs** to install the skills those repos ship (installed in order, as if you ran the
command once per repo):

```sh
scsh installskills https://github.com/dkorolev/beautiful-skills
# several at once — installed in order, landing as one reviewable diff:
scsh installskills https://github.com/dkorolev/beautiful-skills https://github.com/dimacurrentai/code-review-skills
```

Like a real run, `installskills`/`updateskills` insist on a **clean working tree** (so the install
is a reviewable diff, not mixed into unrelated work) and make sure **`/tmp` is gitignored** before
writing, so the repo is run-ready afterward.

**If the source repo has its own `.scsh.yml`, that manifest drives the install.** `scsh`
validates it first (and stops if it's malformed), then for every skill it lists — except
the **authoring-only** ones (marked **`autoinstall: false`**, *or* named with the
**`internal-`** prefix, e.g. a repo's own self-check skill) — it copies the skill folder
*and* merges that skill's YAML block verbatim into **your** `.scsh.yml** (same schema,
including `invocations:` matrix skills). Existing skill keys in the consumer are left
untouched — scsh warns when a key would conflict. The skills are then runnable
immediately: a default skill on `scsh run`, a profiled one on `scsh run --profile
<name>`. Matrix skills expand to `{skill}-{route}` invocations at run time. Skills the manifest doesn't list are skipped
(the manifest is the shipping list), and skills already in your `.scsh.yml` are left
untouched. Without a source `.scsh.yml`, `scsh` simply installs every `.skills/<name>/`
folder it finds (no manifest merge).

Either way it wires up the five host symlinks (`.claude/skills`, `.codex/skills`,
`.cursor/skills`, `.opencode/skills`, `.agents/skills` → `../.skills`), and never clobbers
a file that differs from the source (an identical one is simply "already installed"). Use
`scsh updateskills [url]` to overwrite skill files with the source's version.

The legacy flags `--help`/`-h`, `--version`/`-V`, and `--init-demo-project` still work as
aliases. (`ls` is an alias for `list`.)

Set **`SCSH_RUNTIME=<docker|podman|container>`** to force a container runtime instead
of auto-detection (handy when the auto-picked one can't reach the system temp dir —
e.g. snap-packaged Docker, where `SCSH_RUNTIME=podman` is the fix; `scsh` already
prefers Podman over a snap Docker automatically).

---

## Watching a run (the live board)

On a terminal, `scsh run` shows an **interactive live board**: the image build and every skill
are **collapsible rows**, each a `▶`/`▼` triangle, a status glyph (spinner → `✓`/`✗`), the label,
a smart elapsed clock, and the latest output line.

- **Press `0` … `9`, then `A` … `Z`** — image builds appear first, then skill rows in manifest order; each labelled on the left (`[0]`, `[1]`, …). Press
  the row's shortcut to **expand/collapse** it. This is the reliable path when the mouse isn't forwarded:
  scsh turns on the terminal's keyboard-enhancement protocol so Ctrl+digit works too (on a
  terminal without it, the plain digit toggles instead). You can also **click the row** if your
  terminal forwards the mouse.
- Expanding shows that process's full output beneath it, **every line stamped with its time relative
  to when that process started** (`+1.2s`). Open shows it all; closed tucks it away.
- **Scroll** with the **mouse wheel**, **↑/↓**, **PgUp/PgDn**, or **Home/End**. It follows the tail
  until you scroll up, and resumes following at the bottom. **`e`/`c`** expand/collapse every row.
- **`Ctrl-C`** aborts the run — SIGTERM on every child and container, then SIGKILL after one second.

The board is drawn **inline** in the normal terminal buffer — **not** a full-screen takeover — so
your terminal's own scrollback keeps working during the run. When the run finishes, `scsh` **wipes
the live region and leaves a compact `✓`/`✗` summary** in its place: one line per process, nothing
more. Off a TTY (a pipe, a file, CI) there's no board — each step prints a plain `▶` then `✓`/`✗`
line, so logs stay readable.

When the session browser daemon is running, the same events also appear in a browser at a
deep link `http://127.0.0.1:7274/session/abcdef` URL printed at the end of `scsh run`.

> **See it without a container or a model:** `scsh __ui-demo` runs the real board over a few
> scripted subprocesses (click the rows, scroll), and `scsh __ui-demo --frames` prints a few static
> frames of it — handy in a doc or a pipe.

---

## Caching

`scsh` caches each skill's result. Before running a skill it computes a **SHA-256** over
a deterministic blob of:

- the repo's **committed content** (the git `HEAD` tree),
- the **skill's own files** (`SKILL.md` + scripts), and
- the **resolved environment** forwarded to the skill (sorted).

Same content + same skill + same env → same key → a **cache hit**: `scsh` restores the
result instantly and prints `(cached)` — no clone, no container, no model call. A miss
runs the skill and stores its result. The cache lives in the repo's gitignored
**`tmp/.sccache/<sha256>.json`**, and nowhere else.

A commit-enabled skill's run **also journals the commits it made** into that cache entry.
When it commits, the repo's tree changes, so the *next* run is a miss (it runs again). But
get back to the same committed state (e.g. `git reset --hard`) and re-run: it's a hit that
restores the result **and replays the journaled commits** — so the commit reappears on top.
A cache hit reproduces the full side effect, not just the result file. `scsh help cache`
has the details.

---

## What's in the container image

`scsh` builds **one image** (a glibc **Debian-slim** base) shared by every skill, baked with
a broad dev/CLI toolchain so skills can do real work with no setup step:

- **Languages & build:** `python3` (+ **`uv`**), **Go**, **Rust** (`cargo`), C/C++
  (`gcc`/`g++`, `make`, `cmake`, `pkg-config`, `libssl-dev`), `perl`, `gawk`, `node` (+
  `opencode`, the harness).
- **Data & CLI:** `jq`, `yq`, `ripgrep`, `shellcheck`, `git` (+ `git-lfs`), **`gh`** (GitHub
  CLI), `sqlite3`, `postgresql-client` (`psql`), `protobuf-compiler` (`protoc`),
  `curl`/`wget`, `tar`/`gzip`/`xz`/`zip`/`unzip`, `patch`/`diffutils`, `tree`, `less`, `file`.
- **Cloud:** **`aws`** (AWS CLI v2), **`gcloud`** + `gsutil`, **`kubectl`**.
- **Networking:** `ping`, `traceroute`, `dig`/`nslookup`, `nc` (netcat), `ss`/`ip`, `whois`, `socat`.
- **Base:** `ca-certificates`, `gnupg`, `openssh-client`, a UTF-8 locale (`C.UTF-8`).

**Java is intentionally _not_ installed** — nothing in scsh or the example skills is JVM, and
a JDK adds ~300 MB. If you need it, that's a deliberate future add (a per-repo image override).

**Timezone:** the image is built with the **timezone of the machine that builds it** (scsh
passes the host's `TZ` as a build arg), so timestamps a skill produces line up with your
machine. (`scsh run` does the build; see `scsh help internals`.)

**Platform-agnostic:** the image builds on **`x86_64` and `arm64`** (Apple Silicon, arm
servers) alike — every architecture-specific download resolves the target arch at build time,
so there are no hardcoded-arch URLs. The Dockerfile is a single static file,
[`src/Dockerfile`](src/Dockerfile), embedded into the binary at compile time.

The base is glibc Debian (not musl Alpine) precisely so these prebuilt toolchains install and
run without friction. The image is large (a few GB) and **built once, then cached** and reused
across runs — the first `scsh run` (or any change to the Dockerfile) rebuilds it.

## What you need

- A **Rust toolchain** (`cargo`) to build the binary.
- **`git`** on your `PATH`.
- A **container runtime**: Apple `container` → Docker → Podman on macOS; Docker →
  Podman on Linux.
- **Network** only for a real `scsh run` (it pulls the base image and installs
  opencode). `list` and `init-demo-project` need none.
- For skills to do real work, the container's opencode needs a configured model;
  `scsh` forwards your host opencode login into each run for its duration.

---

## Safety & guarantees

- **Your working tree is never touched.** Containers only ever see a throwaway clone
  bind-mounted from the host (**push IN**). Skills must not `git fetch`, `git pull`, or
  `git clone` inside the container. After each skill, scsh **pulls OUT** on the host: the
  `result` file always; new commits only when `commits: true` and the skill committed
  (local fetch from the run clone — not GitHub). scsh never pushes to any remote.
  in the system temp dir; the only thing written back is each skill's `result`, into
  the gitignored `tmp/` (existing files backed up, never clobbered).
- **A real run refuses unless the repo is clean and `/tmp` is gitignored**, so scratch
  and results can never be committed by accident.
- **Least privilege.** The container runs as a non-root `agent` user whose UID/GID
  match yours, so files it writes are owned by you.
- **Secrets don't linger.** Your opencode credential is copied into a run only for its
  duration and removed afterward (opt out with `SCSH_NO_OPENCODE_AUTH=1`).
- **Scratch is cleaned up.** Each skill's container is `--rm`, and its throwaway clone in
  the system temp dir is removed after the skill **succeeds**; a **failed** skill's clone is
  kept for inspection (its path is printed), and clones older than a day are swept at the next
  run's start. Keep every clone with `SCSH_KEEP_RUNS=1`.
- **Nothing outward happens for you.** `scsh` builds and runs locally; it never
  pushes, publishes, or deletes your data.

> **A note on `tmp/`.** Throughout `scsh`, **`tmp/` means the gitignored `tmp/`
> subdirectory of your repo** — never the operating system's temp dir (which we call
> "the system temp dir"). The `/tmp` line in `.gitignore` is anchored to the repo
> root, so it matches `<repo>/tmp/` and has nothing to do with the OS `/tmp`.

---

## Building & repo conventions

- **Build:** a Rust toolchain — `cargo build --release`. The crate is std-only; its only
  dependencies are `crossterm` + `console` (the live UI) and `signal-hook` (signals).
- **Formatting:** `rustfmt.toml` pins the house style — run `cargo fmt` before committing.
- **`Cargo.lock` is committed** on purpose: `scsh` is a binary, so the lockfile pins exact
  dependency versions for reproducible builds.
- **The image installs the latest `opencode` via `npm`** (`npm install -g opencode-ai`) —
  more reliable than the upstream curl installer, and easy to pin.
- **There is no root `.scsh.yml` in this repo, by design.** It is the *tool*, not a
  consumer of itself, so there is nothing to run against it. `scsh init-demo-project` (and
  `installskills`) is what writes a `.scsh.yml` into a demo or target repo.

## Learn more

- **[DEMO.md](DEMO.md)** — the invocation test suite in plain English: follow it (or
  ask your AI to) to watch `scsh` explain every failure mode from its own output.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** — house style and conventions.
- **`scsh help`** — the full command list and the `.scsh.yml` schema.
