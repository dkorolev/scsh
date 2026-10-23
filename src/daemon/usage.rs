//! Local artifact collection; pure accounting and schema validation live in `crate::usage`.

use crate::usage::{cursor_hook_summary, unavailable, Harness, Summary};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Default quiet-turn accounting wait, allowing time for a delayed API retry to finish.
pub const DEFAULT_ACCOUNTING_TIMEOUT_SECS: u64 = 600;
/// Extra seconds after the accounting wait before a wedged container is abandoned.
const ACCOUNTING_LEAK_GRACE_SECS: u64 = 15;

/// How long to wait for native counters after the turn has gone quiet.
///
/// `SCSH_USAGE_ACCOUNTING_TIMEOUT` (seconds, > 0) overrides every harness. Otherwise
/// every harness gets [`DEFAULT_ACCOUNTING_TIMEOUT_SECS`].
pub fn accounting_timeout_secs() -> u64 {
  accounting_timeout_from_env(std::env::var("SCSH_USAGE_ACCOUNTING_TIMEOUT").ok().as_deref())
}

fn accounting_timeout_from_env(raw: Option<&str>) -> u64 {
  if let Some(raw) = raw {
    if let Ok(n) = raw.parse::<u64>() {
      if n > 0 {
        return n;
      }
    }
  }
  DEFAULT_ACCOUNTING_TIMEOUT_SECS
}

/// One host-owned deadline for every harness. The container only exits after this
/// controller has saved validated counters or journaled the accounting failure.
pub struct Completion {
  /// Quiet-turn instant that starts the accounting deadline. Screen activity or a
  /// growing native stream clears this so a live generation is not cut off.
  started: Option<Instant>,
  /// Authorization starts a separate, bounded graceful-exit allowance.
  released: Option<Instant>,
  /// Throttle native transcript reads without delaying watchdog protection.
  next_poll: Instant,
  /// Last observed native accounting artifact size, for detecting a still-open turn.
  native_bytes: Option<u64>,
}

impl Completion {
  /// Display the same monotonic budget enforced by `poll`, including cancellation
  /// while generation resumes and the separate graceful-exit allowance.
  pub fn recovery(&self) -> crate::recovery::Recovery {
    use crate::recovery::{deadline, Recovery};
    if let Some(released) = self.released {
      Recovery::Exit(deadline(released, Duration::from_secs(15)))
    } else if let Some(started) = self.started {
      Recovery::Accounting(deadline(started, Duration::from_secs(accounting_timeout_secs())))
    } else {
      Recovery::Finishing
    }
  }

  pub fn new() -> Self {
    Self { started: None, released: None, next_poll: Instant::now(), native_bytes: None }
  }

  pub fn poll(
    &mut self, harness: crate::config::Harness, dir: &Path, result: &Path, required: bool, live: bool,
  ) -> bool {
    let now = Instant::now();
    if let Some(released) = self.released {
      return now.duration_since(released) < Duration::from_secs(15);
    }
    if !result.is_file() {
      return false;
    }
    let wait = accounting_timeout_secs();
    if let Some(started) = self.started {
      if now.duration_since(started) >= Duration::from_secs(wait.saturating_add(ACCOUNTING_LEAK_GRACE_SECS)) {
        // Even a full disk preventing the shutdown instruction must not leak a container.
        return false;
      }
    }
    if now < self.next_poll {
      return true;
    }
    self.next_poll = now + Duration::from_millis(250);
    let error = dir.join(format!("{}.usage-error", crate::runtime::RUN_LOG_REL));
    let ready = if required { completed_usage(harness, dir, result) } else { None };
    let saved = if let Some(summary) = ready {
      crate::atomic_write(
        &dir.join(format!("{}.usage-final", crate::runtime::RUN_LOG_REL)),
        summary.to_json().as_bytes(),
      )
      .is_ok()
    } else {
      false
    };
    let grew = self.native_grew(harness, dir);
    // Cursor writes the result with a tool, then generates the final message. No hook
    // fires until that generation ends. A wall clock from the result file would kill
    // that turn and leave tokens=null. Hold the deadline while the screen or native
    // stream is still moving; start it only once the turn has gone quiet.
    let quiet = !live && !grew;
    let expired =
      quiet && self.started.map(|started| now.duration_since(started) >= Duration::from_secs(wait)).unwrap_or(false);
    if required && !saved && !expired {
      if quiet {
        let _ = self.started.get_or_insert(now);
      } else {
        self.started = None;
      }
      return true;
    }
    if required && !saved {
      // A timeout remains a failure even if late counters arrive during teardown.
      let detail = format!("timeout: native accounting did not complete within {wait}s of the turn going quiet");
      if crate::atomic_write(&error, detail.as_bytes()).is_err() {
        return false;
      }
    }
    if crate::atomic_write(&dir.join(format!("{}.shutdown", crate::runtime::RUN_LOG_REL)), b"exit").is_ok() {
      self.released = Some(now);
    }
    true
  }

  fn native_grew(&mut self, harness: crate::config::Harness, dir: &Path) -> bool {
    let bytes = accounting_bytes(harness, dir);
    match self.native_bytes {
      None => {
        self.native_bytes = Some(bytes);
        false
      }
      Some(previous) => {
        self.native_bytes = Some(bytes);
        bytes > previous
      }
    }
  }
}

fn accounting_bytes(harness: crate::config::Harness, dir: &Path) -> u64 {
  // A headless run writes the same native files as its agent's TUI run.
  let paths = match harness.agent() {
    crate::config::Harness::Cursor => {
      vec![dir.join(format!("{}.cursor-hooks.jsonl", crate::runtime::RUN_LOG_REL))]
    }
    crate::config::Harness::Claude => {
      jsonl_files(&dir.join(crate::runtime::CLAUDE_AUTH_REL).join(".claude/projects")).unwrap_or_default()
    }
    crate::config::Harness::Codex => {
      jsonl_files(&dir.join(crate::runtime::CODEX_FORWARD_REL).join("sessions")).unwrap_or_default()
    }
    crate::config::Harness::Grok => grok_transcript(dir).into_iter().collect(),
    crate::config::Harness::Cline => cline_session_files(dir).into_iter().flat_map(|(s, m)| [s, m]).collect(),
    crate::config::Harness::Opencode => vec![opencode_events(dir)],
    _ => return 0,
  };
  paths.iter().map(|path| std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)).sum()
}

/// Each attempt owns a fresh transcript tree. Require it to change at or after the
/// result and require its latest logical turn to be terminal.
fn completed_usage(harness: crate::config::Harness, dir: &Path, result: &Path) -> Option<Summary> {
  // A headless run writes the same native files as its agent's TUI run.
  let harness = harness.agent();
  let result_time = std::fs::metadata(result).ok()?.modified().ok()?;
  let paths = match harness {
    crate::config::Harness::Cursor => vec![dir.join(format!("{}.cursor-hooks.jsonl", crate::runtime::RUN_LOG_REL))],
    crate::config::Harness::Claude => {
      jsonl_files(&dir.join(crate::runtime::CLAUDE_AUTH_REL).join(".claude/projects")).ok()?
    }
    crate::config::Harness::Codex => jsonl_files(&dir.join(crate::runtime::CODEX_FORWARD_REL).join("sessions")).ok()?,
    crate::config::Harness::Grok => vec![grok_transcript(dir).ok()?],
    crate::config::Harness::Cline => cline_session_files(dir).into_iter().map(|(session, _)| session).collect(),
    crate::config::Harness::Opencode => vec![opencode_events(dir)],
    _ => return None,
  };
  let mut fresh = false;
  let mut streams = Vec::new();
  for path in &paths {
    let before = std::fs::metadata(path).ok()?;
    let stream = std::fs::read_to_string(path).ok()?;
    let after = std::fs::metadata(path).ok()?;
    if before.len() != after.len() || before.modified().ok()? != after.modified().ok()? {
      return None;
    }
    fresh |= after.modified().ok()? >= result_time;
    // Cursor and Grok have no separate turn boundary: their one summary below already
    // reports whether the latest turn is terminal, so their stream is parsed once.
    let summarized = matches!(
      harness,
      crate::config::Harness::Cursor
        | crate::config::Harness::Grok
        | crate::config::Harness::Cline
        | crate::config::Harness::Opencode
    );
    if !summarized && !crate::usage::turn_finished(harness, &stream) && !abandoned_subagent(harness, path, &after) {
      return None;
    }
    streams.push(Some(stream));
  }
  let summary = match harness {
    crate::config::Harness::Cursor => cursor_hook_summary(streams.first()?.as_deref()?),
    crate::config::Harness::Claude => crate::usage::claude_session_summary(&streams)?,
    crate::config::Harness::Codex => crate::usage::codex_session_summary(&streams)?,
    crate::config::Harness::Grok => crate::usage::grok_session_summary(streams.first()?.as_deref()?),
    crate::config::Harness::Cline => cline_summary(dir),
    crate::config::Harness::Opencode => crate::usage::opencode_events_summary(streams.first()?.as_deref()?),
    _ => return None,
  };
  (fresh && summary.complete && summary.tokens.is_some()).then_some(summary)
}

/// How long a Claude subagent transcript must sit untouched, without a finished turn, before it
/// counts as abandoned. Longer than Claude's default two-minute Bash timeout, so a subagent
/// waiting on a slow command still holds the accounting wait.
const ABANDONED_SUBAGENT_IDLE: Duration = Duration::from_secs(120);

/// A Claude subagent the main turn left behind mid-turn: its transcript under `subagents/` ends
/// on a tool call and has not changed for [`ABANDONED_SUBAGENT_IDLE`]. Its counters are final, so
/// it must not hold the main session's accounting until the deadline.
fn abandoned_subagent(harness: crate::config::Harness, path: &Path, meta: &std::fs::Metadata) -> bool {
  harness == crate::config::Harness::Claude
    && path.parent().and_then(Path::file_name).is_some_and(|dir| dir == "subagents")
    && meta.modified().ok().and_then(|at| at.elapsed().ok()).is_some_and(|idle| idle >= ABANDONED_SUBAGENT_IDLE)
}

/// Read the native, local accounting artifacts written by one interactive harness run.
/// This performs no network request and no model call.
pub fn from_run_dir(harness: crate::config::Harness, run_dir: &Path) -> Option<Summary> {
  match harness {
    crate::config::Harness::Claude | crate::config::Harness::ClaudeHeadless => Some(
      claude_session_summary(&run_dir.join(crate::runtime::CLAUDE_AUTH_REL).join(".claude/projects"))
        .unwrap_or_else(|| unavailable(Harness::ClaudeCode)),
    ),
    crate::config::Harness::Codex | crate::config::Harness::CodexHeadless => Some(
      codex_session_summary(&run_dir.join(crate::runtime::CODEX_FORWARD_REL).join("sessions"))
        .unwrap_or_else(|| unavailable(Harness::Codex)),
    ),
    crate::config::Harness::Cursor => {
      let path = run_dir.join(format!("{}.cursor-hooks.jsonl", crate::runtime::RUN_LOG_REL));
      Some(
        std::fs::read_to_string(path)
          .ok()
          .map(|stream| cursor_hook_summary(&stream))
          .unwrap_or_else(|| unavailable(Harness::Cursor)),
      )
    }
    crate::config::Harness::Grok => Some(
      grok_transcript(run_dir)
        .and_then(std::fs::read_to_string)
        .ok()
        .map(|stream| crate::usage::grok_session_summary(&stream))
        .unwrap_or_else(|| unavailable(Harness::Grok)),
    ),
    crate::config::Harness::Cline | crate::config::Harness::ClineHeadless => Some(cline_summary(run_dir)),
    crate::config::Harness::Opencode => Some(
      std::fs::read_to_string(opencode_events(run_dir))
        .ok()
        .map(|stream| crate::usage::opencode_events_summary(&stream))
        .unwrap_or_else(|| unavailable(Harness::Opencode)),
    ),
  }
}

/// The opencode usage plugin's event stream beside the run log.
fn opencode_events(run_dir: &Path) -> PathBuf {
  run_dir.join(format!("{}.opencode-events.jsonl", crate::runtime::RUN_LOG_REL))
}

/// The session ids of this attempt's Cline sessions: the ids Cline tags its requests with.
pub fn cline_session_ids(run_dir: &Path) -> std::collections::BTreeSet<String> {
  cline_session_files(run_dir)
    .into_iter()
    .filter_map(|(session, _)| session.file_stem().and_then(|s| s.to_str()).map(str::to_string))
    .collect()
}

/// The opencode plugin's event stream for this attempt, when it wrote one.
pub fn opencode_events_text(run_dir: &Path) -> Option<String> {
  std::fs::read_to_string(opencode_events(run_dir)).ok()
}

/// Every Cline session of this attempt as `(<id>.json, <id>.messages.json)` paths, sorted.
fn cline_session_files(run_dir: &Path) -> Vec<(PathBuf, PathBuf)> {
  let root = run_dir.join(crate::runtime::CLINE_FORWARD_REL).join("data/sessions");
  let mut files = Vec::new();
  let Ok(sessions) = std::fs::read_dir(&root) else { return files };
  for session in sessions.flatten() {
    let Ok(kind) = session.file_type() else { continue };
    if !kind.is_dir() {
      continue;
    }
    let dir = session.path();
    let Ok(name) = session.file_name().into_string() else { continue };
    let path = dir.join(format!("{name}.json"));
    if path.is_file() {
      files.push((path, dir.join(format!("{name}.messages.json"))));
    }
  }
  files.sort();
  files
}

fn cline_summary(run_dir: &Path) -> Summary {
  let sessions: Vec<crate::usage::ClineSession> = cline_session_files(run_dir)
    .iter()
    .filter_map(|(session, messages)| {
      Some(crate::usage::ClineSession {
        session: std::fs::read_to_string(session).ok()?,
        messages: std::fs::read_to_string(messages).ok(),
      })
    })
    .collect();
  crate::usage::cline_sessions_summary(&sessions)
}

/// Native evidence that a run's agent has started work: counted model work in its usage
/// files, or — for Cline, whose first response reports no counters until it ends — a session
/// whose turn is open. Feeds the startup watchdog only; it never invents accounting.
pub fn native_work_started(harness: crate::config::Harness, run_dir: &Path) -> bool {
  if from_run_dir(harness, run_dir).is_some_and(|usage| usage.has_started_work()) {
    return true;
  }
  harness.agent() == crate::config::Harness::Cline
    && cline_session_files(run_dir)
      .iter()
      .any(|(session, _)| std::fs::read_to_string(session).is_ok_and(|text| crate::usage::cline_session_running(&text)))
}

/// Fresh GROK_HOME contains one primary session plus optional children. The primary
/// bills already fold in child spend. Select it by native metadata, never directory order.
fn grok_transcript(run_dir: &Path) -> std::io::Result<PathBuf> {
  let root = run_dir.join(crate::runtime::GROK_FORWARD_REL).join("sessions");
  let files = jsonl_files(&root)?;
  let mut primary = None;
  for events in files.iter().filter(|path| path.file_name().is_some_and(|name| name == "events.jsonl")) {
    let text = std::fs::read_to_string(events)?;
    let id =
      crate::usage::grok_primary_session(&text).map_err(|_| std::io::Error::other("invalid Grok session metadata"))?;
    if let Some(id) = id {
      let dir = events.parent().ok_or_else(|| std::io::Error::other("missing Grok session directory"))?;
      if dir.file_name().and_then(|name| name.to_str()) != Some(&id) {
        return Err(std::io::Error::other("Grok session id does not match its directory"));
      }
      let updates = dir.join("updates.jsonl");
      if !files.contains(&updates) {
        return Err(std::io::Error::other("missing Grok session updates"));
      }
      if primary.replace(updates).is_some() {
        return Err(std::io::Error::other("more than one primary Grok session in one attempt"));
      }
    }
  }
  primary.ok_or_else(|| std::io::Error::other("no primary Grok session in this attempt"))
}

pub(crate) fn claude_session_summary(root: &Path) -> Option<Summary> {
  let transcripts: Vec<_> =
    jsonl_files(root).ok()?.into_iter().map(|path| std::fs::read_to_string(path).ok()).collect();
  crate::usage::claude_session_summary(&transcripts)
}

pub(crate) fn codex_session_summary(root: &Path) -> Option<Summary> {
  let transcripts: Vec<_> =
    jsonl_files(root).ok()?.into_iter().map(|path| std::fs::read_to_string(path).ok()).collect();
  crate::usage::codex_session_summary(&transcripts)
}

pub(crate) fn jsonl_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
  fn visit(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> std::io::Result<()> {
    if depth > 32 {
      return Err(std::io::Error::other("harness transcript tree exceeds 32 directory levels"));
    }
    for entry in std::fs::read_dir(dir)? {
      let entry = entry?;
      let path = entry.path();
      let kind = entry.file_type()?;
      if kind.is_symlink() {
        return Err(std::io::Error::other("harness transcript tree contains a symlink"));
      }
      if kind.is_dir() {
        visit(&path, out, depth + 1)?;
      } else if kind.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        out.push(path);
      }
    }
    Ok(())
  }
  let mut out = Vec::new();
  visit(root, &mut out, 0)?;
  out.sort();
  Ok(out)
}

#[cfg(test)]
mod completion_tests {
  use super::*;
  use crate::config::Harness as Agent;

  struct Run(PathBuf);
  impl Run {
    fn new() -> Self {
      let dir = std::env::temp_dir().join(format!("scsh-accounting-{}", crate::runtime::random_nonce_6()));
      std::fs::create_dir_all(dir.join("tmp")).unwrap();
      std::fs::write(dir.join("tmp/result.json"), "{}").unwrap();
      Self(dir)
    }
    fn artifact(&self, suffix: &str) -> PathBuf {
      self.0.join(format!("{}.{suffix}", crate::runtime::RUN_LOG_REL))
    }
    fn poll(&self, state: &mut Completion, harness: Agent, required: bool) -> bool {
      self.poll_live(state, harness, required, false)
    }
    fn poll_live(&self, state: &mut Completion, harness: Agent, required: bool, live: bool) -> bool {
      state.next_poll = Instant::now();
      state.poll(harness, &self.0, &self.0.join("tmp/result.json"), required, live)
    }
  }
  impl Drop for Run {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  fn grok_waits_for_primary_bill_and_ignores_child_totals() {
    let run = Run::new();
    let root = run.0.join(crate::runtime::GROK_FORWARD_REL).join("sessions/repo");
    let primary = root.join("session-1");
    let child = root.join("child");
    for (dir, relationship, id) in [(&primary, "primary", "session-1"), (&child, "subagent", "child")] {
      std::fs::create_dir_all(dir).unwrap();
      std::fs::write(
        dir.join("events.jsonl"),
        format!(r#"{{"type":"turn_started","session_relationship":"{relationship}","session_id":"{id}"}}"#),
      )
      .unwrap();
      std::fs::write(dir.join("updates.jsonl"), "").unwrap();
    }
    let fixture = include_str!("../../tests/fixtures/grok-usage/updates.jsonl");
    std::fs::write(child.join("updates.jsonl"), fixture.replace("session-1", "child")).unwrap();
    let mut state = Completion::new();
    assert!(run.poll(&mut state, Agent::Grok, true));
    assert!(!run.artifact("shutdown").exists(), "child completion cannot finish the primary turn");
    std::fs::write(primary.join("updates.jsonl"), fixture).unwrap();
    assert!(run.poll(&mut state, Agent::Grok, true));
    assert!(run.artifact("shutdown").exists());
    assert!(!run.artifact("usage-error").exists());
    let saved = Summary::from_json(&std::fs::read_to_string(run.artifact("usage-final")).unwrap()).unwrap();
    assert!(saved.complete);
    assert_eq!(saved.tokens.unwrap().input, 20_974, "child spend is already folded into the primary bill");
    assert_eq!(from_run_dir(Agent::Grok, &run.0).unwrap().llm_round_trips, Some(2));

    std::fs::File::options()
      .write(true)
      .open(primary.join("updates.jsonl"))
      .unwrap()
      .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
      .unwrap();
    assert!(completed_usage(Agent::Grok, &run.0, &run.0.join("tmp/result.json")).is_none());
    std::fs::write(
      child.join("events.jsonl"),
      r#"{"type":"turn_started","session_relationship":"primary","session_id":"child"}"#,
    )
    .unwrap();
    assert!(grok_transcript(&run.0).is_err(), "ambiguous primaries must not double-count");
  }

  #[test]
  fn delayed_native_counters_release_each_supported_harness_only_after_terminal_snapshot() {
    for (harness, path, stream) in [
      (
        Agent::Cursor,
        format!("{}.cursor-hooks.jsonl", crate::runtime::RUN_LOG_REL),
        r#"{"hook_event_name":"stop","conversation_id":"c","status":"completed","input_tokens":10,"output_tokens":2,"cache_read_tokens":3,"cache_write_tokens":0}"#,
      ),
      (
        Agent::Claude,
        format!("{}/.claude/projects/p/run.jsonl", crate::runtime::CLAUDE_AUTH_REL),
        r#"{"type":"assistant","message":{"id":"m","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":0}}}"#,
      ),
      (
        Agent::Codex,
        format!("{}/sessions/run.jsonl", crate::runtime::CODEX_FORWARD_REL),
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"output_tokens":2,"cached_input_tokens":3}}}}
{"type":"event_msg","payload":{"type":"task_complete"}}"#,
      ),
    ] {
      let run = Run::new();
      let mut state = Completion::new();
      assert!(run.poll(&mut state, harness, true));
      assert!(!run.artifact("shutdown").exists());
      state.started = Some(Instant::now() - Duration::from_secs(accounting_timeout_secs().saturating_sub(1)));
      assert!(run.poll(&mut state, harness, true));
      assert!(!run.artifact("shutdown").exists(), "the full accounting budget remains available");
      let transcript = run.0.join(path);
      std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
      std::fs::write(transcript, stream).unwrap();
      assert!(run.poll(&mut state, harness, true));
      assert!(run.artifact("shutdown").exists(), "{harness:?}");
      let saved = Summary::from_json(&std::fs::read_to_string(run.artifact("usage-final")).unwrap()).unwrap();
      assert!(saved.complete);
      assert!(!run.artifact("usage-error").exists());
    }
  }

  #[test]
  fn an_abandoned_claude_subagent_does_not_hold_finished_accounting() {
    let run = Run::new();
    let projects = run.0.join(crate::runtime::CLAUDE_AUTH_REL).join(".claude/projects/p");
    std::fs::create_dir_all(projects.join("s/subagents")).unwrap();
    let usage =
      r#""usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":0}"#;
    std::fs::write(
      projects.join("s.jsonl"),
      format!(r#"{{"type":"assistant","message":{{"id":"main","stop_reason":"end_turn",{usage}}}}}"#),
    )
    .unwrap();
    // The subagent's last response is a tool call that never got an answer.
    let subagent = projects.join("s/subagents/agent-a.jsonl");
    std::fs::write(
      &subagent,
      format!(r#"{{"type":"assistant","message":{{"id":"sub","stop_reason":"tool_use",{usage}}}}}"#),
    )
    .unwrap();
    let result = run.0.join("tmp/result.json");
    let age = |path: &Path, secs: u64| {
      let at = std::time::SystemTime::now() - Duration::from_secs(secs);
      std::fs::File::options().write(true).open(path).unwrap().set_modified(at).unwrap();
    };
    age(&result, 300);
    age(&subagent, 60);
    assert!(
      completed_usage(Agent::Claude, &run.0, &result).is_none(),
      "a recently active subagent may still be working"
    );
    age(&subagent, ABANDONED_SUBAGENT_IDLE.as_secs());
    let summary =
      completed_usage(Agent::Claude, &run.0, &result).expect("the main turn finished; the subagent was left behind");
    assert!(summary.complete);
    assert_eq!(summary.llm_round_trips, Some(2), "the abandoned subagent's spend still counts");
    // The main transcript gets no such allowance: an unfinished main turn always holds the wait.
    std::fs::rename(&subagent, projects.join("s.jsonl")).unwrap();
    age(&projects.join("s.jsonl"), 600);
    assert!(completed_usage(Agent::Claude, &run.0, &result).is_none());
  }

  #[test]
  fn all_harnesses_obey_required_accounting_and_opt_out() {
    for harness in [Agent::Cursor, Agent::Claude, Agent::Codex, Agent::Grok, Agent::Cline, Agent::Opencode] {
      let run = Run::new();
      let mut state = Completion::new();
      state.started = Some(Instant::now() - Duration::from_secs(accounting_timeout_secs() + 1));
      assert!(run.poll(&mut state, harness, true));
      assert!(run.artifact("usage-error").exists());
      if harness != Agent::Opencode {
        assert_eq!(
          std::fs::read_to_string(run.artifact("usage-error")).unwrap(),
          format!(
            "timeout: native accounting did not complete within {}s of the turn going quiet",
            accounting_timeout_secs()
          )
        );
      }
      assert!(run.artifact("shutdown").exists());
      state.released = Some(Instant::now() - Duration::from_secs(16));
      assert!(!run.poll(&mut state, harness, true), "teardown remains bounded");
      let fast = Run::new();
      assert!(fast.poll(&mut Completion::new(), harness, false));
      assert!(fast.artifact("shutdown").exists());
      assert!(!fast.artifact("usage-error").exists());
    }
  }

  #[test]
  fn old_cursor_stop_cannot_release_a_new_result() {
    let run = Run::new();
    let hooks = run.artifact("cursor-hooks.jsonl");
    std::fs::write(&hooks, r#"{"hook_event_name":"stop","status":"completed","input_tokens":10,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0}"#).unwrap();
    std::fs::File::options()
      .write(true)
      .open(hooks)
      .unwrap()
      .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
      .unwrap();
    assert!(run.poll(&mut Completion::new(), Agent::Cursor, true));
    assert!(!run.artifact("shutdown").exists());
  }

  #[test]
  fn accounting_wait_defaults_to_ten_minutes_unless_overridden() {
    assert_eq!(accounting_timeout_from_env(None), 600);
    assert_eq!(accounting_timeout_from_env(Some("12")), 12);
    for invalid in ["0", "-1", "bad", ""] {
      assert_eq!(accounting_timeout_from_env(Some(invalid)), 600);
    }
  }

  #[test]
  fn real_cursor_hook_order_releases_complete_counters() {
    let run = Run::new();
    std::fs::write(run.artifact("cursor-hooks.jsonl"), r#"{"hook_event_name":"stop","conversation_id":"c","generation_id":"g","status":"completed","input_tokens":294213,"output_tokens":8032,"cache_read_tokens":171008,"cache_write_tokens":0}
{"hook_event_name":"afterAgentResponse","conversation_id":"c","generation_id":"g"}
{"hook_event_name":"sessionEnd","conversation_id":"c","status":"completed"}
"#).unwrap();
    let mut completion = Completion::new();
    assert!(run.poll(&mut completion, Agent::Cursor, true));
    assert!(run.artifact("shutdown").exists());
    let saved = Summary::from_json(&std::fs::read_to_string(run.artifact("usage-final")).unwrap()).unwrap();
    let tokens = saved.tokens.unwrap();
    assert_eq!(tokens.input, 123_205);
    assert_eq!(tokens.output, 8_032);
    assert_eq!(tokens.cache_read, 171_008);
  }

  #[test]
  fn cursor_child_tools_allow_parent_accounting_to_release_shutdown() {
    let run = Run::new();
    std::fs::write(run.artifact("cursor-hooks.jsonl"), r#"{"hook_event_name":"postToolUse","conversation_id":"child","generation_id":"child","transcript_path":null,"tool_use_id":"t1"}
{"hook_event_name":"afterAgentResponse","conversation_id":"parent","generation_id":"g","input_tokens":853434,"output_tokens":12287,"cache_read_tokens":640384,"cache_write_tokens":0}
{"hook_event_name":"stop","conversation_id":"parent","generation_id":"g","status":"completed","input_tokens":853434,"output_tokens":12287,"cache_read_tokens":640384,"cache_write_tokens":0}
"#).unwrap();
    let mut completion = Completion::new();
    assert!(run.poll(&mut completion, Agent::Cursor, true));
    assert!(run.artifact("shutdown").exists());
    assert!(!run.artifact("usage-error").exists());
    let saved = Summary::from_json(&std::fs::read_to_string(run.artifact("usage-final")).unwrap()).unwrap();
    assert!(saved.complete);
    assert_eq!(saved.tokens.unwrap().input, 213_050);
  }

  #[test]
  fn a_live_cursor_turn_after_the_result_does_not_start_the_deadline() {
    let run = Run::new();
    std::fs::write(
      run.artifact("cursor-hooks.jsonl"),
      r#"{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g","tool_use_id":"t1"}
"#,
    )
    .unwrap();
    let mut state = Completion::new();
    assert!(run.poll_live(&mut state, Agent::Cursor, true, true));
    assert!(!run.artifact("shutdown").exists());
    assert!(state.started.is_none(), "screen activity means the generation is still running");
    assert_eq!(state.recovery(), crate::recovery::Recovery::Finishing);
    state.started = Some(Instant::now() - Duration::from_secs(accounting_timeout_secs() + 1));
    assert!(run.poll_live(&mut state, Agent::Cursor, true, true));
    assert!(!run.artifact("usage-error").exists(), "a live turn must not expire the quiet-turn budget");
    assert!(!run.artifact("shutdown").exists());
    assert!(state.started.is_none());
    assert_eq!(state.recovery(), crate::recovery::Recovery::Finishing);
  }

  #[test]
  fn completion_reports_the_accounting_and_exit_budgets_it_enforces() {
    use crate::recovery::Recovery;
    let mut state = Completion::new();
    state.started = Some(Instant::now());
    let now = crate::daemon::now_unix_secs();
    assert!(matches!(state.recovery(), Recovery::Accounting(at) if at >= now + accounting_timeout_secs() - 1));
    state.released = Some(Instant::now());
    assert!(matches!(state.recovery(), Recovery::Exit(at) if (now + 14..=now + 16).contains(&at)));
  }

  #[test]
  fn growing_cursor_hooks_after_the_result_hold_the_deadline() {
    let run = Run::new();
    let hooks = run.artifact("cursor-hooks.jsonl");
    std::fs::write(
      &hooks,
      r#"{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g","tool_use_id":"t1"}
"#,
    )
    .unwrap();
    let mut state = Completion::new();
    assert!(run.poll(&mut state, Agent::Cursor, true));
    state.started = Some(Instant::now() - Duration::from_secs(accounting_timeout_secs() + 1));
    std::fs::write(
      &hooks,
      r#"{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g","tool_use_id":"t1"}
{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g","tool_use_id":"t2"}
"#,
    )
    .unwrap();
    assert!(run.poll(&mut state, Agent::Cursor, true));
    assert!(!run.artifact("shutdown").exists(), "another tool call is still the same open turn");
    std::fs::write(
      &hooks,
      r#"{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g","tool_use_id":"t1"}
{"hook_event_name":"postToolUse","conversation_id":"c","generation_id":"g","tool_use_id":"t2"}
{"hook_event_name":"stop","conversation_id":"c","generation_id":"g","status":"completed","input_tokens":10,"output_tokens":2,"cache_read_tokens":3,"cache_write_tokens":0}
"#,
    )
    .unwrap();
    assert!(run.poll(&mut state, Agent::Cursor, true));
    assert!(run.artifact("shutdown").exists());
    assert!(!run.artifact("usage-error").exists());
    let saved = Summary::from_json(&std::fs::read_to_string(run.artifact("usage-final")).unwrap()).unwrap();
    assert!(saved.complete);
    assert_eq!(saved.tokens.unwrap().output, 2);
  }
}
