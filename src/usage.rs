//! Per-attempt spend from a harness that can report it.
//!
//! Cursor's interactive TUI does not emit `--print` stream-json. scsh therefore
//! installs a user-level hook (in the forwarded config home, never the skill repo)
//! that appends `stop` / `afterAgentResponse` / `postToolUse` payloads. Those are
//! the events exposed by the pinned Cursor CLI in interactive mode.
//!
//! Hook `input_tokens` includes cache. This summary stores the four buckets the
//! same way print-mode did: `input` is uncached only.

pub(crate) mod claude;

use crate::json::{self, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Writer installed into the container's Cursor config home (Python ships in the base image).
/// Reads one hook event on stdin, appends it as a single line, always exits 0.
pub const CURSOR_HOOK_PY: &str = r#"#!/usr/bin/env python3
import fcntl
import json
import os
import sys

try:
    event = json.load(sys.stdin)
    path = os.environ.get("SCSH_CURSOR_HOOKS_LOG") or os.path.join(os.path.dirname(__file__), "scsh-cursor-hooks.jsonl")
    os.umask(0o077)
    with open(path, "a") as log:
        fcntl.flock(log, fcntl.LOCK_EX)
        log.write(json.dumps(event, separators=(",", ":")) + "\n")
        log.flush()
        os.fsync(log.fileno())
except Exception:
    pass  # Accounting must never interrupt the agent loop.
print("{}")
"#;

/// opencode plugin installed into the container's config home (`plugins/`, never the skill
/// repo). It appends opencode's own accounting events to `$SCSH_RUN_LOG.opencode-events.jsonl`:
/// assistant messages (whose final update carries the token counters), finished tool calls,
/// and session busy/idle status. Everything else is dropped to keep the stream small.
pub const OPENCODE_PLUGIN_JS: &str = r#"import { appendFileSync } from "node:fs";

export const ScshUsage = async () => ({
  event: async ({ event }) => {
    try {
      const p = event.properties || {};
      let line = null;
      if (event.type === "message.updated" && p.info && p.info.role === "assistant") line = event;
      else if (event.type === "session.status") line = event;
      else if (event.type === "message.part.updated" && p.part && p.part.type === "tool" && p.part.state &&
        (p.part.state.status === "completed" || p.part.state.status === "error")) {
        // The tool's own input and output are not accounting; keep the call's identity only.
        const { id, sessionID, messageID, tool } = p.part;
        line = { type: event.type, properties: { part: { id, sessionID, messageID, tool, type: "tool" } } };
      }
      if (line) appendFileSync(process.env.SCSH_RUN_LOG + ".opencode-events.jsonl", JSON.stringify(line) + "\n");
    } catch {} // Accounting must never interrupt the agent loop.
  },
});
"#;

/// `hooks.json` that points Cursor's interactive TUI at [`CURSOR_HOOK_PY`].
pub fn hooks_json(script_in_container: &str) -> String {
  let cmd = json::quote(script_in_container);
  format!(
    r#"{{
  "version": 1,
  "hooks": {{
    "afterAgentResponse": [{{ "command": {cmd} }}],
    "stop": [{{ "command": {cmd} }}],
    "postToolUse": [{{ "command": {cmd} }}],
    "sessionEnd": [{{ "command": {cmd} }}]
  }}
}}
"#
  )
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
  /// Uncached prompt/input tokens consumed by the provider.
  pub input: u64,
  /// Model output tokens reported by the provider.
  pub output: u64,
  /// Prompt/input tokens served from a provider cache.
  pub cache_read: u64,
  /// Prompt/input tokens written into a provider cache, if reported by this harness version.
  pub cache_write: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Harness {
  ClaudeCode,
  Codex,
  Cursor,
  Grok,
  Cline,
  Opencode,
}

impl Harness {
  fn json_name(self) -> &'static str {
    match self {
      Harness::ClaudeCode => "claude_code",
      Harness::Codex => "codex",
      Harness::Cursor => "cursor",
      Harness::Grok => "grok",
      Harness::Cline => "cline",
      Harness::Opencode => "opencode",
    }
  }

  fn source(self) -> &'static str {
    match self {
      Harness::ClaudeCode => "claude_session_jsonl",
      Harness::Codex => "codex_session_jsonl",
      Harness::Cursor => "cursor_hooks",
      Harness::Grok => "grok_session_jsonl",
      Harness::Cline => "cline_session_json",
      Harness::Opencode => "opencode_plugin_events",
    }
  }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
  /// Harness whose native accounting produced this summary.
  pub harness: Harness,
  /// Whether every native record needed for an exact total parsed successfully.
  pub complete: bool,
  /// Normalized token buckets, or `None` when the harness emitted no usable counters.
  pub tokens: Option<Tokens>,
  /// Observable model responses when the native format identifies them reliably.
  pub llm_round_trips: Option<u64>,
  /// Observable tool calls when the native format identifies them reliably.
  pub tool_calls: Option<u64>,
}

impl Summary {
  /// Partial accounting can still prove that a turn started. No final result or complete
  /// token total is needed to stop treating an active agent as a failed startup.
  pub fn has_started_work(&self) -> bool {
    self.tool_calls.is_some_and(|n| n > 0)
      || self.llm_round_trips.is_some_and(|n| n > 0)
      || self
        .tokens
        .as_ref()
        .is_some_and(|t| t.input > 0 || t.output > 0 || t.cache_read > 0 || t.cache_write.is_some_and(|n| n > 0))
  }

  pub fn to_json(&self) -> String {
    json::write_pretty(&summary_value(self))
  }

  pub fn compact_json(&self) -> String {
    json::write(&summary_value(self))
  }

  pub fn from_json(text: &str) -> Option<Summary> {
    let value = json::parse(text).ok()?;
    if object_len(&value)? != 1 || object_keys(&value)? != ["TokenUsage"].into_iter().collect() {
      return None;
    }
    let payload = field(&value, "TokenUsage")?;
    if object_len(payload)? != 7
      || object_keys(payload)?
        != ["complete", "harness", "llm_round_trips", "schema_version", "source", "tokens", "tool_calls"]
          .into_iter()
          .collect()
    {
      return None;
    }
    if count(payload, "schema_version") != Some(1) {
      return None;
    }
    let harness = match string(payload, "harness")? {
      "claude_code" => Harness::ClaudeCode,
      "codex" => Harness::Codex,
      "cursor" => Harness::Cursor,
      "grok" => Harness::Grok,
      "cline" => Harness::Cline,
      "opencode" => Harness::Opencode,
      _ => return None,
    };
    if string(payload, "source") != Some(harness.source()) {
      return None;
    }
    let complete = match field(payload, "complete")? {
      Value::Bool(value) => *value,
      _ => return None,
    };
    let tokens = match field(payload, "tokens")? {
      Value::Null => None,
      usage => {
        if object_len(usage)? != 4
          || object_keys(usage)? != ["cache_read", "cache_write", "input", "output"].into_iter().collect()
        {
          return None;
        }
        Some(Tokens {
          input: count(usage, "input")?,
          output: count(usage, "output")?,
          cache_read: count(usage, "cache_read")?,
          cache_write: optional_count(usage, "cache_write")?,
        })
      }
    };
    if complete && tokens.is_none() {
      return None;
    }
    Some(Summary {
      harness,
      complete,
      tokens,
      llm_round_trips: optional_count(payload, "llm_round_trips")?,
      tool_calls: optional_count(payload, "tool_calls")?,
    })
  }

  /// One-line board / job-page fragment: cache split, trips, tools.
  pub fn phrase(&self) -> String {
    let mut parts = Vec::new();
    if let Some(t) = &self.tokens {
      let total_input = t.input.saturating_add(t.cache_read).saturating_add(t.cache_write.unwrap_or(0));
      if total_input > 0 {
        parts.push(format!("{} input tokens", compact_count(total_input)));
      }
      if t.output > 0 {
        parts.push(format!("{} output tokens", compact_count(t.output)));
      }
      if t.cache_read > 0 {
        parts.push(format!("{} cached tokens", compact_count(t.cache_read)));
      }
    }
    if self.tokens.is_none() {
      parts.push("tokens unavailable".into());
    }
    if let Some(trips) = self.llm_round_trips.filter(|n| *n > 0) {
      parts.push(format!("{trips} LLM call{}", if trips == 1 { "" } else { "s" }));
    }
    if let Some(tools) = self.tool_calls.filter(|n| *n > 0) {
      parts.push(format!("{tools} tool call{}", if tools == 1 { "" } else { "s" }));
    }
    if !self.complete {
      parts.push("partial".into());
    }
    if parts.is_empty() {
      parts.push("No measured cost".into());
    }
    parts.join(" · ")
  }

  /// Exact native buckets for the compact UI summary's hover text.
  pub fn details(&self) -> String {
    let mut parts = Vec::new();
    if let Some(t) = &self.tokens {
      if t.input > 0 {
        parts.push(format!("Uncached input: {}", grouped_count(t.input)));
      }
      if t.cache_read > 0 {
        parts.push(format!("Cache read: {}", grouped_count(t.cache_read)));
      }
      if let Some(write) = t.cache_write.filter(|n| *n > 0) {
        parts.push(format!("Cache write: {}", grouped_count(write)));
      }
      if t.output > 0 {
        parts.push(format!("Output: {}", grouped_count(t.output)));
      }
    }
    if let Some(calls) = self.llm_round_trips.filter(|n| *n > 0) {
      parts.push(format!("Model calls: {}", grouped_count(calls)));
    }
    if let Some(tools) = self.tool_calls.filter(|n| *n > 0) {
      parts.push(format!("Tool calls: {}", grouped_count(tools)));
    }
    if !self.complete {
      parts.push("Partial accounting".into());
    }
    parts.join(" · ")
  }
}

fn compact_count(n: u64) -> String {
  if n < 1_000 {
    return n.to_string();
  }
  let (value, suffix) = if n < 1_000_000 {
    (n, "k")
  } else if n < 1_000_000_000 {
    (n / 1_000, "m")
  } else {
    (n / 1_000_000, "b")
  };
  let whole = value / 1_000;
  let tenth = (value % 1_000) / 100;
  if whole < 10 && tenth > 0 {
    format!("{whole}.{tenth}{suffix}")
  } else {
    format!("{whole}{suffix}")
  }
}

fn grouped_count(n: u64) -> String {
  let digits = n.to_string();
  let mut out = String::with_capacity(digits.len() + digits.len() / 3);
  for (index, ch) in digits.chars().enumerate() {
    if index > 0 && (digits.len() - index).is_multiple_of(3) {
      out.push(',');
    }
    out.push(ch);
  }
  out
}

fn summary_value(summary: &Summary) -> Value {
  object(vec![(
    "TokenUsage",
    object(vec![
      ("schema_version", number(1)),
      ("harness", Value::String(summary.harness.json_name().into())),
      ("source", Value::String(summary.harness.source().into())),
      ("complete", Value::Bool(summary.complete)),
      ("tokens", tokens_json(summary.tokens.as_ref())),
      ("llm_round_trips", optional_number(summary.llm_round_trips)),
      ("tool_calls", optional_number(summary.tool_calls)),
    ]),
  )])
}

fn tokens_json(tokens: Option<&Tokens>) -> Value {
  match tokens {
    None => Value::Null,
    Some(t) => object(vec![
      ("input", Value::Number(t.input as f64)),
      ("output", Value::Number(t.output as f64)),
      ("cache_read", Value::Number(t.cache_read as f64)),
      ("cache_write", optional_number(t.cache_write)),
    ]),
  }
}

#[derive(Default)]
struct Conversation {
  /// Every event so far is a tool hook using the conversation id as generation id.
  child_tools_only: bool,
  /// At least one tool hook explicitly lacked a transcript, as child startup does.
  /// Later child hooks can name a transcript without supplying separate accounting.
  has_child_marker: bool,
  generations: BTreeSet<String>,
  /// Most recent event generation, used to recognize Cursor's trailing response hook.
  active_generation: Option<String>,
  /// Generation named by the last completed stop, when Cursor supplies one.
  stopped_generation: Option<String>,
  responses: u64,
  tools: BTreeSet<String>,
  missing_tool_id: bool,
  tokens: Option<Tokens>,
  stopped: bool,
}

/// Summarize a Cursor TUI hook stream. Missing token fields stay null; partial
/// counts are lower bounds. `stop` and `afterAgentResponse` share totals for one
/// `generation_id` — keep the last complete payload per conversation, never sum them.
pub fn cursor_hook_summary(stream: &str) -> Summary {
  let mut conversations: BTreeMap<String, Conversation> = BTreeMap::new();
  let mut invalid = 0;
  for line in stream.lines().filter(|line| !line.trim().is_empty()) {
    let Ok(event) = json::parse(line) else {
      invalid += 1;
      continue;
    };
    let Some(event_name) = hook_name(&event) else {
      invalid += 1;
      continue;
    };
    let id =
      string(&event, "conversation_id").or_else(|| string(&event, "session_id")).unwrap_or("session").to_string();
    let conversation = conversations
      .entry(id.clone())
      .or_insert_with(|| Conversation { child_tools_only: true, ..Conversation::default() });
    // Cursor child streams initially report a null transcript and use their
    // conversation id as generation id, even after a transcript becomes available.
    // Missing metadata alone is not evidence of a child.
    conversation.child_tools_only &=
      event_name == "postToolUse" && string(&event, "generation_id") == Some(id.as_str());
    conversation.has_child_marker |=
      event_name == "postToolUse" && matches!(field(&event, "transcript_path"), Some(Value::Null));
    if matches!(event_name, "afterAgentResponse" | "stop" | "postToolUse") {
      if let Some(generation) = string(&event, "generation_id") {
        conversation.generations.insert(generation.to_string());
        if event_name != "stop" {
          conversation.active_generation = Some(generation.to_string());
        }
      }
    }
    match event_name {
      "afterAgentResponse" => {
        conversation.responses += 1;
        let generation = string(&event, "generation_id");
        let trailing_completed_response = conversation.stopped
          && match conversation.stopped_generation.as_deref() {
            Some(stopped) => generation == Some(stopped),
            None => generation.is_none(),
          };
        if !trailing_completed_response {
          conversation.stopped = false;
          conversation.stopped_generation = None;
        }
        // Cursor emits `stop` before the same generation's response hook. Preserve
        // real stop counters when that trailing hook omits its token fields.
        if let Some(tokens) = hook_tokens(&event) {
          conversation.tokens = Some(tokens);
        }
      }
      "stop" => {
        if conversation
          .active_generation
          .as_deref()
          .is_some_and(|active| string(&event, "generation_id") != Some(active))
        {
          continue;
        }
        conversation.stopped = string(&event, "status") == Some("completed");
        conversation.stopped_generation = conversation
          .stopped
          .then(|| {
            string(&event, "generation_id").map(str::to_string).or_else(|| conversation.active_generation.clone())
          })
          .flatten();
        conversation.tokens = hook_tokens(&event);
      }
      "sessionEnd" => {
        // Session teardown cannot turn an aborted generation into a completed turn.
      }
      "postToolUse" => {
        conversation.stopped = false;
        conversation.stopped_generation = None;
        if let Some(tool) = string(&event, "tool_use_id").or_else(|| string(&event, "call_id")) {
          conversation.tools.insert(tool.to_string());
        } else {
          conversation.missing_tool_id = true;
        }
      }
      _ => {}
    }
  }
  let llm_round_trips = conversations
    .values()
    .map(|c| if !c.generations.is_empty() { c.generations.len() as u64 } else { c.responses })
    .sum();
  let tool_calls = conversations.values().map(|c| c.tools.len() as u64).sum();
  let accounting: Vec<_> = conversations.values().filter(|c| !(c.child_tools_only && c.has_child_marker)).collect();
  let tokens =
    accounting.iter().try_fold(Tokens { input: 0, output: 0, cache_read: 0, cache_write: Some(0) }, |mut total, c| {
      let t = c.tokens.as_ref()?;
      total.input = total.input.checked_add(t.input).filter(|n| *n <= 9_007_199_254_740_991)?;
      total.output = total.output.checked_add(t.output).filter(|n| *n <= 9_007_199_254_740_991)?;
      total.cache_read = total.cache_read.checked_add(t.cache_read).filter(|n| *n <= 9_007_199_254_740_991)?;
      total.cache_write = Some(total.cache_write?.checked_add(t.cache_write?).filter(|n| *n <= 9_007_199_254_740_991)?);
      Some(total)
    });
  let tokens = tokens.filter(|_| !accounting.is_empty() && invalid == 0);
  let complete = tokens.is_some()
    && invalid == 0
    && !accounting.is_empty()
    && accounting.iter().all(|c| c.stopped && c.tokens.is_some())
    && conversations.values().all(|c| !c.missing_tool_id);
  Summary {
    harness: Harness::Cursor,
    complete,
    tokens,
    llm_round_trips: Some(llm_round_trips),
    tool_calls: Some(tool_calls),
  }
}

pub(crate) fn unavailable(harness: Harness) -> Summary {
  Summary { harness, complete: false, tokens: None, llm_round_trips: None, tool_calls: None }
}

/// Native turn boundaries, separate from token snapshots: a tool-call response or
/// cumulative counter alone does not mean the agent has finished its last turn.
pub(crate) fn turn_finished(harness: crate::config::Harness, stream: &str) -> bool {
  let harness = harness.agent();
  let mut finished = false;
  for line in stream.lines().filter(|line| !line.trim().is_empty()) {
    let Ok(event) = json::parse(line) else { return false };
    match harness {
      crate::config::Harness::Claude => match string(&event, "type") {
        Some("user") => finished = false,
        Some("system") if string(&event, "subtype") == Some("turn_duration") => finished = true,
        Some("assistant") => {
          finished = field(&event, "message").and_then(|m| string(m, "stop_reason")) == Some("end_turn");
        }
        _ => {}
      },
      crate::config::Harness::Codex => {
        if let Some(payload) = field(&event, "payload") {
          match string(payload, "type") {
            Some("task_started") => finished = false,
            Some("task_complete") => finished = true,
            _ => {}
          }
        }
      }
      _ => return false,
    }
  }
  finished
}

/// Claude Code records one `usage` object per assistant response. The forwarded config
/// directory starts empty for every attempt, so every transcript below it belongs to this run,
/// including subagents. A model response can appear under several transcript UUIDs, so
/// account by the provider message ID and request ID, retaining the largest streamed counters.
pub fn claude_session_summary(transcripts: &[Option<String>]) -> Option<Summary> {
  claude::Ledger::parse(transcripts).summary()
}

/// Codex emits cumulative `total_token_usage` snapshots. Keep the last valid snapshot in
/// each fresh per-run session file. Forks with inherited accounting are unavailable.
pub fn codex_session_summary(transcripts: &[Option<String>]) -> Option<Summary> {
  let mut total = Tokens { input: 0, output: 0, cache_read: 0, cache_write: Some(0) };
  let mut sessions = 0;
  let mut invalid = false;
  for text in transcripts {
    let Some(text) = text else {
      invalid = true;
      continue;
    };
    let mut last = None;
    let mut session_meta_seen = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
      let Ok(event) = json::parse(line) else {
        invalid = true;
        continue;
      };
      if string(&event, "type") == Some("session_meta") {
        // A fork may copy the parent's cumulative counters. Until the format supplies a
        // reliable accounting baseline, refuse to add inherited spend a second time.
        let fork = field(&event, "payload").and_then(|payload| string(payload, "forked_from_id"));
        invalid |= session_meta_seen || fork.is_some();
        session_meta_seen = true;
      }
      let info = field(&event, "payload")
        .filter(|payload| string(payload, "type") == Some("token_count"))
        .and_then(|payload| field(payload, "info"));
      if let Some(usage) = info.and_then(|info| field(info, "total_token_usage")) {
        if let Some(tokens) = codex_tokens(usage) {
          last = Some(tokens);
        } else {
          invalid = true;
        }
      }
    }
    if let Some(tokens) = last {
      sessions += 1;
      if !add_tokens(&mut total, &tokens) {
        invalid = true;
      }
    } else if session_meta_seen {
      invalid = true;
    }
  }
  if sessions == 0 {
    return None;
  }
  Some(Summary {
    harness: Harness::Codex,
    complete: !invalid,
    tokens: (!invalid).then_some(total),
    // Codex's token_count snapshots are not one-per-turn, so do not relabel them as turns.
    llm_round_trips: None,
    tool_calls: None,
  })
}

fn claude_tokens(usage: &Value) -> Option<Tokens> {
  Some(Tokens {
    input: count(usage, "input_tokens")?,
    output: count(usage, "output_tokens")?,
    cache_read: count(usage, "cache_read_input_tokens")?,
    cache_write: Some(count(usage, "cache_creation_input_tokens")?),
  })
}

fn codex_tokens(usage: &Value) -> Option<Tokens> {
  let input = count(usage, "input_tokens")?;
  let cache_read = count(usage, "cached_input_tokens")?;
  let cache_write = match field(usage, "cache_write_input_tokens") {
    None => None, // Older Codex versions do not report cache creation.
    Some(_) => Some(count(usage, "cache_write_input_tokens")?),
  };
  Some(Tokens {
    input: input.checked_sub(cache_read.checked_add(cache_write.unwrap_or(0))?)?,
    output: count(usage, "output_tokens")?,
    cache_read,
    cache_write,
  })
}

/// Identify the primary Grok session from its native event stream. Child usage is
/// folded into its parent's turn totals, so summing every session would double-count.
pub(crate) fn grok_primary_session(events: &str) -> Result<Option<String>, ()> {
  let mut primary = None;
  for line in events.lines().filter(|line| !line.trim().is_empty()) {
    let event = json::parse(line).map_err(|_| ())?;
    if string(&event, "type") == Some("turn_started") {
      let relationship = string(&event, "session_relationship").ok_or(())?;
      if relationship == "primary" {
        let id = string(&event, "session_id").ok_or(())?;
        if primary.as_deref().is_some_and(|previous| previous != id) {
          return Err(());
        }
        primary = Some(id.to_string());
      }
    }
  }
  Ok(primary)
}

/// Summarize the opencode plugin stream ([`OPENCODE_PLUGIN_JS`]). Each assistant message is
/// one model call; its last `message.updated` carries final counters once `time.completed` is
/// set. opencode's `input` already excludes cache reads; reasoning is billed as output. The
/// summary is complete only when every message has completed and every session is idle again.
pub fn opencode_events_summary(stream: &str) -> Summary {
  let mut messages: BTreeMap<String, Value> = BTreeMap::new();
  let mut sessions: BTreeMap<String, bool> = BTreeMap::new();
  let mut tools = BTreeSet::new();
  let mut parsed = true;
  for line in stream.lines().filter(|line| !line.trim().is_empty()) {
    let Ok(event) = json::parse(line) else {
      parsed = false;
      continue;
    };
    let Some(properties) = field(&event, "properties") else { continue };
    match string(&event, "type") {
      Some("message.updated") => {
        if let Some(info) = field(properties, "info") {
          if let Some(id) = string(info, "id") {
            messages.insert(id.to_string(), info.clone());
          }
        }
      }
      Some("message.part.updated") => {
        if let Some(id) = field(properties, "part").and_then(|part| string(part, "id")) {
          tools.insert(id.to_string());
        }
      }
      Some("session.status") => {
        if let (Some(session), Some(status)) =
          (string(properties, "sessionID"), field(properties, "status").and_then(|s| string(s, "type")))
        {
          sessions.insert(session.to_string(), status == "idle");
        }
      }
      _ => {}
    }
  }
  if messages.is_empty() {
    return unavailable(Harness::Opencode);
  }
  let mut tokens = Tokens { input: 0, output: 0, cache_read: 0, cache_write: Some(0) };
  let mut complete = parsed && sessions.values().all(|idle| *idle);
  for info in messages.values() {
    let done = field(info, "time").and_then(|time| field(time, "completed")).is_some_and(|v| !matches!(v, Value::Null));
    let counted = field(info, "tokens").and_then(|t| {
      let cache = field(t, "cache")?;
      Some((
        count(t, "input")?,
        count(t, "output")? + count(t, "reasoning")?,
        count(cache, "read")?,
        count(cache, "write")?,
      ))
    });
    match counted {
      Some((input, output, read, write)) => {
        tokens.input += input;
        tokens.output += output;
        tokens.cache_read += read;
        tokens.cache_write = tokens.cache_write.map(|w| w + write);
      }
      None => complete = false,
    }
    complete &= done;
  }
  Summary {
    harness: Harness::Opencode,
    complete,
    tokens: Some(tokens),
    llm_round_trips: Some(messages.len() as u64),
    tool_calls: Some(tools.len() as u64),
  }
}

/// Each completed opencode request as the provider bills it: `(prompt, completion)` tokens,
/// where the prompt includes cache reads and writes and the completion includes reasoning.
pub fn opencode_request_tokens(stream: &str) -> Vec<(u64, u64)> {
  let mut messages: BTreeMap<String, (u64, u64)> = BTreeMap::new();
  for event in stream.lines().filter_map(|line| json::parse(line).ok()) {
    if string(&event, "type") != Some("message.updated") {
      continue;
    }
    let Some(info) = field(&event, "properties").and_then(|p| field(p, "info")) else { continue };
    if field(info, "time").and_then(|t| field(t, "completed")).is_none_or(|v| matches!(v, Value::Null)) {
      continue;
    }
    let pair = field(info, "tokens").and_then(|t| {
      let cache = field(t, "cache")?;
      Some((
        count(t, "input")? + count(cache, "read")? + count(cache, "write")?,
        count(t, "output")? + count(t, "reasoning")?,
      ))
    });
    if let (Some(id), Some(pair)) = (string(info, "id"), pair) {
      messages.insert(id.to_string(), pair);
    }
  }
  messages.into_values().collect()
}

/// Grok 1.0.34's `_x.ai/session/update` records contain per-prompt bills, not
/// cumulative session totals. Only the primary stream is supplied: its bills
/// already include subagents. Ignore context-window telemetry and modelUsage
/// breakdowns, and honor usageIsIncomplete (including outstanding subagent bills).
/// Source: xai-org/grok-build, xai-grok-shell/src/extensions/notification.rs.
pub fn grok_session_summary(stream: &str) -> Summary {
  let mut turns = BTreeMap::new();
  let mut session = None;
  let mut finished = false;
  for line in stream.lines().filter(|line| !line.trim().is_empty()) {
    let Ok(event) = json::parse(line) else { return unavailable(Harness::Grok) };
    let Some(params) = field(&event, "params") else { return unavailable(Harness::Grok) };
    let Some(id) = string(params, "sessionId") else { return unavailable(Harness::Grok) };
    if session.as_deref().is_some_and(|previous| previous != id) {
      return unavailable(Harness::Grok);
    }
    session = Some(id.to_string());
    let Some(update) = field(params, "update") else { return unavailable(Harness::Grok) };
    match string(update, "sessionUpdate") {
      Some("turn_completed") => {
        if string(&event, "method") != Some("_x.ai/session/update") {
          return unavailable(Harness::Grok);
        }
        let Some(prompt) = string(update, "prompt_id") else { return unavailable(Harness::Grok) };
        let Some(usage) = field(update, "usage") else { return unavailable(Harness::Grok) };
        let Some(tokens) = grok_tokens(usage) else { return unavailable(Harness::Grok) };
        let Some(calls) = count(usage, "modelCalls") else { return unavailable(Harness::Grok) };
        let bill = (tokens, calls);
        if turns.get(prompt).is_some_and(|previous| previous != &bill || !finished) {
          return unavailable(Harness::Grok);
        }
        turns.insert(prompt.to_string(), bill);
        finished = string(update, "stop_reason") == Some("end_turn");
      }
      Some(
        "user_message_chunk"
        | "agent_message_chunk"
        | "agent_thought_chunk"
        | "tool_call"
        | "tool_call_update"
        | "tool_call_delta_chunk",
      ) => finished = false,
      _ => {}
    }
  }
  if turns.is_empty() {
    return unavailable(Harness::Grok);
  }
  let mut total = Tokens { input: 0, output: 0, cache_read: 0, cache_write: Some(0) };
  let mut calls = 0u64;
  for (tokens, count) in turns.values() {
    if !add_tokens(&mut total, tokens) {
      return unavailable(Harness::Grok);
    }
    let Some(sum) = calls.checked_add(*count).filter(|n| *n <= 9_007_199_254_740_991) else {
      return unavailable(Harness::Grok);
    };
    calls = sum;
  }
  Summary {
    harness: Harness::Grok,
    complete: finished,
    tokens: Some(total),
    llm_round_trips: Some(calls),
    // The primary transcript omits child tool calls; do not present a partial count as exact.
    tool_calls: None,
  }
}

/// One Cline session: its `<id>.json` document and, when present, `<id>.messages.json`.
#[derive(Debug, Clone, Default)]
pub struct ClineSession {
  pub session: String,
  pub messages: Option<String>,
}

/// One Cline session. Tokens come from the session document's `aggregateUsage` when the CLI
/// wrote it, otherwise from `usage`. Round trips and tool calls come from the conversation:
/// one assistant message with `metrics` per model response, one `tool_use` block per call —
/// the same two counts Claude reports, so the routes compare directly. Both interfaces (TUI
/// and `--json`) write these files, so `cline` and `cline-headless` are counted identically.
/// A session that has not finished is incomplete.
pub fn cline_session_summary(session: &ClineSession) -> Summary {
  let Ok(root) = json::parse(&session.session) else { return unavailable(Harness::Cline) };
  let (llm_round_trips, tool_calls) = match session.messages.as_deref().map(cline_conversation_counts) {
    Some(Some((trips, tools))) => (Some(trips), Some(tools)),
    _ => (None, None),
  };
  let usage = field(&root, "metadata").and_then(|meta| field(meta, "aggregateUsage").or_else(|| field(meta, "usage")));
  // A turn still on its first response has no counters yet: incomplete, never zero.
  let tokens = match usage {
    Some(usage) => match cline_tokens(usage) {
      Some(tokens) => Some(tokens),
      None => return unavailable(Harness::Cline),
    },
    None => None,
  };
  Summary {
    harness: Harness::Cline,
    // Headless exits as `completed`. The TUI rests at `idle` with the counters already
    // final; `/quit` only happens after the host has accepted those counters.
    complete: tokens.is_some() && matches!(string(&root, "status"), Some("completed") | Some("idle")),
    tokens,
    llm_round_trips,
    tool_calls,
  }
}

/// Whether a Cline session document says a turn is under way (`status: running`). The
/// session opens when the prompt is submitted, before the model answers, so this is native
/// evidence that startup finished even while the first response is still thinking.
pub fn cline_session_running(session: &str) -> bool {
  json::parse(session).ok().is_some_and(|root| string(&root, "status") == Some("running"))
}

/// `(assistant responses, tool calls)` from a `messages.json`; `None` when it is unreadable.
fn cline_conversation_counts(text: &str) -> Option<(u64, u64)> {
  let root = json::parse(text).ok()?;
  let Some(Value::Array(messages)) = field(&root, "messages") else { return None };
  let mut trips = 0;
  let mut tools = 0;
  for message in messages.iter().filter(|m| string(m, "role") == Some("assistant")) {
    if field(message, "metrics").is_some() {
      trips += 1;
    }
    if let Some(Value::Array(blocks)) = field(message, "content") {
      tools += blocks.iter().filter(|b| string(b, "type") == Some("tool_use")).count() as u64;
    }
  }
  Some((trips, tools))
}

/// Sum every session from one attempt. Any unfinished session keeps the total incomplete;
/// any session without counters keeps tokens unavailable rather than understated.
pub fn cline_sessions_summary(sessions: &[ClineSession]) -> Summary {
  if sessions.is_empty() {
    return unavailable(Harness::Cline);
  }
  let mut total = Some(Tokens { input: 0, output: 0, cache_read: 0, cache_write: Some(0) });
  let mut complete = true;
  let mut trips = Some(0u64);
  let mut tools = Some(0u64);
  for session in sessions {
    let summary = cline_session_summary(session);
    complete &= summary.complete;
    trips = trips.zip(summary.llm_round_trips).map(|(a, b)| a + b);
    tools = tools.zip(summary.tool_calls).map(|(a, b)| a + b);
    total = match (total, summary.tokens) {
      (Some(mut sum), Some(tokens)) => add_tokens(&mut sum, &tokens).then_some(sum),
      _ => None,
    };
  }
  Summary {
    harness: Harness::Cline,
    complete: complete && total.is_some(),
    tokens: total,
    llm_round_trips: trips,
    tool_calls: tools,
  }
}

/// Cline's `inputTokens` is the whole prompt, cache included: across one session, each
/// response's `cacheReadTokens` is the previous response's `inputTokens`. Subtract the cache
/// buckets, as for Codex and Grok, so `input` means uncached input for every harness. Counters
/// that cannot be split that way are refused rather than guessed.
fn cline_tokens(usage: &Value) -> Option<Tokens> {
  let input = count(usage, "inputTokens")?;
  let cache_read = count(usage, "cacheReadTokens")?;
  let cache_write = count(usage, "cacheWriteTokens")?;
  Some(Tokens {
    input: input.checked_sub(cache_read.checked_add(cache_write)?)?,
    output: count(usage, "outputTokens")?,
    cache_read,
    cache_write: Some(cache_write),
  })
}

fn grok_tokens(usage: &Value) -> Option<Tokens> {
  if !matches!(field(usage, "usageIsIncomplete"), None | Some(Value::Bool(false))) {
    return None;
  }
  let input = count(usage, "inputTokens")?;
  let output = count(usage, "outputTokens")?;
  let cache_read = count(usage, "cachedReadTokens")?;
  let cache_write = count(usage, "cacheCreationTokens")?;
  if count(usage, "totalTokens")? != input.checked_add(output)? {
    return None;
  }
  Some(Tokens {
    input: input.checked_sub(cache_read.checked_add(cache_write)?)?,
    output,
    cache_read,
    cache_write: Some(cache_write),
  })
}

fn add_tokens(total: &mut Tokens, add: &Tokens) -> bool {
  let Some(input) = total.input.checked_add(add.input) else { return false };
  let Some(output) = total.output.checked_add(add.output) else { return false };
  let Some(cache_read) = total.cache_read.checked_add(add.cache_read) else { return false };
  let cache_write = match (total.cache_write, add.cache_write) {
    (Some(left), Some(right)) => {
      let Some(sum) = left.checked_add(right) else { return false };
      Some(sum)
    }
    _ => None,
  };
  if [input, output, cache_read, cache_write.unwrap_or(0)].iter().any(|n| *n > 9_007_199_254_740_991) {
    return false;
  }
  *total = Tokens { input, output, cache_read, cache_write };
  true
}

fn hook_name(event: &Value) -> Option<&str> {
  string(event, "hook_event_name").or_else(|| string(event, "event_name"))
}

fn hook_tokens(event: &Value) -> Option<Tokens> {
  let input = count(event, "input_tokens").or_else(|| count(event, "inputTokens"))?;
  let output = count(event, "output_tokens").or_else(|| count(event, "outputTokens"))?;
  let cache_read = count(event, "cache_read_tokens").or_else(|| count(event, "cacheReadTokens"))?;
  let cache_write = count(event, "cache_write_tokens").or_else(|| count(event, "cacheWriteTokens"))?;
  Some(Tokens {
    input: input.checked_sub(cache_read.checked_add(cache_write)?)?,
    output,
    cache_read,
    cache_write: Some(cache_write),
  })
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
  match value {
    Value::Object(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
    _ => None,
  }
}

fn object_keys(value: &Value) -> Option<BTreeSet<&str>> {
  match value {
    Value::Object(fields) => Some(fields.iter().map(|(key, _)| key.as_str()).collect()),
    _ => None,
  }
}

fn object_len(value: &Value) -> Option<usize> {
  match value {
    Value::Object(fields) => Some(fields.len()),
    _ => None,
  }
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
  match field(value, key)? {
    Value::String(value) if !value.is_empty() => Some(value),
    _ => None,
  }
}

fn count(value: &Value, key: &str) -> Option<u64> {
  match field(value, key)? {
    Value::Number(n) if n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0 => {
      Some(*n as u64)
    }
    _ => None,
  }
}

fn optional_count(value: &Value, key: &str) -> Option<Option<u64>> {
  match field(value, key)? {
    Value::Null => Some(None),
    _ => count(value, key).map(Some),
  }
}

fn optional_number(value: Option<u64>) -> Value {
  value.map(|n| Value::Number(n as f64)).unwrap_or(Value::Null)
}

fn object(fields: Vec<(&str, Value)>) -> Value {
  Value::Object(fields.into_iter().map(|(key, value)| (key.to_string(), value)).collect())
}

fn number(n: usize) -> Value {
  Value::Number(n as f64)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn native_work_requires_positive_evidence_but_not_complete_accounting() {
    let mut summary = unavailable(Harness::Codex);
    assert!(!summary.has_started_work());
    summary.complete = true;
    summary.tool_calls = Some(0);
    summary.llm_round_trips = Some(0);
    summary.tokens = Some(Tokens { input: 0, output: 0, cache_read: 0, cache_write: Some(0) });
    assert!(!summary.has_started_work(), "a metadata-only or empty session is not work");
    summary.complete = false;
    summary.tokens.as_mut().unwrap().output = 1;
    assert!(summary.has_started_work(), "partial model spend proves startup finished");
    summary.tokens = None;
    summary.tool_calls = Some(1);
    assert!(summary.has_started_work(), "a tool call needs no token counters");
  }

  #[test]
  fn native_work_is_detected_in_unfinished_cursor_and_codex_sessions() {
    let cursor = cursor_hook_summary(
      r#"{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g","tool_use_id":"read-proposal"}"#,
    );
    assert!(!cursor.complete);
    assert!(cursor.has_started_work());
    let codex = codex_session_summary(&[Some(
      r#"{"type":"session_meta","payload":{"id":"review"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":3}}}}
"#.into(),
    )]).unwrap();
    assert!(codex.has_started_work(), "no task-complete event is needed");
    assert!(!cursor_hook_summary("").has_started_work());
    assert!(!cursor_hook_summary("incomplete json").has_started_work());
  }

  #[test]
  fn native_work_is_detected_in_unfinished_claude_and_grok_sessions() {
    // A transcript mid-write: one finished assistant response, then a half-flushed line.
    let claude = claude_session_summary(&[Some(
      r#"{"type":"assistant","requestId":"r1","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"tool_use","id":"t1"}]}}
{"type":"assistant","message":{"id":"m2""#
        .into(),
    )])
    .unwrap();
    assert!(!claude.complete);
    assert!(claude.has_started_work(), "a torn trailing line must not hide the finished response");
    assert!(claude_session_summary(&[Some(r#"{"type":"user","message":{"content":"go"}}"#.into())]).is_none());
    assert!(grok_session_summary(GROK).has_started_work());
    assert!(!grok_session_summary("").has_started_work());
  }

  pub(crate) const GROK: &str = include_str!("../tests/fixtures/grok-usage/updates.jsonl");

  #[test]
  fn grok_real_turn_matches_native_usage_and_round_trips() {
    let summary = grok_session_summary(GROK);
    assert!(summary.complete);
    assert_eq!(summary.tokens, Some(Tokens { input: 20_974, output: 191, cache_read: 19_456, cache_write: Some(0) }));
    assert_eq!(summary.llm_round_trips, Some(2));
    assert_eq!(summary.tool_calls, None);
    assert_eq!(Summary::from_json(&summary.to_json()), Some(summary));
  }

  #[test]
  fn grok_counts_turns_once_and_does_not_add_model_breakdowns() {
    let completed = GROK.lines().last().unwrap();
    let duplicated = format!("{GROK}{completed}");
    assert_eq!(grok_session_summary(&duplicated), grok_session_summary(GROK));
    let two_turns = format!("{GROK}{}", GROK.replace("prompt-1", "prompt-2"));
    let summary = grok_session_summary(&two_turns);
    assert!(summary.complete);
    assert_eq!(summary.tokens.unwrap().input, 41_948);
    assert_eq!(summary.llm_round_trips, Some(4));
  }

  #[test]
  fn grok_rejects_missing_partial_inconsistent_or_invalid_counters() {
    for (from, to) in [
      ("\"inputTokens\":40430", "\"inputTokens\":-1"),
      ("\"outputTokens\":191", "\"outputTokens\":1.5"),
      ("\"cachedReadTokens\":19456", "\"cachedReadTokens\":50000"),
      ("\"cacheCreationTokens\":0,", ""),
      ("\"modelCalls\":2", "\"modelCalls\":null"),
      ("\"totalTokens\":40621", "\"totalTokens\":1"),
      ("\"usage\":{", "\"usage\":{\"usageIsIncomplete\":true,"),
      ("\"usage\":{", "\"usage\":{\"usageIsIncomplete\":\"false\","),
    ] {
      let summary = grok_session_summary(&GROK.replace(from, to));
      assert!(!summary.complete, "{to}");
      assert_eq!(summary.tokens, None, "{to}");
    }
    assert_eq!(grok_session_summary("").tokens, None);
    assert_eq!(grok_session_summary(&format!("{GROK}{{")).tokens, None);
    let conflict = format!("{GROK}{}", GROK.lines().last().unwrap().replace("\"modelCalls\":2", "\"modelCalls\":3"));
    assert_eq!(grok_session_summary(&conflict).tokens, None);
  }

  #[test]
  fn grok_cache_writes_are_disjoint_and_sum_overflow_is_unavailable() {
    let summary = grok_session_summary(&GROK.replace("\"cacheCreationTokens\":0", "\"cacheCreationTokens\":100"));
    let tokens = summary.tokens.unwrap();
    assert_eq!(tokens.input, 20_874);
    assert_eq!(tokens.cache_write, Some(100));
    let large = GROK.replace("\"modelCalls\":2", "\"modelCalls\":9007199254740991");
    assert!(grok_session_summary(&large).complete);
    assert!(!grok_session_summary(&format!("{large}{}", large.replace("prompt-1", "prompt-2"))).complete);
  }

  #[test]
  fn grok_waits_for_final_bill_and_rejects_stale_completion_after_resuming() {
    let open = GROK.lines().filter(|line| !line.contains("turn_completed")).collect::<Vec<_>>().join("\n");
    assert!(!grok_session_summary(&open).complete);
    assert!(grok_session_summary(GROK).complete);
    let resumed = format!("{GROK}{}\n", GROK.lines().next().unwrap());
    assert!(!grok_session_summary(&resumed).complete);
    assert!(!grok_session_summary(&format!("{resumed}{}", GROK.lines().last().unwrap())).complete);
    assert!(!grok_session_summary(&GROK.replace("end_turn", "cancelled")).complete);
  }

  // Hook input_tokens includes cache. Last stop is the cumulative turn total.
  const HOOKS: &str = r#"{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g1","model":"cursor-grok-4.6-high-fast","input_tokens":45461,"output_tokens":80,"cache_read_tokens":30144,"cache_write_tokens":0}
{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g1","tool_use_id":"t1"}
{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g2","input_tokens":45544,"output_tokens":120,"cache_read_tokens":30144,"cache_write_tokens":0}
{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g2","tool_use_id":"t2"}
{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g3","input_tokens":45561,"output_tokens":190,"cache_read_tokens":30144,"cache_write_tokens":0}
{"hook_event_name":"stop","conversation_id":"c","generation_id":"g3","status":"completed","input_tokens":45561,"output_tokens":190,"cache_read_tokens":30144,"cache_write_tokens":0}
"#;

  #[test]
  fn hook_tokens_subtract_cache_and_keep_the_last_cumulative_payload() {
    let summary = cursor_hook_summary(HOOKS);
    assert!(summary.complete);
    let tokens = summary.tokens.as_ref().expect("tokens");
    assert_eq!(tokens.input, 45561 - 30144);
    assert_eq!(tokens.output, 190);
    assert_eq!(tokens.cache_read, 30144);
    assert_eq!(tokens.cache_write, Some(0));
    assert_eq!(summary.llm_round_trips, Some(3));
    assert_eq!(summary.tool_calls, Some(2));
    assert!(summary.phrase().contains("190 output tokens"));
    assert!(summary.phrase().contains("3 LLM calls"));
  }

  #[test]
  fn cursor_child_tools_do_not_require_separate_token_totals() {
    // Reduced from job awsoof: children emit only tools, with null transcripts.
    let children = r#"{"hook_event_name":"postToolUse","conversation_id":"child1","generation_id":"child1","transcript_path":null,"tool_use_id":"a"}
{"hook_event_name":"postToolUse","conversation_id":"child2","generation_id":"child2","transcript_path":null,"tool_use_id":"b"}
{"hook_event_name":"postToolUse","conversation_id":"child3","generation_id":"child3","transcript_path":null,"tool_use_id":"c"}
{"hook_event_name":"postToolUse","conversation_id":"child1","generation_id":"child1","transcript_path":"child1.jsonl","tool_use_id":"d"}
"#;
    let stream = format!("{children}{HOOKS}");
    let summary = cursor_hook_summary(&stream);
    assert!(summary.complete);
    assert_eq!(summary.tokens, cursor_hook_summary(HOOKS).tokens);
    assert_eq!(summary.tool_calls, Some(6));
    assert_eq!(summary.llm_round_trips, Some(6));
    assert!(!cursor_hook_summary(children).complete, "children alone cannot authorize shutdown");
    assert_eq!(cursor_hook_summary(children).tokens, None);
    for altered in [
      stream.replace("\"transcript_path\":null,", ""),
      stream.replace("\"transcript_path\":null", "\"transcript_path\":\"parent.jsonl\""),
      stream.replace("\"generation_id\":\"child1\"", "\"generation_id\":\"other\""),
      stream.replace("\"tool_use_id\":\"a\"", "\"unused\":\"a\""),
      stream.lines().filter(|line| !line.contains("\"stop\"")).collect::<Vec<_>>().join("\n"),
    ] {
      assert!(!cursor_hook_summary(&altered).complete, "unproven completion must still block shutdown");
    }
  }

  #[test]
  fn cursor_child_shape_cannot_hide_an_incomplete_accounted_conversation() {
    let child = r#"{"hook_event_name":"postToolUse","conversation_id":"child","generation_id":"child","transcript_path":null,"tool_use_id":"a"}
"#;
    let response = r#"{"hook_event_name":"afterAgentResponse","conversation_id":"child","generation_id":"child"}
"#;
    for stream in [format!("{HOOKS}{child}{response}"), format!("{HOOKS}{response}{child}")] {
      let summary = cursor_hook_summary(&stream);
      assert!(!summary.complete);
      assert_eq!(summary.tokens, None);
    }
  }

  #[test]
  fn parallel_tools_share_one_generation() {
    let stream = r#"{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g1","input_tokens":10,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0}
{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g1","tool_use_id":"t1"}
{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g1","tool_use_id":"t2"}
{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g2","input_tokens":12,"output_tokens":4,"cache_read_tokens":0,"cache_write_tokens":0}
{"hook_event_name":"stop","conversation_id":"c","generation_id":"g2","status":"completed","input_tokens":12,"output_tokens":4,"cache_read_tokens":0,"cache_write_tokens":0}
"#;
    let summary = cursor_hook_summary(stream);
    assert_eq!(summary.llm_round_trips, Some(2));
    assert_eq!(summary.tool_calls, Some(2));
    assert_eq!(summary.tokens.unwrap().input, 12);
  }

  #[test]
  fn missing_or_malformed_tokens_stay_null() {
    for text in [
      String::new(),
      r#"{"hook_event_name":"stop","conversation_id":"c","generation_id":"g","status":"completed","input_tokens":null,"output_tokens":1,"cache_read_tokens":0,"cache_write_tokens":0}
"#
      .into(),
      r#"{"hook_event_name":"stop","conversation_id":"c","generation_id":"g","status":"completed","input_tokens":-1,"output_tokens":1,"cache_read_tokens":0,"cache_write_tokens":0}
"#
      .into(),
      format!("{HOOKS}truncated"),
      format!(r#"{HOOKS}{{"hook_event_name":"stop","conversation_id":"c","generation_id":"g3","status":"completed","input_tokens":null}}"#),
    ] {
      let summary = cursor_hook_summary(&text);
      assert_eq!(summary.tokens, None, "{text}");
      assert!(!summary.complete);
    }
  }

  #[test]
  fn hooks_json_names_the_container_script() {
    let text = hooks_json("/home/agent/repo/tmp/.cursor/scsh-cursor-usage.py");
    assert!(text.contains("afterAgentResponse"));
    assert!(text.contains("postToolUse"));
    assert!(text.contains("/home/agent/repo/tmp/.cursor/scsh-cursor-usage.py"));
  }

  #[test]
  fn claude_transcripts_sum_responses_and_subagents() {
    let root = std::env::temp_dir().join(format!("scsh-claude-usage-{}", crate::runtime::random_nonce_6()));
    let subagent = root.join("projects/repo/subagents");
    std::fs::create_dir_all(&subagent).unwrap();
    std::fs::write(
      root.join("projects/repo/main.jsonl"),
      r#"{"type":"assistant","uuid":"a","message":{"content":[{"type":"tool_use","id":"t"}],"usage":{"input_tokens":10,"output_tokens":3,"cache_read_input_tokens":20,"cache_creation_input_tokens":4}}}
{"type":"user","uuid":"u","message":{"content":"next"}}
{"type":"assistant","uuid":"b","message":{"content":[],"usage":{"input_tokens":2,"output_tokens":5,"cache_read_input_tokens":6,"cache_creation_input_tokens":1}}}
"#,
    )
    .unwrap();
    std::fs::write(
      subagent.join("agent-x.jsonl"),
      r#"{"type":"assistant","uuid":"c","message":{"content":[],"usage":{"input_tokens":7,"output_tokens":11,"cache_read_input_tokens":13,"cache_creation_input_tokens":17}}}
"#,
    )
    .unwrap();
    let summary = crate::daemon::usage::claude_session_summary(&root).unwrap();
    assert_eq!(summary.harness, Harness::ClaudeCode);
    assert_eq!(summary.tokens, Some(Tokens { input: 19, output: 19, cache_read: 39, cache_write: Some(22) }));
    assert_eq!(summary.llm_round_trips, Some(3));
    assert_eq!(summary.tool_calls, Some(1));
    assert!(summary.complete);
    std::fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn codex_sessions_keep_each_files_last_cumulative_snapshot() {
    let root = std::env::temp_dir().join(format!("scsh-codex-usage-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(root.join("sessions/2026/09/18")).unwrap();
    let event = |input: u64, cached: u64, output: u64| {
      r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":$INPUT,"cached_input_tokens":$CACHED,"cache_write_input_tokens":0,"output_tokens":$OUTPUT}}}}
"#
        .replace("$INPUT", &input.to_string())
        .replace("$CACHED", &cached.to_string())
        .replace("$OUTPUT", &output.to_string())
    };
    std::fs::write(
      root.join("sessions/2026/09/18/parent.jsonl"),
      format!("{}{}", event(100, 80, 5), event(150, 100, 9)),
    )
    .unwrap();
    std::fs::write(root.join("sessions/2026/09/18/subagent.jsonl"), event(40, 30, 7)).unwrap();
    let summary = crate::daemon::usage::codex_session_summary(&root).unwrap();
    assert_eq!(summary.harness, Harness::Codex);
    assert_eq!(summary.tokens, Some(Tokens { input: 60, output: 16, cache_read: 130, cache_write: Some(0) }));
    assert!(summary.complete);
    std::fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn public_schema_is_single_key_strict_and_round_trips() {
    let summary = Summary {
      harness: Harness::Codex,
      complete: true,
      tokens: Some(Tokens { input: 1, output: 2, cache_read: 3, cache_write: Some(4) }),
      llm_round_trips: None,
      tool_calls: None,
    };
    assert_eq!(Summary::from_json(&summary.to_json()), Some(summary));
    assert!(Summary::from_json(
      r#"{"TokenUsage":{"schema_version":1,"harness":"codex","source":"codex_session_jsonl","complete":true,"tokens":null,"llm_round_trips":0,"tool_calls":0,"surprise":1}}"#
    )
    .is_none());
    assert!(Summary::from_json(
      r#"{"TokenUsage":{"schema_version":1,"schema_version":1,"harness":"codex","source":"codex_session_jsonl","complete":true,"tokens":null,"llm_round_trips":0,"tool_calls":0}}"#
    )
    .is_none());
  }

  #[test]
  fn compact_cost_hides_zero_counters_but_keeps_them_in_json() {
    let zero = Summary {
      harness: Harness::ClaudeCode,
      complete: true,
      tokens: Some(Tokens { input: 0, output: 0, cache_read: 0, cache_write: Some(0) }),
      llm_round_trips: Some(0),
      tool_calls: Some(0),
    };
    assert_eq!(zero.phrase(), "No measured cost");
    assert_eq!(zero.details(), "");
    assert!(zero.to_json().contains("\"input\": 0"));

    let input_only =
      Summary { tokens: Some(Tokens { input: 6, output: 0, cache_read: 0, cache_write: Some(0) }), ..zero };
    assert_eq!(input_only.phrase(), "6 input tokens");
    assert!(!input_only.phrase().contains("output"));
  }

  #[test]
  fn public_schema_rejects_bad_nested_fields_and_types() {
    let good = r#"{"TokenUsage":{"schema_version":1,"harness":"claude_code","source":"claude_session_jsonl","complete":true,"tokens":{"input":10,"output":2,"cache_read":3,"cache_write":4},"llm_round_trips":1,"tool_calls":0}}"#;
    assert!(Summary::from_json(good).is_some());
    for (from, to) in [
      (r#""complete":true"#, r#""complete":"true""#),
      (r#""input":10"#, r#""input":-1"#),
      (r#""input":10"#, r#""input":1.5"#),
      (r#""input":10"#, r#""input":10,"input":10"#),
      (r#""input":10"#, r#""input":10,"typo":0"#),
      (r#""schema_version":1"#, r#""schema_version":2"#),
      (r#""source":"claude_session_jsonl""#, r#""source":"codex_session_jsonl""#),
      (r#""tool_calls":0"#, r#""tool_calls":false"#),
      (r#"{"input":10,"output":2,"cache_read":3,"cache_write":4}"#, "null"),
    ] {
      assert!(Summary::from_json(&good.replace(from, to)).is_none(), "accepted {to}");
    }
  }

  #[test]
  fn claude_counts_a_provider_response_once_across_transcript_blocks() {
    let root = std::env::temp_dir().join(format!("scsh-claude-dedup-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&root).unwrap();
    let text = r#"{"type":"assistant","uuid":"a","requestId":"r","message":{"id":"msg1","content":[{"type":"thinking"}],"usage":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":20,"cache_creation_input_tokens":4}}}
{"type":"assistant","uuid":"b","requestId":"r","message":{"id":"msg1","content":[{"type":"tool_use","id":"tool1"}],"usage":{"input_tokens":10,"output_tokens":8,"cache_read_input_tokens":20,"cache_creation_input_tokens":4}}}
{"type":"assistant","uuid":"c","requestId":"r","message":{"id":"msg1","content":[{"type":"tool_use","id":"tool2"}],"usage":{"input_tokens":10,"output_tokens":8,"cache_read_input_tokens":20,"cache_creation_input_tokens":4}}}
"#;
    std::fs::write(root.join("parent.jsonl"), text).unwrap();
    std::fs::write(root.join("copied-history.jsonl"), text).unwrap();
    let summary = crate::daemon::usage::claude_session_summary(&root).unwrap();
    assert_eq!(summary.llm_round_trips, Some(1));
    assert_eq!(summary.tool_calls, Some(2));
    assert_eq!(summary.tokens, Some(Tokens { input: 10, output: 8, cache_read: 20, cache_write: Some(4) }));
    std::fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn native_cache_buckets_are_validated_and_absence_is_not_zero() {
    let old_codex = json::parse(r#"{"input_tokens":100,"cached_input_tokens":40,"output_tokens":3}"#).unwrap();
    assert_eq!(codex_tokens(&old_codex).unwrap().cache_write, None);
    let new_codex =
      json::parse(r#"{"input_tokens":100,"cached_input_tokens":40,"cache_write_input_tokens":5,"output_tokens":3}"#)
        .unwrap();
    assert_eq!(codex_tokens(&new_codex).unwrap().input, 55);
    for text in [
      r#"{"input_tokens":3,"cached_input_tokens":4,"output_tokens":1}"#,
      r#"{"input_tokens":100,"cached_input_tokens":40,"cache_write_input_tokens":-1,"output_tokens":3}"#,
    ] {
      assert!(codex_tokens(&json::parse(text).unwrap()).is_none());
    }
  }

  #[test]
  fn codex_forked_history_is_not_counted_as_fresh_spend() {
    let stream = r#"{"type":"session_meta","payload":{"id":"child","forked_from_id":"parent"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":3}}}}
"#;
    let summary = codex_session_summary(&[Some(stream.into())]).unwrap();
    assert!(!summary.complete);
    assert_eq!(summary.tokens, None);
    assert_eq!(summary.llm_round_trips, None);
  }

  #[cfg(unix)]
  #[test]
  fn transcript_scan_refuses_symlinks_instead_of_following_cycles() {
    let root = std::env::temp_dir().join(format!("scsh-usage-link-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
    assert!(crate::daemon::usage::jsonl_files(&root).is_err());
    std::fs::remove_dir_all(root).unwrap();
  }

  #[cfg(unix)]
  #[test]
  fn concurrent_cursor_hooks_append_whole_json_records() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let root = std::env::temp_dir().join(format!("scsh-hooks-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("hooks.jsonl");
    let mut children = Vec::new();
    for index in 0..8 {
      let mut child = Command::new("python3")
        .args(["-c", CURSOR_HOOK_PY])
        .env("SCSH_CURSOR_HOOKS_LOG", &log)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
      let payload = object(vec![("index", number(index)), ("text", Value::String("x".repeat(10_000)))]);
      child.stdin.take().unwrap().write_all(json::write_pretty(&payload).as_bytes()).unwrap();
      children.push(child);
    }
    for child in children {
      let result = child.wait_with_output().unwrap();
      assert!(result.status.success());
      assert_eq!(result.stdout, b"{}\n");
    }
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 8);
    let indexes: BTreeSet<_> = text.lines().map(|line| count(&json::parse(line).unwrap(), "index").unwrap()).collect();
    assert_eq!(indexes.len(), 8);
    std::fs::remove_dir_all(root).unwrap();
  }

  #[cfg(unix)]
  #[test]
  fn cursor_hook_only_records_events_and_never_authorizes_shutdown() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let root = std::env::temp_dir().join(format!("scsh-hook-ready-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("hooks.jsonl");
    let ready = root.join("usage-ready");
    let run = |payload: &str| {
      let mut child = Command::new("python3")
        .args(["-c", CURSOR_HOOK_PY])
        .env("SCSH_CURSOR_HOOKS_LOG", &log)
        .env("SCSH_CURSOR_USAGE_READY", &ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
      child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
      assert!(child.wait_with_output().unwrap().status.success());
    };
    run(
      r#"{"hook_event_name":"stop","status":"completed","input_tokens":null,"output_tokens":2,"cache_read_tokens":3,"cache_write_tokens":0}"#,
    );
    assert!(!ready.exists(), "incomplete counters must not release teardown");
    run(
      r#"{"hook_event_name":"stop","status":"completed","input_tokens":10,"output_tokens":2,"cache_read_tokens":3,"cache_write_tokens":0}"#,
    );
    assert!(!ready.exists(), "hooks must never publish a sticky readiness marker");
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(!text.contains("scsh_result_revision"), "the hook log contains only Cursor's event data");
    std::fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn cursor_completed_stop_survives_its_trailing_response_and_session_end() {
    let stream = r#"{"hook_event_name":"stop","conversation_id":"c","generation_id":"g","status":"completed","input_tokens":294213,"output_tokens":8032,"cache_read_tokens":171008,"cache_write_tokens":0}
{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g"}
{"hook_event_name":"sessionEnd","conversation_id":"c","status":"completed"}
"#;
    let summary = cursor_hook_summary(stream);
    assert!(summary.complete);
    assert_eq!(
      summary.tokens,
      Some(Tokens { input: 123_205, output: 8_032, cache_read: 171_008, cache_write: Some(0) })
    );

    let without_generation = stream.replace(r#","generation_id":"g""#, "");
    assert!(cursor_hook_summary(&without_generation).complete, "generation ids are optional in Cursor hook payloads");

    let resumed = stream.replacen(
      r#"{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g"}"#,
      r#"{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"next"}"#,
      1,
    );
    assert!(!cursor_hook_summary(&resumed).complete, "a later generation still invalidates the completed stop");
  }

  #[test]
  fn resumed_cursor_work_invalidates_a_previous_completed_stop() {
    let text = format!(
      r#"{HOOKS}{{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g4","tool_use_id":"t4"}}
{{"hook_event_name":"sessionEnd","conversation_id":"c","reason":"completed"}}
"#
    );
    assert!(!cursor_hook_summary(&text).complete);
    let stale_stop = format!(
      r#"{text}{{"hook_event_name":"stop","conversation_id":"c","generation_id":"g3","status":"completed","input_tokens":10,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0}}"#
    );
    assert!(!cursor_hook_summary(&stale_stop).complete, "an old generation's late stop cannot finish new work");
  }

  #[test]
  fn native_turn_boundaries_reject_resumed_work() {
    let claude = r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}
"#;
    assert!(turn_finished(crate::config::Harness::Claude, claude));
    assert!(!turn_finished(crate::config::Harness::Claude, &format!("{claude}{{\"type\":\"user\"}}")));
    let codex = r#"{"type":"event_msg","payload":{"type":"task_complete"}}
"#;
    assert!(turn_finished(crate::config::Harness::Codex, codex));
    assert!(!turn_finished(
      crate::config::Harness::Codex,
      &format!("{codex}{{\"payload\":{{\"type\":\"task_started\"}}}}")
    ));
  }

  #[test]
  fn opencode_plugin_events_sum_completed_messages_and_wait_for_idle() {
    // Real stream (opencode 1.18, cline-pass/glm-5.3): a tool-calling step, then the answer.
    let msg = |id: &str, done: bool, input: u64, output: u64, reasoning: u64, read: u64, finish: &str| {
      let completed = if done { r#","completed":2"# } else { "" };
      format!(
        r#"{{"type":"message.updated","properties":{{"info":{{"id":"{id}","sessionID":"ses_a","role":"assistant","time":{{"created":1{completed}}},"tokens":{{"input":{input},"output":{output},"reasoning":{reasoning},"cache":{{"write":0,"read":{read}}}}},"finish":{finish}}}}}}}"#
      )
    };
    let status = |s: &str| {
      format!(r#"{{"type":"session.status","properties":{{"sessionID":"ses_a","status":{{"type":"{s}"}}}}}}"#)
    };
    let tool = r#"{"type":"message.part.updated","properties":{"part":{"id":"prt_1","sessionID":"ses_a","messageID":"msg_1","tool":"write","type":"tool"}}}"#;
    let lines = [
      status("busy"),
      msg("msg_1", false, 0, 0, 0, 0, "null"),
      tool.to_string(),
      msg("msg_1", true, 12043, 76, 22, 85, r#""tool-calls""#),
      msg("msg_2", false, 0, 0, 0, 0, "null"),
      msg("msg_2", true, 108, 3, 0, 12127, r#""stop""#),
      status("busy"),
    ];
    let busy = lines.join("\n");
    let open = opencode_events_summary(&busy);
    assert!(!open.complete, "a busy session is still generating");
    let done = opencode_events_summary(&format!("{busy}\n{}", status("idle")));
    assert!(done.complete);
    assert_eq!(done.harness, Harness::Opencode);
    assert_eq!(done.tokens, Some(Tokens { input: 12151, output: 101, cache_read: 12212, cache_write: Some(0) }));
    assert_eq!((done.llm_round_trips, done.tool_calls), (Some(2), Some(1)));
    let unfinished = format!("{}\n{}\n{}", status("busy"), msg("msg_1", false, 0, 0, 0, 0, "null"), status("idle"));
    assert!(!opencode_events_summary(&unfinished).complete, "a message without time.completed is not final");
    assert_eq!(opencode_events_summary("").tokens, None);
    let parsed = Summary::from_json(&done.to_json()).unwrap();
    assert_eq!(parsed, done);
  }

  #[test]
  fn cline_session_subtracts_cache_from_input_and_waits_for_completion() {
    // Real counters (cline 3.0.70, two responses): inputTokens includes the cache reads.
    let text = r#"{"status":"completed","interactive":false,"metadata":{"usage":{"inputTokens":11348,"outputTokens":23,"cacheReadTokens":490,"cacheWriteTokens":0,"totalCost":0},"aggregateUsage":{"inputTokens":11348,"outputTokens":23,"cacheReadTokens":490,"cacheWriteTokens":0,"totalCost":0}}}"#;
    let messages = r#"{"version":1,"messages":[
      {"role":"user","content":[{"type":"text","text":"go"}]},
      {"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"run_commands","input":{}}],"metrics":{"inputTokens":60}},
      {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[]}]},
      {"role":"assistant","content":[{"type":"text","text":"done"}],"metrics":{"inputTokens":40}}
    ]}"#;
    let session = |s: &str| ClineSession { session: s.to_string(), messages: Some(messages.to_string()) };
    let summary = cline_session_summary(&session(text));
    assert!(summary.complete);
    assert_eq!(
      summary.tokens,
      Some(Tokens { input: 10858, output: 23, cache_read: 490, cache_write: Some(0) }),
      "input is uncached input, as for every other harness"
    );
    assert_eq!((summary.llm_round_trips, summary.tool_calls), (Some(2), Some(1)), "counted like Claude's");
    assert!(!cline_session_summary(&session(&text.replace("completed", "running"))).complete);
    assert!(
      cline_session_summary(&session(&text.replace("completed", "idle"))).complete,
      "a finished TUI turn rests at idle until /quit"
    );
    // An open turn with no counters yet: incomplete, no invented zero, and no invented work.
    let open = ClineSession { session: r#"{"status":"running","metadata":{}}"#.into(), messages: None };
    let open_summary = cline_session_summary(&open);
    assert!(!open_summary.complete && open_summary.tokens.is_none() && !open_summary.has_started_work());
    assert!(cline_session_running(&open.session), "the open session itself is the startup evidence");
    assert!(
      cline_sessions_summary(&[session(text), open]).tokens.is_none(),
      "one session without counters keeps the total unavailable"
    );
    let saved = summary.to_json();
    let parsed = Summary::from_json(&saved).unwrap();
    assert_eq!(parsed.harness, Harness::Cline);
    assert_eq!(parsed, summary);
  }
}
