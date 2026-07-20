# The proc lifecycle

A **proc** is one row on a job page: a single execution attempt of one route, one image
build, or one annotation. `scsh` records six proc states and one job-level lifecycle derived
from them. This document is the contract — what each state means, who may set it, and which
transitions are legal. [`DAEMON-JOBS.md`](DAEMON-JOBS.md) covers the job-level rules that sit
on top; [`DAEMON.md`](DAEMON.md) covers the endpoints.

Read this before adding a state, a terminal path, or a renderer. The states are defined in
`src/daemon/model.rs` (`ProcStatus`) and mirrored in the hand-maintained browser client
(`src/daemon/html/client_js.rs`), with unit tests pinning the parity.

## The states

| State | Meaning | Terminal |
| --- | --- | --- |
| `waiting` | The row exists; its process has not started. A **scheduled future**, not a running thing. | no |
| `running` | The process is live. | no |
| `ok` | Finished successfully. | yes |
| `graceful` | Produced a valid durable result, but its exit or container teardown was unreliable. Counts as success for dependencies and job outcome; rendered orange so the wrinkle stays visible. | yes |
| `fail` | Finished unsuccessfully, or was settled as failed. Always carries a `fail_reason`. | yes |
| `skipped` | Decided but never run — a workflow step gated off by `when:`, or downstream of a skipped step. | yes |

`waiting` is the state that repays attention. It is the only non-terminal state with **no
process behind it**, so nothing about the operating system can settle it: a `running` proc
whose process dies is caught by a dead-PID sweep, but a `waiting` proc has no PID to be
dead. It is settled only by the run process advancing it, or by an explicit sweep.

## Who sets what

Two writers, and they own different transitions.

**The run process** owns the forward path. It registers each attempt (`proc/add`, always as
`waiting`), starts it (`proc/start` → `running`), and finishes it (`proc/finish` → `ok` /
`graceful` / `fail` / `skipped`). The daemon does not advance a proc on its own during a
healthy run.

**The daemon** owns settlement — turning non-terminal rows terminal when the run cannot or
will not. It never moves a proc backwards, and it never overwrites an already-terminal row.

The two communicate in one direction only: the run process **posts** to the daemon. There is
no daemon→runner socket. Anything the daemon needs to ask of a runner travels as a marker
file under the system temp dir (`src/daemon/paths.rs`) which the runner polls:

- `restart-requests/<session>-<proc>` — respawn this one route.
- `cancel-requests/<session>` — abandon this whole job.

## Legal transitions

```
                  proc/add
                     │
                     ▼
                 ┌─────────┐   proc/start   ┌─────────┐
                 │ waiting │───────────────►│ running │
                 └────┬────┘                └────┬────┘
                      │                          │
                      │      proc/finish         │  proc/finish
                      │  (skipped: never ran)    │
                      ▼                          ▼
              ┌───────────────────────────────────────────┐
              │      ok · graceful · fail · skipped        │  terminal
              └───────────────────────────────────────────┘
                      ▲                          ▲
                      └──── settlement (fail) ───┘
                         deregister · dead PID ·
                         stop · daemon restart
```

Rules that hold everywhere:

- **A browser kill wins the teardown race.** Once a proc carries a stop- or restart-shaped
  `fail_reason` (`stop_requested`, `force_stopped`, `restart_requested`, `force_restarted`),
  a late `proc/finish` for it is dropped rather than applied — the human's action is the
  authoritative outcome, not whatever the dying container reported afterwards. The single
  exception is deliberate: a **restart** whose original attempt came back `ok` or `graceful`
  lets that success stand, because the runner consumes the restart marker without respawning,
  so the finished attempt is the only one that will ever exist.
- **A retry is a NEW proc**, never a reset of the old one. The fresh row carries
  `previous_attempt` pointing at the row it supersedes, which is what makes attempt
  numbering, the "superseded — see attempt N" link, and both navigation directions
  deterministic. "Superseded" is *derived* from that edge, never stored.
- **An ended session accepts no new procs.** `proc/add` is refused once `ended_at` is set.
  Without that rule a retry attempt queued before a stop can land after it — the run client
  posts asynchronously — leaving a `waiting` row that nothing will ever settle.

## Settlement: every path that makes a proc terminal

| Trigger | What it settles | `fail_reason` |
| --- | --- | --- |
| Run deregisters normally | `waiting` + `running` procs | `session_end_before_proc_finish` |
| Browser stops the job | `waiting` + `running` procs | `force_stopped` |
| Browser restarts one proc | that proc | `force_restarted` |
| Dead run PID (periodic sweep) | `waiting` + `running`, and ends the session | `session_end_before_proc_finish` |
| Daemon restart (store load) | `waiting` + `running` of incomplete sessions | `session_end_before_proc_finish` |
| Annotation interrupted | that annotate proc | `annotation_interrupted` |

The dead-PID sweep deliberately skips sessions that already ended: a stopped job has no PID
left to check, and re-settling terminal rows would be a no-op at best.

## Job lifecycle is derived, never stored

A session's `running` / `completed` / `failed` / `cancelled` status is **computed** from its
procs on every read (`Session::lifecycle_status`), and the browser mirrors that computation.
Only the inputs are persisted — `ended_at`, `last_seen_at`, `client_connected`, `run_pid`,
and each proc's own state.

- Ended **and** any proc still `waiting` or `running` → `cancelled`.
- Ended with non-superseded failures that are all stop-shaped (`force_stopped`,
  `force_restarted`, `session_end_before_proc_finish`) → `cancelled`.
- Ended with any other non-superseded failure → `failed`.
- Ended with none → `completed`.
- Not ended, past the liveness deadline → `failed`. Otherwise `running`.

The consequence worth remembering: **one unsettled `waiting` proc pins the whole job to
`cancelled`**, forever, no matter what its other routes did. That is why the settlement
table above must stay exhaustive.

## Stopping a job, end to end

A stop is three separate things, and it needs all three because each can fail independently:

1. **Settle the store** — end the session, mark `waiting` and `running` procs `force_stopped`,
   and disarm the supervisor so it never resurrects what a human killed.
2. **Kill the containers** — every proc's still-named container.
3. **Stop the runner** — write the `cancel-requests/<session>` marker, then SIGTERM (and
   SIGKILL) the run process when its PID is known.

Step 3's marker is not redundant with the signal. A route waiting out a retry backoff is
sleeping in the run process, and the PID is not always known; the marker is what that sleep
polls, so a stopped job stops retrying even when the signal never arrives. The runner peeks
the marker — it does not consume it, since every route of a fleet must see it — and clears it
once at teardown.

## Invariants to preserve

- One unsettled non-terminal proc makes its job read as incomplete forever. Every new way for
  a proc to be created needs a matching way for it to be settled.
- Renderers must agree. The job graph, the fleet rollup table, and the job badge all derive
  from the same proc states; if they can disagree, the states are wrong, not the renderers.
- Marker files are the only daemon→runner channel. Anything the daemon must ask of a running
  `scsh run` goes through `src/daemon/paths.rs`, and the runner must poll it somewhere that
  is reached even while it is sleeping.
