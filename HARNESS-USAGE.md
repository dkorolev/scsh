# Harness usage accounting

`scsh` requires token usage for every successful agent attempt without
making another provider request. Collection happens before
the attempt's forwarded credentials and temporary config are scrubbed:

- Claude Code: sum the `usage` object on each unique assistant response in the fresh
  transcript tree, including subagent transcripts. Every transcript must end on a finished
  turn, except a subagent the main turn left behind mid-turn: once its transcript has been
  untouched for two minutes, its counters are final and it no longer holds the wait.
- Codex: keep the last cumulative `total_token_usage` record in each fresh session file,
  then sum independent parent and subagent sessions. Forked sessions with inherited
  accounting are marked unavailable rather than double-counting the parent.
- Cursor: keep the last cumulative token payload for each conversation from user-level
  TUI hooks. The hooks append events; they never publish a persistent readiness marker.
  Resumed work invalidates the previous completed stop. Cursor emits a completed `stop` before the same generation's trailing `afterAgentResponse` and `sessionEnd`; those trailing records preserve the stop, while a later generation or tool call invalidates it. The hooks live in the forwarded
  config home, never in the skill repository.
- Cline: sum `aggregateUsage` (or `usage`) from each fresh session document under the
  forwarded data directory; its `inputTokens` includes the cache buckets, which are subtracted. Round trips are the assistant messages carrying `metrics` in the
  session's `messages.json`, and tool calls are its `tool_use` blocks, so Cline reports the
  same two counts as Claude Code. A session still on its first response has no counters and
  stays incomplete.
- OpenCode: a plugin in the container's config home (`plugins/scsh-usage.js`, never the skill
  repository) appends opencode's own events to `<run log>.opencode-events.jsonl`: assistant
  `message.updated` records, finished tool calls, and `session.status`. Each assistant message
  is one model call whose final update (with `time.completed`) carries its counters; `input`
  already excludes the cache buckets, and reasoning tokens count as output. The summary is
  complete once every message has completed and every session is idle again.

**Headless types count exactly like their agents.** `claude-headless`, `codex-headless`, and
`cline-headless` run the same CLI in print/exec/JSON mode, and that mode writes the same native
session files as the TUI: Claude's transcript JSONL, Codex's rollout files, Cline's session
documents. `scsh` reads those files with the same parser, never the headless event stream. The
stored `TokenUsage.harness` is the agent's (`claude_code`, `codex`, `cline`), and the route's
own harness name stays on the proc and in `scsh stats`. A TUI route and a headless route of
one agent therefore report directly comparable numbers. Any difference between them is a
real difference in what the CLI sent; for example, Claude's interactive mode adds tool
schemas that print mode does not (see [CLAUDE-PROMPTS-RESULTS.md](CLAUDE-PROMPTS-RESULTS.md)).

Once a result exists and the turn has gone quiet, the host owns a single accounting deadline: up to 600 seconds for every harness, allowing time for delayed API retries. Accounting completion releases the run immediately; the deadline is a maximum wait. Screen activity and a still-growing native stream hold that deadline — Cursor writes the result with a tool, then generates the final message with no hooks until `stop`. Override the wait with `SCSH_USAGE_ACCOUNTING_TIMEOUT` (positive integer seconds; invalid values fall back to 600 seconds). Startup, inactivity, wall-clock, and result-quiescence watchdogs yield during that bounded phase. The host validates fresh native records and the latest turn's completion, atomically saves the accounting snapshot, then authorizes the container to request a clean exit (`/exit` or `/quit`). No completion path sends Ctrl-C. Teardown gets a separate bounded grace period; a wedged process is still cleaned up. A timeout is recorded before teardown and stays a failure even if counters arrive late.

Missing or incomplete native counters fail the attempt as `usage_accounting_unavailable`; a harness that remains live without complete accounting for the full deadline fails as `usage_accounting_timeout`. Both retain the result, recording, hook stream, and run clone for inspection. For latency-sensitive work where counters are deliberately unnecessary, `SCSH_NO_USAGE=1 scsh run …` disables the requirement and accounting wait for every harness.
Cache hits launch no agent and are exempt from new accounting.

The session browser shows one small usage line below the recording only after the attempt
has finished. A cache hit launches no harness and therefore creates no new usage record.

Grok reads `_x.ai/session/update` → `turn_completed.usage` in the primary session’s `updates.jsonl`. It sums distinct prompt bills, including Grok’s folded subagent usage; it does not add child transcripts or per-model breakdowns again. `usageIsIncomplete`, missing counters, malformed records, and conflicting duplicate bills prevent successful accounting. The final turn must end with `end_turn`, and the transcript must be at least as recent as the result file. Model-call counts come from `modelCalls`; tool counts remain unavailable because the primary stream does not contain all child tool calls.

## Stable schema

The same strict object is available at `procs[].usage` from
`GET /api/v1/session/{id}`, in fleet route JSON, and in
`$SCSH_HOME/sessions/<session>/results/<invocation>.usage.json`:

```json
{
  "TokenUsage": {
    "schema_version": 1,
    "harness": "claude_code",
    "source": "claude_session_jsonl",
    "complete": true,
    "tokens": {
      "input": 120,
      "output": 30,
      "cache_read": 400,
      "cache_write": 20
    },
    "llm_round_trips": 3,
    "tool_calls": 2
  }
}
```

`harness` is exactly `claude_code`, `codex`, `cursor`, `grok`, `cline`, or `opencode`; its corresponding `source` is exactly `claude_session_jsonl`, `codex_session_jsonl`, `cursor_hooks`, `grok_session_jsonl`, `cline_session_json`, or `opencode_plugin_events`. Unknown fields,
versions, harnesses, and harness/source combinations are rejected by the reader.

`tokens.input` is uncached input. Claude already reports that bucket separately; Codex, Cursor, Grok, and Cline input counters include cached input, so `scsh` subtracts the cache buckets. (Cline's per-response metrics show it: each response's `cacheReadTokens` equals the previous response's `inputTokens`.) Missing
accounting is `tokens: null` with `complete: false`, never a fabricated zero. Counters are
observable client records rather than a billing statement. Claude and Codex include local subagent transcripts; Grok uses the primary bill that already includes them.

Every field is required. Counts are nonnegative integers no greater than
9,007,199,254,740,991. `cache_write`, `llm_round_trips`, and `tool_calls` may be `null`
when the harness does not expose them reliably. In particular, Codex token snapshots
do not identify model round-trips or tool calls. An interrupted attempt without a finalized accounting snapshot has
`complete: false` even when some counters were recovered. Unsupported harnesses and
cache hits have `usage: null`; they do not pretend to have measured usage.

Collection polls local files during completion; it makes no provider request.
Cursor additionally appends one local record per hook. The cost scales with the run's
transcript size. The results-side file contains the latest attempt; uniquely named log
files and individual session procs preserve each attempt separately.

The original Cursor hook stream is retained under `logs/<stem>.cursor-hooks.jsonl` for
verification. Every harness's normalized summary is retained as `logs/<stem>.usage.json`.

## Verification

Offline parser and schema fixtures run with `cargo test --bin scsh usage::`; UI placement
is covered by `usage_appears_only_after_a_finished_player`. `HARNESS-SMOKE.md` describes the
live, human-followable check across all configured routes.

Claude invocation policies, per-response ledgers, direct-control accounting, and the remote comparison procedure are documented in [CLAUDE-PROMPTS.md](CLAUDE-PROMPTS.md).
