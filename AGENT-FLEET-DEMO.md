# AGENT-FLEET-DEMO.md — one agent drives a fleet of three through `scsh`

This walkthrough demos **agent-driven orchestration**: you — an AI agent running in your
own harness (Cursor, Claude Code, Codex CLI, …) — use `scsh` as your fan-out primitive.
One `scsh run` launches the same *"explain this codebase"* skill as **three separate agent
jobs** — **claude**, **codex**, and **cursor** — each in its own container on a clean clone
of the same commit. The call **blocks until every job has written its result**, so you just
wait; then you read the three JSON results and **synthesize the comparison yourself**.

The contract you are exercising is `scsh help agent`: exit codes and JSON only — you never
poll, never scrape human-formatted output. And nothing goes anywhere: the target repo stays
byte-clean, results land only under its **gitignored `tmp/`**, no commits, no pushes.

Like [`DEMO.md`](DEMO.md), this file is written to be **handed to an AI agent**: start your
agent in any directory that is **not** inside a git repository and ask it to *follow the
steps in AGENT-FLEET-DEMO.md*. And because this walkthrough is embedded in the binary, no
path to any checkout is needed — with `scsh` on the PATH, the whole prompt is:

> Run `scsh demo agent-fleet` and follow the steps it prints, exactly as written. Report
> every **Expect:** line as PASS or FAIL, and finish with the step-8 comparison.

> **If you are the agent following this:** run each command, in order, exactly as written.
> Every step is a single copy-pasteable command; you should never need to write a script of
> your own — the one exception is step 8, which is **your own analysis work**, not a shell
> command. Report each **Expect:** line as PASS or FAIL, and give a final PASS/FAIL verdict.

## What this proves

1. An agent can drive `scsh` end to end using nothing but exit codes and JSON:
   `probe --json` to gate, one blocking `run` to execute, declared result files to collect.
2. One command fans out to **three separate agent jobs in parallel** on the same committed
   state; the driving harness waits on the single call — no polling loop, no job table.
3. The target repository never contains any scsh setup (`--override-dot-scsh-yml` brings
   the config and the skill from an external bundle) and stays clean before and after.
4. Three different agents, given the identical task, produce **usefully different**
   explanations — and the driving agent (you) is the one who puts them together.

A note on words: in `.scsh.yml` the key that names which agent CLI runs a job is spelled
`harness:` — in this demo "the harness" always means the tool *you* are running in, and the
three workers are just **agents** / **agent jobs**.

## 1. Probe the environment

```sh
[ -f ~/.zshrc ] && . ~/.zshrc 2>/dev/null || true
command -v git  >/dev/null && echo "git: ok"  || echo "git: MISSING (required)"
command -v scsh >/dev/null && echo "scsh: on PATH" || echo "scsh: MISSING (build with cargo build --release)"
{ command -v docker || command -v podman || command -v container; } >/dev/null 2>&1 \
  && echo "container runtime: ok" || echo "container runtime: MISSING (required)"
```

**Expect:** all three `ok`. The agent routes themselves are probed in step 5.

## 2. Work area

```sh
DEMO_ROOT="$(pwd)/scsh-agent-fleet-demo-$(date -u +%Y%m%d-%H%M%S)" && mkdir -p "$DEMO_ROOT" && echo "$DEMO_ROOT"
```

(If you are inside the `scsh` repo itself, prefix the path with `tmp/` so nothing pollutes
the checkout: `DEMO_ROOT="$(pwd)/tmp/scsh-agent-fleet-demo-…"`.)

## 3. The target codebase — small, real, and deliberately quirky

A tiny in-memory job scheduler, four Python files, with a few **planted quirks** the agents
may or may not notice (the scorecard in step 8 names them). It ships **no `.scsh.yml` and no
`.skills/`** — the target knows nothing about scsh.

```sh
mkdir -p "$DEMO_ROOT/repo" && cd "$DEMO_ROOT/repo" && git init -q && printf '/tmp\n' > .gitignore && printf '# minisched\n\nA tiny in-memory job scheduler. The code is the documentation.\n' > README.md && cat > scheduler.py <<'EOF' && cat > jobs.py <<'EOF2' && cat > worker.py <<'EOF3' && cat > main.py <<'EOF4' && git add -A && git commit -qm "minisched: tiny in-memory job scheduler." && ls && git status --porcelain && echo "repo: clean"
"""In-memory priority scheduler. Lower number = higher priority."""

import heapq
import itertools


class Scheduler:
    def __init__(self):
        self._heap = []
        self._seq = itertools.count()
        self._cancelled = set()

    def submit(self, job, priority=5):
        heapq.heappush(self._heap, (priority, next(self._seq), job))

    def cancel(self, job_id):
        self._cancelled.add(job_id)

    def next_job(self):
        while self._heap:
            _, _, job = heapq.heappop(self._heap)
            if job.job_id in self._cancelled:
                continue
            return job
        return None
EOF
"""Job + its retry policy."""

from dataclasses import dataclass
from typing import Callable


@dataclass
class Job:
    job_id: str
    action: Callable
    max_attempts: int = 3
    attempts: int = 0

    def backoff_seconds(self):
        return min(2 ** self.attempts, 60)

    def exhausted(self):
        return self.max_attempts != 0 and self.attempts >= self.max_attempts
EOF2
"""Pulls jobs from a Scheduler and runs them; failures retry, then dead-letter."""

DEAD_LETTER_CAP = 100


class Worker:
    def __init__(self, scheduler):
        self.scheduler = scheduler
        self.dead_letters = []
        self.completed = []

    def tick(self):
        job = self.scheduler.next_job()
        if job is None:
            return False
        try:
            job.action()
            self.completed.append(job.job_id)
        except Exception:
            job.attempts += 1
            if job.exhausted():
                self.dead_letters.append(job.job_id)
                if len(self.dead_letters) > DEAD_LETTER_CAP:
                    self.dead_letters.pop(0)
            else:
                self.scheduler.submit(job, priority=9)
        return True
EOF3
"""Demo entry point: submit a few jobs, run the worker to completion."""

from jobs import Job
from scheduler import Scheduler
from worker import Worker


def main():
    s = Scheduler()
    w = Worker(s)
    s.submit(Job("greet", lambda: print("hello")), priority=1)
    s.submit(Job("boom", lambda: 1 / 0, max_attempts=2), priority=1)
    s.submit(Job("later", lambda: print("later")), priority=8)
    while w.tick():
        pass
    print("completed:", w.completed)
    print("dead-lettered:", w.dead_letters)


if __name__ == "__main__":
    main()
EOF4
```

**Expect:** the listing shows the four `.py` files + `README.md`, NO `.scsh.yml` and NO
`.skills/`; `repo: clean` prints.

## 4. The external bundle — the skill and its three-agent matrix

The bundle lives **outside** the repo: one skill, `explain-codebase`, fanned out to three
agents. Each job is **read-only** (no `commits:`) and writes structured JSON to the file
named by `$SCSH_RESULT` — which scsh always places under the target repo's gitignored
`tmp/` and copies back per route as `tmp/explain_<route>_result.json`.

```sh
mkdir -p "$DEMO_ROOT/bundle/.skills/explain-codebase" && cat > "$DEMO_ROOT/bundle/.scsh.yml" <<'YML' && cat > "$DEMO_ROOT/bundle/.skills/explain-codebase/SKILL.md" <<'MD' && echo "bundle: ready"
skills:
  explain-codebase:
    timeout: 900
    result: tmp/explain_{name}_result.json
    invocations:
      claude-sonnet:
        harness: claude
        model: sonnet
      codex-luna:
        harness: codex
        model: gpt-5.6-luna
      cursor-composer-fast:
        harness: cursor
        model: composer-2.5-fast
YML
---
name: explain-codebase
description: "Read every tracked file in the current repository and write a structured JSON explanation of the codebase — purpose, architecture, entry points, and anything surprising or risky — to the file named by $SCSH_RESULT. Read-only: modifies nothing, commits nothing."
---

# explain-codebase

You are one of several agents given the **identical task on the identical commit**; your
answers will be compared side by side. Work alone, from the code only.

## Steps

1. Read `README.md` and **every tracked source file** in the repository.
2. Do **not** modify, create, or delete any tracked file. Do not commit. Your only output
   is the result file.
3. Write a single JSON object to the file named by the `SCSH_RESULT` environment variable
   (the path is under the repository's gitignored `tmp/`; create the directory if needed):

   {
     "agent": "<the CLI you are: claude | codex | cursor | ...>",
     "one_liner": "<the whole codebase in one sentence>",
     "purpose": "<2-4 sentences: what this code is for>",
     "architecture": ["<3-6 bullets: the pieces and how they fit>"],
     "entry_points": ["<how you would run or use it>"],
     "surprising_or_risky": ["<2-5 bullets: quirks, foot-guns, silent behaviors you found in the code>"],
     "confidence": "high|medium|low"
   }

Dig for the `surprising_or_risky` bullets — this codebase has deliberate quirks. Point at
real lines of code, not generic advice.
MD
```

**Expect:** `bundle: ready` prints.

## 5. Probe the three agent routes

Gate before running — this is the `scsh help agent` loop, step 2:

```sh
cd "$DEMO_ROOT/repo" && scsh probe --override-dot-scsh-yml "$DEMO_ROOT/bundle/.scsh.yml" --json
```

**Expect:** JSON with exactly three routes — `claude`·`sonnet`, `codex`·`gpt-5.6-luna`,
`cursor`·`composer-2.5-fast` — each with an `available` flag, plus `available`/`total`
counts; exit code 0 when at least one is available. The run needs **at least one**; the
comparison in step 8 is only meaningful with **two or more**. Report the count. If zero
are available, **stop here with a FAIL** — log in to at least one of the three CLIs and
retry.

## 6. Run the fleet — one command, three agent jobs, one wait

```sh
cd "$DEMO_ROOT/repo" && scsh run --override-dot-scsh-yml "$DEMO_ROOT/bundle/.scsh.yml"
```

This is the moment the demo is about: the command launches every available route as its own
containerized agent job on a clean clone of the commit, and **blocks until all of them
finish**. Sit on the call — that IS the wait. (First run may build container images; a few
extra minutes. Spectators: `scsh daemon start` and http://127.0.0.1:7274 shows the jobs
live.)

**Expect:** one line per route; every route that probed available **succeeds**, routes that
probed unavailable are **skipped**; overall exit code 0.

```sh
ls tmp/ && git status --porcelain && echo "repo: clean"
```

**Expect:** one `explain_<route>_result.json` per successful route under `tmp/`
(`explain_claude-sonnet_result.json`, `explain_codex-luna_result.json`,
`explain_cursor-composer-fast_result.json`); `repo: clean` prints — the results are in
gitignored scratch, nothing tracked changed, nothing committed, nothing left the machine.

## 7. Collect

```sh
for f in tmp/explain_*_result.json; do echo "== $f"; cat "$f"; echo; done
```

**Expect:** each file is one valid JSON object matching the schema from step 4.

## 8. Synthesize — this part is you

No shell command here: **you are the third stage of the pipeline.** Read the result files
and produce a comparison for the user:

1. **Scorecard.** The target has three planted quirks. For each agent, report which it
   surfaced in `surprising_or_risky`:
   - `max_attempts=0` means **retry forever** (`jobs.py`, `exhausted()`) — documented nowhere;
   - a retried job **loses its original priority** — resubmitted at 9, the back of the queue
     (`worker.py`, the `except` branch);
   - the dead-letter list is **capped at 100 and silently drops the oldest** failure
     (`worker.py`, `DEAD_LETTER_CAP`).
2. **Consensus vs. disagreement.** Where do the one-liners and architecture bullets agree?
   Where do the agents genuinely differ — and is any of them *wrong*?
3. **Unique insights.** Anything only one agent noticed (the FIFO tie-break counter in
   `scheduler.py` and the silent drop of cancelled jobs are honest bonus finds).
4. **Verdict.** Which explanation would you hand to a new teammate, and why — one short
   paragraph.

**Expect:** a scorecard table plus your synthesis. PASS if every collected result parsed and
the scorecard could be filled in; the *scores themselves* are the interesting output, not a
pass condition.

## 9. Re-collect for free — the cache

```sh
scsh run --override-dot-scsh-yml "$DEMO_ROOT/bundle/.scsh.yml"
```

**Expect:** every previously-successful route reports `(cached)` and the command returns
near-instantly — no containers, no model calls. Same commit + same env = the fleet's answers
are already yours; re-running the orchestration is free.

## 10. Cleanup (optional)

The demo leaves `$DEMO_ROOT` for inspection. When done:

```sh
rm -rf "$DEMO_ROOT"
```

---

That's the whole pattern, and it generalizes past this demo: any harness that can run a
shell command can gate on `scsh probe`, fan work out to a fleet of agents with one blocking
`scsh run`, and collect declared JSON result files — `scsh help agent` is the compact
contract, and the bundle here is a template for skills of your own.
