# Session browser daemon

`scsh` can run a small HTTP server on **localhost** (default port **7274** — `scsh` on a
numeric keypad) that collects events from every `scsh run` and renders them as an expandable
web UI. Terminal scrollback is painful for parallel container output; the session browser is
the easier way to read build logs, harness output, and per-skill results.

The daemon serves the **local machine only**. It accepts connections on every interface so a
remote caller gets an explicit `403` denial ("scsh daemon serves the local machine only")
rather than a silent connection refusal, but it turns away every non-loopback peer before
reading the request — only clients on the host itself are ever served.

## Commands

```console
scsh daemon start      # persistent — runs until scsh daemon stop
scsh daemon stop       # stop the daemon
scsh daemon restart    # stop then start (persistent)
scsh daemon status     # exit 0 when the daemon is listening
```

During `scsh run`, if no persistent daemon is already running, `scsh` auto-starts an
**ephemeral** daemon. That daemon stays up while runs are active (with periodic pings) and
shuts down **five minutes** after the last client disconnects.

Every `scsh run` gets a session id of six random lowercase letters. The run prints its
clickable deep link URL twice — right after registering (watch live) and again as one of the
very last lines (so a coding agent relaying the tail of the output always surfaces it). It is
reachable while a daemon is listening on that port; start `scsh daemon start` for durable
post-run browsing, or rely on persisted state after `scsh daemon restart`:

```text
http://127.0.0.1:7274/job/abcdef
```

Open it in a browser to see image builds and skills as collapsible sections, with timestamped
harness output and container names.

## What is collected

| Event | Source |
| --- | --- |
| Image build start / output / success / failure | Live board build rows |
| Skill clone / harness phases | Proc notes |
| Container start / stop | Named container around each skill |
| Every stdout/stderr line | Build tail + harness tee (`scsh-run.log` stream) |
| Terminal recording (`.cast`) | asciinema PTY recording of each harness (see below) |

## Terminal recordings (asciinema)

Every harness runs inside a real PTY recorded by asciinema (size from the `terminal:`
block in `.scsh.yml`, default **200×50**). **All five harnesses** run as the genuine
end-to-end interactive TUI — the same screen a human would see: claude, codex, and cursor,
plus opencode (`opencode --prompt`) and grok (`grok "<prompt>"`, its default Build TUI). The
`scsh-tui-record` script (baked into the base image) runs the harness in a tmux session,
records the attached screen, and — when the skill's result file appears — sends the quit
keys (`/exit`, double Ctrl-C) and ends the session.

There is **no screen-scraping**: every consent, trust, and login prompt is skipped ahead
of time by a flag or seeded config, so the recording is clean and a stuck harness surfaces
as a timeout (a real setup bug) rather than being auto-clicked. Per harness:

- **claude** — `--permission-mode acceptEdits` (the `bypassPermissions` consent screen has
  no non-interactive escape); onboarding + workspace trust seeded into the forwarded
  `.claude.json`.
- **codex** — `--dangerously-bypass-approvals-and-sandbox`; `trust_level = "trusted"`
  appended to the forwarded `config.toml`.
- **cursor** — `--force`; its `~/.cursor/projects/<repo-slug>/.workspace-trusted` marker
  pre-created in-container (`--trust` is print-mode-only, and there is no config key).

Missing/invalid credentials fail fast with a clear "log in on the host" error before any
container starts — scsh never tries to drive a login screen.

Each recorded skill — and each image build — is shown as an **inline player** in the
session page. Builds are recorded by scsh itself (`scsh __record-pty` runs the builder
under a real PTY and writes the asciicast), so every build is a cast on every machine —
there is no text-log format. The player is scsh's own **scsh-cast-player** — a first-party,
clean-room component (`src/daemon/html/player/`, MIT like the rest of scsh; no third-party
code or license rides in the browser UI). It has:

- **Playback** — play/pause, timeline scrubbing, and native keyboard: **space** pause,
  **←/→** seek, **&lt;/&gt;** speed, **[/]** jump between chapters (click the player first
  to focus it).
- **Fullscreen** — fills the viewport, fitting the terminal both horizontally and vertically.
- **Chapters** — if a cast has been annotated (below), its chapters show as **markers on the
  timeline** (YouTube-style) plus clickable chapter chips, with a one-sentence **summary**
  above the player.
- **Link at time** — copies a deep link to the standalone player at the current timestamp
  (`/cast/{session}/{proc}/play#t=<seconds>`).
- **⬇ .cast** — downloads the raw asciicast v3. Works **mid-run**: the recording is NDJSON,
  so the daemon serves the bytes written so far (truncated to the last complete line).
- **⬇ .html** — downloads the recording as **one self-contained offline HTML player page**
  (the same rendering `scsh export-cast` does, chapters sidecar folded in when present),
  named `<cast stem>.html`. Hidden until the recording has at least one complete frame —
  the export endpoint 404s on a frameless cast.
- **⬇ session .html** — in the session-page header: downloads **the entire session as one
  self-contained offline page** (`scsh-session-<id>.html`) — a summary header plus every
  recording embedded as its own per-cast export page (annotated casts keep their summary
  and chapters; procs without a recording become note rows). Shown whenever any proc has a
  registered cast.

## In-progress recordings

A run that just started has a registered cast with no complete frames yet — the player
does not error on it. Instead:

- **Placeholder** — until the recording has at least one complete event line, the player
  box (inline embed and standalone page alike) shows a calm *"Recording in progress — no
  frames yet."* note. The moment frames exist, it upgrades to a real player in place.
- **Smooth live growth** — while a proc runs, the daemon probes its recording cheaply on
  the WebSocket tick (a stat plus a tail-parse from a cached offset — tolerant of a
  truncated trailing line) and pushes `cast_growth` messages: `{ "type": "cast_growth",
  "session", "proc", "duration", "running" }`. Each one makes the page fetch the recording
  and **append only the new suffix to the mounted player in place** (`player.append`) — no
  re-creation, no seek, no banner. The seek bar and duration simply grow; a viewer parked
  at the live edge sees the new frames as they land, and one who paused or seeked back is
  never yanked forward. No client-side polling — the server pushes, and only while someone
  is subscribed.
- **Live mode** — a **● Live** toggle (visible only while the proc runs) parks the
  playhead at the live edge, where the player's positional (`tail -f`-style) follow policy
  renders every append immediately. When the proc finishes, a final `running: false`
  notice ends live mode cleanly — one last reload picks up the complete durable cast and
  the toggle is disabled.

All of it degrades gracefully without the WebSocket: pages still load, finished casts
play exactly as before, and the manual ↻ reload button keeps working.

## Cast chapters & summaries (cursor / Composer)

After a run, if the `cursor-agent` CLI and a cursor login are present on the host, `scsh`
annotates each new recording: it renders the cast to a compact timestamped transcript, asks
cursor-agent on the **Composer** model for a one-sentence summary plus 3–8 chapters, and
writes a `<cast>.chapters.json` sidecar next to the recording. When the host also has
**tmux** and **asciinema**, the annotate step runs as a recorded interactive TUI (same
visual path as a skill run) under the session's `casts/` dir; otherwise it falls back to
headless `cursor-agent -p`. Failures show as **annotation failed** on the annotate proc
(best-effort — they never fail the parent skill). When a daemon client is live, annotation
appears as post-skill **annotate** procs on the same job (before the session ends);
standalone `scsh annotate-cast` may register a short `(internal)` session instead.
Background annotations run **fully detached** (their own session, no controlling
terminal), so the launching terminal or agent harness tearing down its process group
cannot kill an annotation that is doing its job. An annotate row whose process still
vanished mid-work (crash, reboot) settles as `annotation_interrupted` — distinct from a
real model watchdog `annotation_timed_out` — and its stale `.annotating` marker expires
after 15 minutes, so the recording is re-annotated by a later run instead of staying
chapterless forever.
The player loads chapters from `GET /cast/{session}/{proc}/chapters` (returns `{}` when
absent). Annotate on demand with:

```console
scsh annotate-cast ~/.scsh/sessions/<session>/casts/<recording>.cast   # override model via SCSH_ANNOTATE_MODEL
```

The job page shows how many casts still lack a sidecar (`N casts finalizing chapters`) and
labels the whole-job export `Chapters pending ⬇` until they land (or `Incomplete job ⬇` while
the job is still running; per-run downloads say `Incomplete run ⬇`). Projects → **Internal** lists synthetic-repo sessions —
`(image builds)` and `(internal)` — separate from real projects and repositories.

## Artifact formats

**Recording — asciicast v3** (`*.cast`). The [asciicast v3](https://docs.asciinema.org/manual/asciicast/v3/)
format: a header object on the first line, then one JSON array per line (NDJSON) — an output
event is `[<interval-seconds:number>, "o", "<text:string>"]` (times are intervals from the
previous event, not absolute). Being line-delimited is what makes a
partial file valid mid-run.

```jsonc
{"version": 3, "term": {"cols": 200, "rows": 50, "type": "xterm-256color"}, "timestamp": 1783108212}
[0.12, "o", "[?25lstarting…\r\n"]
[1.22, "o", "done\r\n"]
```

**Annotation sidecar** (`<cast-stem>.chapters.json`). Written by the annotation pass, served
at `/cast/{session}/{proc}/chapters`. The player uses `summary` for the caption and each
chapter as a timeline marker / jump target:

```jsonc
{
  "summary": "One sentence describing what the session did.",  // string, required
  "chapters": [                                                 // ascending by t; first is 0; [] allowed
    { "t": 0,    "title": "Startup" },      // t: seconds in (number, may be fractional); title: short label
    { "t": 6.5,  "title": "Read the skill" },
    { "t": 9.2,  "title": "Write result JSON" }
  ]
}
```

Field names are `snake_case`; the Rust source of truth for the shape is `CastAnnotation` /
`Chapter` in `src/annotate.rs`. An absent sidecar is served as `{}` (no summary, no chapters).

## Where artifacts live

While the container runs, the cast is served straight from the run dir
(`<run_dir>/tmp/scsh-run.log.cast`, bind-mounted and growing live). When the skill ends,
`scsh run` copies each run's artifacts into that run's own **permanent per-session home**,
`$SCSH_HOME/sessions/<session>/` (default `~/.scsh/sessions/<session>/`) — so a throwaway
caller clone (e.g. `code-beautiful-review` under `tmp/`) cannot wipe session-exportable
recordings, one `ls` names everything a run produced, and one `rm -rf` forgets exactly one run:

| Artifact | Path (under `~/.scsh/sessions/<session>/`) |
| --- | --- |
| Skill recording | `casts/<stem>.cast` |
| Image-build cast | `casts/build-<target>-<stamp>-utc-<nonce>.cast` |
| Annotation sidecar | `casts/<stem>.chapters.json` |
| Harness run log | `logs/<stem>.log` |
| Verbose debug log | `logs/<stem>.debug.log` (claude/grok) · `logs/<stem>.last.log` (codex) |

The stem is `<skill>-<YYYYMMDD-HHMMSS>-utc-<nonce>`. The timestamp alone is not unique — every
skill in one `scsh run` shares it — so the random nonce keeps same-second runs from overwriting
each other. Logs are kept for **every** run (including failures, when they matter most).

`sessions/` is durable by default: ordinary runs never delete from it. Use `scsh gc` to
reclaim old session dirs (dry-run by default; `scsh gc --apply` to delete) — it never
touches `projects/`, `stats.jsonl`, or the daemon redb files. Delete a session's directory
manually to forget that run — nothing else references it (the daemon store keeps its own
copy of the metadata, and stored cast paths simply stop resolving).

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `SCSH_DAEMON_PORT` | `7274` | HTTP listen port (localhost only) |
| `SCSH_HOME` | `~/.scsh` | Dir for the persistent session store (`daemon-<port>.redb`), permanent per-session artifacts (`sessions/<id>/casts/` + `logs/`), and browser-created `projects/` — created on demand wherever it points |

(Every scsh environment variable is listed in one place: README “Environment variables”.)

## Where state lives

The daemon persists its session store in an embedded **redb** database at
**`~/.scsh/daemon-<port>.redb`** (override the `~/.scsh` dir with `SCSH_HOME`). Each session
is one row, so a mutation writes just that session — not a rewrite of the whole store. Only
the daemon opens the DB (redb allows one process at a time); the CLI reads the daemon's mode
from a tiny cross-process marker instead.

Runtime files — the PID lock and the mode marker (`daemon-<port>.pid`, `daemon-<port>.mode`)
and the prune queue — live under the **system temp dir** `$TMPDIR/scsh-daemon/`. Session
history survives a `daemon restart`; the daemon's own uptime/client state starts fresh.

## Zombie-container reaper

A `scsh run` that dies (closed terminal, killed process) takes its inactivity watchdog with
it, and its containers keep running forever. The daemon sweeps every available runtime
(docker/podman **and** Apple `container`) about once a minute for `scsh-*-run-*` containers
that no genuinely live job claims, and stops + removes any that stay unclaimed for ~30
consecutive sweeps (about half an hour); their `/tmp` run dirs go to the regular prune
queue. The wide grace is deliberate — no registration lag, daemon restart, or transient
ping gap can cost a live run its container, while a day-old zombie still dies. A single
claimed sweep resets a container's count. Disable with `SCSH_REAP_CONTAINERS=0`.

## API (for scripts)

- `GET /` — HTML index (Run tab); also `/jobs`, `/projects`, `/setup`
- `GET /job/{id}` — HTML job detail (`/session/{id}` still accepted)
- `GET /cast/{session}/{proc}` — asciicast v3 recording (valid partial file mid-run);
  `?dl=1` for a download attachment
- `GET /cast/{session}/{proc}/play` — HTML player page (scrub, pause, `#t=…` deep links)
- `GET /cast/{session}/{proc}/export.html` — the recording rendered as one self-contained
  offline HTML player page (identical to `scsh export-cast` output; the chapters sidecar is
  folded in when present, and a malformed sidecar exports without chapters). Served as a
  download attachment named `<cast stem>.html`; 404 with an actionable body until the
  recording has at least one complete frame
- `GET /job/{id}/export.html` — the ENTIRE job as one self-contained offline HTML
  page: a summary header plus every recording embedded as its per-cast export page (iframe
  `srcdoc` composition; procs without a recording become note rows). Served as a download
  attachment named `scsh-job-{id}.html`; 404 with an actionable body when the job
  has no exportable recording yet (`/session/{id}/export.html` still accepted)
- `GET /diff/{session}/{proc}` — the packdiff-packed review page for the commits that step
  brought into the caller's branch (one self-contained HTML file: the diff, commits, and
  in-browser comments). Renders inline in a tab; `?dl=1` for a download attachment. Exists
  only for commit-enabled steps whose commits were integrated while `packdiff` (0.4.5
  required; `cargo install packdiff --version 0.4.5 --locked`) was on the
  PATH of the `scsh run` host; 404 otherwise
- `GET /assets/scsh-cast-player.{js,css}` — the first-party player assets
- `GET /api/v1/sessions` — JSON session id list
- `GET /api/v1/session/{id}` — JSON session detail
- `GET /api/v1/session/{id}/fleet` — JSON fleet aggregation for scripts and reduce steps:
  each multi-route skill's rollup (the same shape as the end-of-run `<skill>-rollup.json`
  files under `sessions/{id}/results/`) plus a job-level `verdict` across every fleet —
  route counts (`ok`/`fail`/`pending`), a grade histogram, the mean on the excellent=5 ·
  good=4 · average=3 · poor=2 · bad=1 scale, and total findings. Computed live from the
  session's procs, so it serves mid-run; `verdict` is `null` on a job with no fleets.
  Descriptive only — scsh reports the numbers and leaves any approval bar to the caller
- `GET /api/v1/images` — JSON status of every scsh image (base + one per harness) on the
  detected runtime: exists, up-to-date (fingerprint match), created, size (created/size are
  `null` on Apple `container`, which has no inspect formatter)
- `POST /api/v1/images/build` — body `{"harnesses": [name…], "rebuild_base": bool, "force":
  bool}` (all optional; no harnesses = all). Spawns a detached `scsh build-images --session
  <id>`, pre-creates that session, and returns `{"ok":true,"session":id}` so the caller can
  deep-link it. One build at a time — a concurrent request gets 409. Stderr is captured and
  the session is reconciled on exit (same as `jobs/start`), so a build that dies before it
  registers becomes a failed session with the error — never a stranded "running" one. Each
  image build is recorded as a cast by scsh's own PTY recorder (same ASCII-cinema player
  as skill runs) — no host tooling required.
- `POST /api/v1/session/start`, `/register`, `/deregister`, `/ping`, `/proc/*`, `/container`
  — event ingestion (used by `scsh run`); `/proc/cast` registers a proc's recording path,
  `/proc/diff` the packed commits-diff page a step's integration produced
- `POST /api/v1/session/stop` — body `{"session":"…"}`. Force-stop a stalled job from the
  session page: stop every still-named container, SIGTERM (then SIGKILL) the `scsh run`
  process when its PID is known, and mark incomplete procs failed with `force_stopped`.
  Idempotent on an already-ended session.
- `POST /api/v1/proc/stop` — body `{"session":"…","proc":N}`. Force-stop ONE run's container;
  the rest of the job continues. The proc settles failed with `force_stopped`.
- `POST /api/v1/proc/restart` — body `{"session":"…","proc":N}`. Force-restart ONE skill run:
  record a restart marker for the owning `scsh run`, kill this attempt's container, and settle
  the proc failed with `force_restarted`; the runner consumes the marker and respawns the route
  as a fresh attempt (a new proc row that supersedes this one). Builds and annotations are
  refused (400), as is a session whose run client is gone (409).
- `POST /api/v1/repos/open` — body `{"path": "…"}`. Validate the path is a git repo, report
  whether it is clean, discover the harness definitions available to it, and remember it as an
  open repo. `{"ok":true,"repo":…,"clean":bool,"dirty":[…],"defs":[…]}`, or `{"ok":false,"error":…}`
- `POST /api/v1/harness-defs` — body `{"repo": "…"}`. Re-discover an open repo's definitions
  (a refresh). `{"defs":[…]}`
- `POST /api/v1/jobs/start` — body `{"repo": "…", "def": "…", "params": {…}}`. Enforce one job
  per directory, validate the definition + params, then spawn a detached `scsh run --def <name>`
  in the repo with the params as environment and a pre-created session id.
  `{"ok":true,"session":id}`; a second job in the same repo (or a dirty tree) gets 409
- `POST /api/v1/jobs/restart` — body `{"session":"…","mode":"resume"|"scratch"}`. Stop the old
  run (exactly `session/stop`; idempotent on an ended job), then start the SAME job fresh from
  the session's persisted start recipe (`$SCSH_HOME/sessions/<id>/start.json` — def/profile +
  params; both web- and CLI-started runs write one). `mode:"resume"` (workflow jobs only)
  spawns the fresh run with `--resume-from <old id>`, so every step the old session completed
  is restored from its persisted result and only the unfinished steps run; the default
  (`"scratch"` or absent) runs everything anew. Answers `{"ok":true,"session":"<new id>"}`.
  The failed-job page's "Restart remaining" / "Restart from scratch" buttons call this. The
  old session records `restarted_as` and the chain's supervisor state (attempt count,
  retries budget, breaker memory) is inherited by the fresh session.
- `POST /api/v1/repos/pick` — pop the host's native folder chooser (the daemon is local) and
  return the chosen path: `{"ok":true,"path":…}`, `{"ok":false,"cancelled":true}`, or
  `{"ok":false,"error":…}` on a headless host (type the path instead)
- `GET /api/v1/repos` — the opened repositories and any repos that have jobs, each with its
  jobs (sessions) grouped underneath

## The job supervisor

Every job is presumed worth finishing: when it terminally fails — step budgets exhausted,
the run process died, the host rebooted — the daemon schedules a job restart at 5m·2ⁿ
backoff (capped at 60m, jittered) and fires it through `jobs/restart` (`mode=resume` for
workflow jobs, so completed steps restore instantly and only the failed frontier
re-executes). Persisted supervised sessions whose run died with the daemon read as failed
on the next tick and schedule normally — a 3am host reboot needs no adoption pass.
Sessions persisted before the retries budget existed parse back to a zero budget, so a
daemon upgrade never resurrects history.

The one knob is the job's **retries budget** — 10 by default for every job, set per start
with `scsh run --retries N`, the `retries` field on `jobs/start`, or the browser start
form's retries input; `0` opts a job out of supervision. Eventually is not infinitely: the
chain stops loudly when any ceiling trips, with the reason in the session's supervisor
state and the failures log:

- **Retries budget**: `N` restarts per chain (default 10).
- **Job-level breaker**: 3 consecutive runs failing at the same step for the same reason —
  a deterministic failure (or an scsh bug), not a provider incident.
- **A human's stop**: force-stopping a job cancels its supervision permanently; a manual
  stop IS supervision, and it wins.

The job page's meta shows the policy in force (`Retries · attempt 3/10 · restarting in
4m`, a link to the replacement session, or exactly why it gave up), and every decision is a
`supervisor_scheduled` / `supervisor_restart` / `supervisor_gave_up` line in
`scsh failures`. `SCSH_JOB_BACKOFF_INITIAL_SECS` shrinks the first delay for tests and
RESILIENCE-DEMO.md.

## Harness definitions

A **harness definition** is a parameterized, runnable job — the unit the "start a job from the
browser" flow (and `scsh run --def <name>`) runs. Definitions come from three places, later
sources shadowing earlier ones by name (repo > home > built-in):

- **built-in** (embedded in the binary, always available): `doctor` (report which agent images
  and credentials are present, then run a trivial end-to-end confirm task), `add` (an a+b math
  self-test), `research` (a trivial tool-calling demo), and `fruits` (a workflow demo, below).
- `~/.harness/<name>.yml` — the running user's personal definitions.
- `<repo>/.harness/<name>.yml` — definitions that ship with a repository.

A definition is **either** a flat one-shot task **or** a workflow. Note the YAML is
**block-form only**: the minimal reader has no inline flow collections, so write nested mappings
and lists as indented blocks (not `{ … }` / `[ … ]`), except `needs:` which is a comma-separated
scalar.

A flat `.harness/<name>.yml`:

```yaml
description: "Add two integers A and B and verify the sum."   # one line, shown in the list
params:                                                       # each forwards as an env var
  A:
    type: int
    default: "2"
    description: "First addend"
  B:
    type: int
    default: "3"
task: |                                                       # passed to the harness as a custom prompt
  Read A and B from the environment, compute A+B, write {"sum": …} to $SCSH_RESULT, and assert.
invocations:                                                  # the agent matrix (as in .scsh.yml)
  opencode-gpt:
    harness: opencode
    model: openai/gpt-5.4-mini-fast
  claude-sonnet:
    harness: claude
    model: sonnet
```

Param types are `string`, `int`, `bool`, and `enum` (with a comma-separated `choices:`). A param
with a `default:` is optional; without one it is required unless `required: false` is set. The
`task` / workflow-step `prompt` body is handed to the harness as a **custom prompt** (harnesses
already accept free-form prompts) — no synthetic `.skills/…/SKILL.md` is written. Repo skills
under `.skills/` still run the skill-file path. `scsh run --def` requires a clean repo just like a
normal run.

## Workflow definitions (`steps:`)

A definition with `steps:` (instead of `task:`+`invocations:`) is a **workflow** — a DAG of
steps scsh runs in order, passing typed output from one step into the next. Each step is a
context-free unit: it names an `agent`, a `prompt`, typed `output` fields, and `inputs` bound
from run params (`params.NAME`) or an upstream step's output (`stepid.field`). scsh resolves the
wiring and hands each input to the step as a plain environment variable, and appends the I/O
contract (which env vars carry the inputs, and the exact JSON to write to `$SCSH_RESULT`) to the
prompt — so a step knows only about itself.

```yaml
description: "Categorize words, then sort each list."
params:
  WORDS:
    type: string
    required: true
steps:
  categorize:
    agent:
      harness: claude
      model: sonnet
    inputs:
      WORDS: params.WORDS         # bind an input from a run param
    prompt: |
      Split the WORDS input into fruits and vegetables (comma-separated).
    output:                       # the typed result this step must write
      fruits:
        type: string
      vegetables:
        type: string
  sort_fruits:                    # sort_fruits and sort_vegetables run in parallel
    needs: categorize             # DAG edge (comma-separated for several)
    agent:
      harness: claude
      model: sonnet
    inputs:
      LIST: categorize.fruits     # bind an input from an upstream output field
    prompt: |
      Sort the LIST input alphabetically.
    output:
      sorted:
        type: string
```

- **`needs:`** — the DAG edges; a step runs once every step it needs has run (or been skipped).
- **`when:`** — an optional gate: a block map of `reference: value` (or `reference: { gte: 3 }`)
  entries, ALL of which must hold (AND) for the step to run. Ops: `eq ne lt lte gt gte in`.
  Disjunction is expressed as separate steps, so there is no OR combinator and no expression
  language. A false gate — or a skipped dependency — skips the step.
- **`output:`** — validated after the step: a missing or mistyped field fails the step (and any
  branch that needs it). Only these fields are visible to downstream `inputs`/`when`.
- **`commits: true`** — optional; same contract as a skill's `commits: true`: commits the step
  makes inside the clone are rebased onto the caller's branch (and packed with packdiff when
  available). The step prompt should tell the agent what to commit.

Every `inputs:`/`when:` reference must resolve to a declared param or an upstream step's declared
output field, and any referenced step must be in `needs:` — checked when the definition is
parsed, so a workflow that could branch on a value no step produces is rejected up front.

**Job-page dependency graph.** Workflow sessions carry an optional `workflow` object on the
session snapshot (`nodes: [{ id, proc_index, order, needs, conditional, when_summary }]`). The job page
renders a live DAG (HTML nodes + SVG edges) above the proc list: click a node to open that
step's panel (`#task-<id>`). Steps with a `when:` gate show a small **when** marker whose
tooltip states the condition (e.g. `Runs only if probe_credentials.ok = true`). Topology comes
only from declared `needs` — never from proc notes. Flat jobs and older sessions without
`workflow` keep the list-only page. `stalled` is
a derived display state when the session heartbeat is stale (`SESSION_STALE_SECS`), not a
persisted proc status.

**Session scratch.** A workflow's per-step result files live under a session directory
`<scratch>/scsh/<session>/`, where `<scratch>` is `.harness/tmp` when that is gitignored (some
repos prefer it) else `tmp`. scsh refuses to run unless the scratch it uses is gitignored, so a
run never dirties the tracked tree.

## Start a job from the browser

The session index page is organized into tabs: **Jobs** (every run), **Projects** (current jobs grouped by project/repository; opened
repos and their jobs), **Start a job**, and **Containers** (the images panel).

Under **Start a job**: type or paste a path (or click **Pick…** for the native folder chooser)
and **Open**. The daemon validates the repo with the *same* checks the run makes — it must be a
git repo that is **committed, clean, and has a gitignored scratch dir** (`tmp/` or `.harness/tmp`)
— and reports `runnable` plus any **blockers**. If it is not runnable, the blockers are shown and
**Start** stays disabled, so a doomed job is never started. Otherwise, pick a definition (its
agent routes and a workflow badge are shown), fill its param form, and **Start job** — which
posts `/api/v1/jobs/start` and deep-links to the spawned session, the same live board a console
run gets, because the job *is* an ordinary `scsh run --def`. The daemon runs **at most one job
per directory** at a time.

**No hidden jobs, no silent failures.** A started job's session is bound to its process: the
daemon captures the spawned run's output and, when the process exits, reconciles the session — a
run that finished normally is left alone, but one that died before it ever registered becomes a
**failed** session showing the captured error, never a stranded "running" one.

Two built-in definitions make good demos: **`doctor`** (no params — confirms the agent images
are built and each agent's credentials proxy through, then runs a trivial end-to-end task) and
**`fruits`** (the workflow demo — give it `WORDS` like `apple, carrot, pear, onion` and watch
`categorize` fan out into `sort_fruits` and `sort_vegetables` running in parallel). For a
**fake PR** in the Web UI (DAG + packdiff ⇄ commits diff with a Description panel), run
**`greet`**: `scaffold` → `implement` → `describe` seeds a broken `greet()`, fixes it, then
commits `PR-DESCRIPTION.md`. For the **minimal** one-shot version (just a feature note +
`PR-DESCRIPTION.md`, no DAG), run **`demo-pr`** — four agent routes (claude / codex / cursor /
grok), each `commits: true`.

## Images panel

The session index page ends with an **images** table: one row per scsh image (`scsh-base`
plus one per harness), with its status — **up to date** (fingerprint matches this scsh
build's embedded Dockerfile), **stale** (exists but fingerprint differs, e.g. after an scsh
upgrade), or **missing**. Select rows and press **Build selected** (or **Build all**);
optional toggles force-rebuild the base image (`--no-cache`) or rebuild images that are
already up to date. The buttons call `POST /api/v1/images/build` and navigate straight to
the spawned `scsh build-images` session, where each image build streams as a proc row —
the same view a run's build rows get. Builds are TUI-first: each image build runs under
a host `asciinema` PTY so Docker BuildKit / Apple `container` show their native progress,
and the session page embeds the cast player (identical to a skill recording).

## Assumptions

- **Assumed:** Port 7274 is acceptable as the default (`scsh` keypad mnemonic); override
  with `SCSH_DAEMON_PORT`.
- **Assumed:** Localhost-only binding is sufficient — no auth layer on the HTTP server.
- **Assumed:** Ephemeral idle timeout is five minutes with no connected `scsh run` clients.
- **Assumed:** Session ids are six lowercase `[a-z]` letters, matching Apple-container
  nonce style.
- **Assumed:** The daemon is best-effort — if it cannot start, `scsh run` still proceeds
  without the browser URL.

## Resetting the store

The daemon retains up to 200 sessions (each proc keeping up to 5000 output lines) in the
redb store. Because it writes only the sessions that changed — not the whole store each tick
— the store stays small and event POSTs don't stall (this replaced the earlier scheme that
re-serialized one growing JSON file and could reach tens of megabytes). To wipe session
history, stop the daemon and delete its DB:

```console
scsh daemon stop
rm ~/.scsh/daemon-${SCSH_DAEMON_PORT:-7274}.redb
scsh daemon start
```

This clears session history only; `.cast` recordings live under `~/.scsh/sessions/<id>/casts/`
(override with `SCSH_HOME`) and are unaffected. To reclaim disk from old session dirs (or
legacy top-level `casts/` / `recordings/`), use `scsh gc` (dry-run) / `scsh gc --apply`
(`--legacy` for the pre-sessions layout).

## Demo

```console
./scripts/daemon-demo.sh
```

Or manually:

```console
cargo build --release
./target/release/scsh daemon start
./target/release/scsh daemon status
# open http://127.0.0.1:7274/ after a scsh run
./target/release/scsh daemon stop
```

## Tests

```console
cargo test
```

Integration tests cover `daemon start` / `status` / `restart` / `stop` on localhost. Unit tests cover
the event model, JSON roundtrip, session id format, the harness-definition schema/discovery, and
the open-repo / start-job / one-job-per-directory endpoints. [`DAEMON-JOBS.md`](DAEMON-JOBS.md) is
a followable harness for the "open a repo & start a job from the browser" path.

## Manual verification (`scsh run` → browser)

Automated tests do not drive a full attended `scsh run` with browser attach. From the
**`scsh` repo root** after `cargo build`, capture the binary you just built:

```console
export SCSH_BIN="$PWD/target/debug/scsh"
```

The steps below use `$SCSH_BIN` so they work after `cd` into a scratch directory.

1. `$SCSH_BIN daemon stop` (clean slate) then `$SCSH_BIN daemon start`.
2. In a **fresh scratch directory**, scaffold a demo project: `$SCSH_BIN init-demo-project`
   (or use any git repo that already has a `.scsh.yml` with a short profile). Then run
   `$SCSH_BIN run` in that directory and note the session URL printed on stderr
   (or open `http://127.0.0.1:7274/`).
3. Confirm the browser shows the session, proc rows appear as skills run, harness
   output streams into the proc panel, and proc status updates to ✓/✗ on finish.
4. When the run ends, confirm the session moves to “ended” on the index page.
5. `$SCSH_BIN daemon restart` — daemon comes back and `GET /` still serves the index page.
6. `$SCSH_BIN daemon stop` — daemon exits and the port is closed.

For ephemeral mode, skip step 1: a short `$SCSH_BIN run` alone should spawn the
daemon, attach, and shut it down after the run disconnects and the idle timeout elapses.
If idle shutdown does not run, use `$SCSH_BIN daemon stop` as cleanup.

7. Remove the scratch directory and any daemon artifacts under the system temp dir, for example:

   ```console
   rm -rf "$SCRATCH_DIR"
   rm -f  ~/.scsh/daemon-${SCSH_DAEMON_PORT:-7274}.redb
   rm -f "$TMPDIR/scsh-daemon/daemon-${SCSH_DAEMON_PORT:-7274}.pid" \
         "$TMPDIR/scsh-daemon/daemon-${SCSH_DAEMON_PORT:-7274}.mode"
   ```
