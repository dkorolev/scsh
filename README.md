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
agent and ask it to *follow the steps in [`DEMO.md`](DEMO.md)*. (A second walkthrough,
[`GLOBAL-SKILLS-DEMO.md`](GLOBAL-SKILLS-DEMO.md), demos `--override-dot-scsh-yml`: an
external bundle's skill installed **globally inside the container** — claude and cursor
discover it natively in their user-level skills dirs — against a repo that ships no
`.scsh.yml` and no `.skills/` at all. A third, [`AGENT-FLEET-DEMO.md`](AGENT-FLEET-DEMO.md),
is agent-first: the agent you hand it to drives `scsh` itself — fanning "explain this
codebase" out to claude, codex, and cursor as three parallel agent jobs, waiting on the one
blocking `run`, then synthesizing the three JSON results. It needs no path at all: tell any
agent with `scsh` on its PATH to *run `scsh demo agent-fleet` and follow the steps it
prints* — `scsh help agent` is the compact contract behind it.) It will build a tiny
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
scsh probe [profile]       Which harness·model routes are runnable here (exit 0 iff at least one is).
scsh init-demo-project     Scaffold AND commit a demo: .scsh.yml + example skills + tmp/ ignore.
scsh installskills [url]   Install skills — bundled, or a git repo's (merges its .scsh.yml).
                           --global installs machine-wide under ~/.scsh instead (no repo needed).
scsh updateskills  [url]   Reinstall skills, overwriting files — bundled or a git repo's (--global too).
scsh help                  Show help (includes the schema).
scsh version               Show the version (with the build's git short hash, +`-dirty`).
scsh daemon start|stop|restart|status
                           Session browser on http://127.0.0.1:7274 (override with SCSH_DAEMON_PORT).
                           scsh run auto-starts an ephemeral daemon and prints a session URL.
```

See [`DAEMON.md`](DAEMON.md) for the session browser API and demo script.

**Harness definitions & starting a job from the browser.** Besides `.scsh.yml` skills, `scsh`
runs **harness definitions** — parameterized jobs in `.harness/<name>.yml` (in the repo or
`~/.harness/`) plus built-ins (`doctor`, `add`, `research`, and the `fruits`, `code-review`,
`arith`, `commit-summary`, and `greet` workflows — `fruits` fans out into two sorters, then
commits `README.md`,
`FRUITS.md`, and `VEGGIES.md` from a final fan-in step; `greet` is the multi-step fake-PR demo:
scaffold a broken `greet()`,
fix it, commit `PR-DESCRIPTION.md` — plus the flat `demo-pr` definition: one shot that commits a
tiny feature note + `PR-DESCRIPTION.md` on claude/codex/cursor/grok). A flat
definition declares a `description`, typed `params` (which become environment variables), a
`task` body, and an `invocations:` agent matrix. A **workflow** definition instead declares
`steps:` — a DAG where each step runs an agent, writes typed `output` (plus any declared
`artifacts:` — plain files copied back beside its result, e.g. a `summary.txt`), optional
`commits: true` (same as a skill — rebase the step's commits onto your branch / packdiff), and feeds later steps
whose `inputs` bind to `params.NAME` or `stepid.field` (`needs:` gives the edges, `when:` gates a
step). Run one from the console with `scsh run --def <name>` (params from the environment), or,
when the daemon is up, open a repository in the browser (type/paste a path or use the native
folder picker) and start a job from a rendered parameter form — the daemon runs at most one job
per directory. Here the word "harness" means the runnable definition; the CLI it dispatches to
(claude/codex/opencode/…) is the definition's *agent*. See [`DAEMON.md`](DAEMON.md) and
[`DAEMON-JOBS.md`](DAEMON-JOBS.md).

`commit-summary` is the file-handoff example: Claude, Codex, and Grok independently analyze
commits from the past `DAYS` days (`7` by default), commit three reports, then each reads all
three files and corrects its own report. Cursor Composer reads the corrected files and commits
one `COMMIT-SUMMARY.md`. Run it with the default window or override it from the environment:

```sh
scsh run --def commit-summary
DAYS=30 scsh run --def commit-summary
```

Failed tasks retry automatically up to five times under a wall-clock budget with
exponential backoff (see `scsh help def`, "Retries and resume"), and every job is presumed worth finishing: on
terminal failure the daemon's supervisor restarts it — resuming completed workflow steps —
up to the job's restart budget (25 by default; `scsh run --retries N`, or 0 to opt out).
A failed workflow job also restarts by hand from the
browser — "Restart remaining" reuses every completed step's result — or from the console
with `--resume-from <session>`. See [`RESILIENCE-DEMO.md`](RESILIENCE-DEMO.md) for the
agent-followable walkthrough.

The built-in `big-beautiful-build` workflow is the browser's complete feature factory: open an existing clean repository or create a new project, paste the full feature brief into its multiline form, and start the job. Cursor Auto executes the canonical `big-beautiful-build` skill — which lives in [dkorolev/beautiful-skills](https://github.com/dkorolev/beautiful-skills), not in the binary: the definition resolves it from the repo's `.skills/` or the machine-wide install, and is listed only where it is installed — commits working code, a runnable demo, documentation, and verification. The job page preserves the structured result and commits diff; the full report is copied into the repository's job scratch directory. No terminal is required to start or follow the build; see [`DEMO-BIG-BEAUTIFUL-BUILD.md`](DEMO-BIG-BEAUTIFUL-BUILD.md).

The built-in `gorgeous-pipeline` workflow prepares the current branch, runs the five-specialty Opus/Codex/Cursor review fleet, and loops through fixes until the score bar passes: Opus 4.8 orchestrates the loop, applies the fixes, and journals the decisions, while Opus 4.8, Codex Spark, and Cursor Auto grade every profile independently. Whatever a fix cycle deliberately declines is journaled as a `PR-DECISION-<topic>.md` note that the next cycle's reviewers read before grading, so a settled question is argued once instead of re-litigated every round. When, from the third round on, everything still holding a route below the bar is a journaled human-adjudication item (split-the-PR requests, product direction) with no poor grades and nothing blocking, the loop exits honestly as `approved_with_reservations` — listing each reservation for the human — instead of grinding the iteration backstop. Every one of its 30 review steps references the same canonical reviewer body embedded for `scsh installskills` from this repository's `.skills/` (originally derived from [dkorolev/code-review-skills](https://github.com/dkorolev/code-review-skills)); `scsh` appends only the workflow's grade/comments output contract. The reviewers inspect commits, diffs, source, tests, documentation, and repository guidelines statically — they never build, run, lint, format, test, execute repository scripts, or invoke the product.

**Installing skills.** With no arguments, `scsh installskills` installs all five code-review specialties, their 15-route `code-review` profile, and `scsh-harness-demo-and-selftest` into the repo's `.skills/` — and deliberately nothing more: the delivery-pipeline skill families live in their own repositories and install from source, so the bundle can never drift from them. Give the command one or more **git URLs** to install another repository's skills (installed in order, as if you ran the command once per repo):

```sh
scsh installskills https://github.com/dkorolev/beautiful-skills
# several at once — installed in order, landing as one reviewable diff:
scsh installskills https://github.com/dkorolev/beautiful-skills https://github.com/dkorolev/code-review-skills
```

Like a real run, `installskills`/`updateskills` insist on a **clean working tree** (so the install
is a reviewable diff, not mixed into unrelated work) and make sure **`/tmp` is gitignored** before
writing, so the repo is run-ready afterward.

**Installing machine-wide.** `scsh installskills --global` needs no git repo at all: skills land
under **`$SCSH_HOME/.skills/`** (default `~/.scsh/.skills/`), their profile blocks merge into the
**global manifest** `$SCSH_HOME/.scsh.yml`, and each installed skill is symlinked into the
user-level skills dir of every coding agent already present on the machine (`~/.claude/skills`,
`~/.cursor/skills`, `~/.codex/skills`, ... — detected by their home dot-dirs; none are planted).
Both global install commands refuse to proceed when one of those agent directories contains a
real local copy of an `scsh`-managed skill: move or remove the shadow copy and rerun, and `scsh`
will create a per-skill symlink to the canonical `$SCSH_HOME/.skills/` copy. This preflight occurs
before the canonical skill files or manifest are changed.
From then on, `scsh run`/`list`/`check-profile` in **any** git repo fall back to the global
manifest for profiles the repo's own `.scsh.yml` does not declare (skill bodies are injected into
the run clone, exactly like `--override-dot-scsh-yml` — the target repo stays clean), so the full
machine setup for the review fleet is just:

```sh
cargo install scsh
scsh installskills --global
cd ~/any/repo && scsh probe code-review && scsh run code-review
```

**Pinning the base commit.** A skill that reads the committed range `origin/main..HEAD`
inside its container — every code reviewer does — sees an `origin/main` that is whatever
your **local** `main` points at. When local `main` is stale, or you want the range measured
from some other commit entirely, pass **`scsh run … --base <ref>`**: the run clone's `main`
(or `master`, when that is the repo's mainline) is repointed at `<ref>` for that run only,
so the range is exactly base-vs-`HEAD`. Your own repository is never touched; nothing is
fetched. The ref is resolved once, up front, so a typo fails the run instead of fifteen
containers, and it is refused when the repo has neither a `main` nor a `master` branch or
when you are standing on that branch (whose diff against itself is empty). A pinned base is
part of the result cache key, and it survives a daemon job restart.

`<ref>` is **any git revision this repository already has**: a commit sha (full or short), a
branch, a tag (annotated tags are peeled to their commit), or a form like `HEAD~3`. Nothing
is fetched, so `origin/main` is only as fresh as your last `git fetch`.

```sh
scsh run code-review --base origin/main      # the upstream tip, as of your last fetch
scsh run code-review --base 5b8a5e7          # a specific commit
scsh run --def gorgeous-pipeline --base v1.38.0   # a release tag
```

`scsh probe [profile]` is the runtime-free companion: it reports which of the selected skills'
harness·model routes are actually runnable on this host (agent CLI installed and authenticated,
opencode models listed), deduped across skills, and exits 0 iff at least one route is — so agents
and scripts gate a fleet run on it instead of hand-rolling per-CLI auth checks.

**If the source repo has its own `.scsh.yml`, that manifest drives the install.** `scsh`
validates it first (and stops if it's malformed), then for every skill it lists — except
the **authoring-only** ones (marked **`autoinstall: false`**, *or* named with the
**`internal-`** prefix, e.g. a repo's own self-check skill) — it copies the skill folder
*and* merges that skill's YAML block verbatim into **your** `.scsh.yml` (same schema,
including `invocations:` matrix skills). `installskills` leaves existing skill keys
untouched and warns when one conflicts; the explicitly overwriting `updateskills` refreshes
existing source blocks too, so profile and route changes migrate with the skill files. The skills are then runnable
immediately: a default skill on `scsh run`, a profiled one on `scsh run --profile
<name>`. Matrix skills expand to `{skill}-{route}` invocations at run time. Skills the manifest doesn't list are skipped
(the manifest is the shipping list). Without a source `.scsh.yml`, `scsh` simply installs every `.skills/<name>/`
folder it finds (no manifest merge).

Either way it wires up the five host symlinks (`.claude/skills`, `.codex/skills`,
`.cursor/skills`, `.opencode/skills`, `.agents/skills` → `../.skills`), and never clobbers
a file that differs from the source (an identical one is simply "already installed"). Both
commands refuse before changing the repository when one of those host paths is a real local
copy instead of a symlink; move or remove the conflicting path and rerun. Use
`scsh updateskills [url]` to overwrite skill files and their existing manifest blocks with the source's version.

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
deep link `http://127.0.0.1:7274/job/abcdef` URL printed at the end of `scsh run`.
Workflow jobs also get a live **dependency graph** on that page (nodes + edges from declared
`needs`), above the usual step panels.

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

**Apple Containers (macOS default):** Apple's `container build` sends the Dockerfile in a
gRPC header with a **16 KB hard limit**
([apple/container#735](https://github.com/apple/container/issues/735)). Larger files fail with
the opaque `Stream unexpectedly closed` / `Transport became inactive`. `scsh` keeps
`src/Dockerfile` under 15 KB, **comment-strips** it before every Apple build, refuses to start
a doomed build with a clear error, and rewrites those opaque failures into a builder-reset
hint. Prefer a healthy BuildKit (`container builder start --cpus 6 --memory 8G`) for the
large base image. If Apple Containers is unavailable, `scsh` automatically falls back to
Docker and then Podman.

The base is glibc Debian (not musl Alpine) precisely so these prebuilt toolchains install and
run without friction. The image is large (a few GB) and **built once, then cached** and reused
across runs — the first `scsh run` (or any change to the Dockerfile) rebuilds it.

## What you need

- A **Rust toolchain** (`cargo`) **1.89 or newer** to build the binary — install via
  [rustup](https://rustup.rs). Distro-packaged toolchains (e.g. Ubuntu's `apt install cargo`)
  are typically older and fail the build.
- **`git`** on your `PATH`.
- A **container runtime**: Apple `container` → Docker → Podman on macOS; Docker → Podman on Linux.
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

## Environment variables

The one place they are all listed. Host-side knobs, all optional:

| Variable | Default | Meaning |
| --- | --- | --- |
| `SCSH_HOME` | `~/.scsh` | scsh's durable home: the daemon session store, permanent per-session artifacts (`sessions/<session>/casts/` + `logs/` — recordings, build casts, harness logs, keyed by the run that made them), and browser-created `projects/`. Created on demand (by the daemon too) wherever it points. |
| `SCSH_DAEMON_PORT` | `7274` | Session browser port (localhost only). |
| `SCSH_RUNTIME` | auto | Force the container runtime: `container` (Apple), `docker`, or `podman`. |
| `SCSH_GIT_TRANSPORT` | auto | `1` forces the git push/clone transport, `0` forces the bind-mount clone (ignored on Apple Containers, which always use the transport). |
| `SCSH_GIT_HOST` | route gateway | Git-daemon host IP as seen from inside the container. |
| `SCSH_GIT_PORT` | ephemeral | Git-daemon port on the host. |
| `SCSH_KEEP_RUNS` | off | `1` keeps every `/tmp/scsh-*-run-*` clone (and skips the stale sweep). |
| `SCSH_REAP_CONTAINERS` | on | `0` disables the daemon's zombie-container reaper: it destroys `scsh-*-run-*` containers that stay unclaimed by any live job for ~30 consecutive minutes of once-a-minute sweeps — orphans left by a killed `scsh run`. |
| `SCSH_NO_RETRY` | off | `1` disables the single automatic retry of transient failures. |
| `SCSH_QUIET` | off | `1` runs harnesses at their default log level (output is still teed to the run log). |
| `SCSH_NO_CLAUDE_AUTH` / `SCSH_NO_OPENCODE_AUTH` / `SCSH_NO_CODEX_AUTH` / `SCSH_NO_GROK_AUTH` / `SCSH_NO_CURSOR_AUTH` | off | `1` skips forwarding that harness's host credentials into containers. |
| `SCSH_ANNOTATE_MODEL` | `gpt-5.6-luna` | Model `scsh annotate-cast` drives via Codex. |
| `SCSH_STATS_FILE` | `~/.scsh/stats.jsonl` | Where run statistics are journaled. |
| `SCSH_HARNESS_HOME` | `~/.harness` | User-level harness-definition directory. |
| `SCSH_BIN` | self | Path to the scsh binary the daemon re-execs (tests/packaging override). |

Host credentials scsh reads (never stored, forwarded per-run): `CLAUDE_CODE_OAUTH_TOKEN`,
`OPENAI_API_KEY`, `XAI_API_KEY`, `CURSOR_API_KEY` — plus each CLI's own login files.

Inside every container, scsh sets the skill contract: `SCSH=1`, `SCSH_RESULT` (the result
file path), and `SCSH_RUN_LOG` (the teed harness log).

## Learn more

- **[DEMO.md](DEMO.md)** — the invocation test suite in plain English: follow it (or
  ask your AI to) to watch `scsh` explain every failure mode from its own output.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** — house style and conventions.
- **`scsh help`** — the full command list and the `.scsh.yml` schema.
