//! Structured failure reasons and logging — every failed proc/skill/run gets a reason code
//! on stderr and, as one JSON object per line, in `$TMPDIR/scsh-daemon/failures.log`
//! (append-only, best-effort). `scsh failures` reads that file back.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::json::{parse, quote, Value};

/// Machine-readable reason codes (stable strings for scripts and the session browser).
pub mod reason {
  pub const BUILD_FAILED: &str = "build_failed";
  pub const ENV_UNRESOLVED: &str = "env_unresolved";
  pub const RUN_DIR: &str = "run_dir_prepare_failed";
  pub const GIT_TRANSPORT: &str = "git_transport_prepare_failed";
  pub const GIT_DAEMON: &str = "git_daemon_start_failed";
  pub const CLONE: &str = "clone_failed";
  pub const CONTAINER_TIMEOUT: &str = "container_timeout";
  pub const HARNESS_NONZERO: &str = "harness_nonzero_exit";
  pub const CONTAINER_RUN: &str = "container_run_failed";
  pub const RESULT_MISSING: &str = "result_file_missing";
  pub const THREAD_PANICKED: &str = "skill_thread_panicked";
  pub const SESSION_END_INCOMPLETE: &str = "session_end_before_proc_finish";
  pub const DAEMON_DRAIN_TIMEOUT: &str = "daemon_poster_drain_timeout";
  pub const DAEMON_POST_FAILED: &str = "daemon_post_failed";
}

const LOG_NAME: &str = "failures.log";

/// Reasons worth one automatic retry: infrastructure hiccups that a fresh clone +
/// container plausibly fix. Deterministic failures (bad env, missing result file,
/// harness exiting non-zero) are never retried. Opt out with `SCSH_NO_RETRY=1`.
pub fn is_transient(reason: &str) -> bool {
  matches!(reason, reason::CONTAINER_TIMEOUT | reason::CONTAINER_RUN | reason::CLONE | reason::GIT_DAEMON)
}

pub fn retry_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_RETRY").ok().as_deref(), Some("1") | Some("true"))
}

/// The append-only JSONL failure log shared by the CLI and the daemon.
pub fn log_path() -> PathBuf {
  crate::daemon::daemon_dir().join(LOG_NAME)
}

/// One recorded failure (or run rollup): a line of `failures.log`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FailureEvent {
  pub ts: u64,
  /// `proc` | `skill` | `session` | `retry` | `run_summary`.
  pub kind: String,
  pub reason: String,
  pub session: Option<String>,
  pub skill: Option<String>,
  /// Proc label ("opencode: add") or other human subject when not a plain skill event.
  pub subject: Option<String>,
  pub harness: Option<String>,
  pub model: Option<String>,
  pub profile: Option<String>,
  pub failed: Option<u64>,
  pub total: Option<u64>,
  pub detail: Option<String>,
}

/// Detail shown in the terminal summary and session browser (reason prefix + body).
pub fn format_detail(reason: &str, body: &str) -> String {
  if body.is_empty() {
    format!("[{reason}]")
  } else {
    format!("[{reason}] {body}")
  }
}

/// A proc row (build or skill) finished red on the live board.
pub fn log_proc(reason: &str, label: &str, detail: Option<&str>) {
  let ev = FailureEvent {
    kind: "proc".into(),
    reason: reason.into(),
    subject: Some(label.into()),
    detail: detail.map(str::to_string),
    ..Default::default()
  };
  write_event(&ev, &format!("proc failed: reason={reason} subject={label}"), detail);
}

/// A skill orchestration step failed before or after the live board row.
pub fn log_skill(reason: &str, skill: &str, detail: &str) {
  let ev = FailureEvent {
    kind: "skill".into(),
    reason: reason.into(),
    skill: Some(skill.into()),
    detail: Some(detail.into()),
    ..Default::default()
  };
  write_event(&ev, &format!("skill failed: reason={reason} skill={skill}"), Some(detail));
}

/// A transient failure is being retried once (recorded so stats can see flaky routes).
pub fn log_retry(session: &str, skill: &str, harness: &str, model: Option<&str>, reason: &str) {
  let ev = FailureEvent {
    kind: "retry".into(),
    reason: reason.into(),
    session: Some(session.into()),
    skill: Some(skill.into()),
    harness: Some(harness.into()),
    model: model.map(str::to_string),
    ..Default::default()
  };
  write_event(&ev, &format!("retrying: reason={reason} session={session} skill={skill} (transient, one retry)"), None);
}

/// The session browser daemon inferred a failure (e.g. deregister while proc still running).
pub fn log_session_proc(session: &str, reason: &str, proc_label: &str, detail: &str) {
  let ev = FailureEvent {
    kind: "session".into(),
    reason: reason.into(),
    session: Some(session.into()),
    subject: Some(proc_label.into()),
    detail: Some(detail.into()),
    ..Default::default()
  };
  write_event(&ev, &format!("session failed: reason={reason} session={session} proc={proc_label}"), Some(detail));
}

/// End-of-run rollup when one or more skills failed.
pub fn log_run_summary(session: &str, profile: Option<&str>, failed: usize, total: usize) {
  let ev = FailureEvent {
    kind: "run_summary".into(),
    reason: "run_failed".into(),
    session: Some(session.into()),
    profile: profile.map(str::to_string),
    failed: Some(failed as u64),
    total: Some(total as u64),
    ..Default::default()
  };
  let profile = profile.unwrap_or("(no profile)");
  write_event(&ev, &format!("run summary: session={session} profile={profile} failed={failed}/{total}"), None);
}

/// One failed skill after the run joins (includes reason code, route, and pointers when known).
pub fn log_failed_skill(session: &str, skill: &str, harness: &str, model: Option<&str>, reason: &str, detail: &str) {
  let ev = FailureEvent {
    kind: "skill".into(),
    reason: reason.into(),
    session: Some(session.into()),
    skill: Some(skill.into()),
    harness: Some(harness.into()),
    model: model.map(str::to_string),
    detail: Some(detail.into()),
    ..Default::default()
  };
  write_event(&ev, &format!("skill failed: reason={reason} session={session} skill={skill}"), Some(detail));
}

/// Print the human line(s) on stderr and append one JSON line to `failures.log`.
fn write_event(ev: &FailureEvent, headline: &str, detail: Option<&str>) {
  eprintln!("scsh: {headline}");
  if let Some(d) = detail.filter(|d| d.contains('\n')) {
    for line in d.lines() {
      eprintln!("scsh:   {line}");
    }
  }
  append_log_file(&event_json(ev));
}

fn append_log_file(line: &str) {
  let path = log_path();
  if std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).is_err() {
    return;
  }
  if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
    let _ = writeln!(f, "{line}");
  }
}

fn event_json(ev: &FailureEvent) -> String {
  let ts = if ev.ts != 0 { ev.ts } else { crate::daemon::now_unix_secs() };
  let mut fields = vec![
    format!("\"ts\": {ts}"),
    format!("\"kind\": {}", quote(&ev.kind)),
    format!("\"reason\": {}", quote(&ev.reason)),
  ];
  let opts: [(&str, &Option<String>); 7] = [
    ("session", &ev.session),
    ("skill", &ev.skill),
    ("subject", &ev.subject),
    ("harness", &ev.harness),
    ("model", &ev.model),
    ("profile", &ev.profile),
    ("detail", &ev.detail),
  ];
  for (key, v) in opts {
    if let Some(s) = v {
      fields.push(format!("{}: {}", quote(key), quote(s)));
    }
  }
  if let Some(n) = ev.failed {
    fields.push(format!("\"failed\": {n}"));
  }
  if let Some(n) = ev.total {
    fields.push(format!("\"total\": {n}"));
  }
  format!("{{ {} }}", fields.join(", "))
}

/// Parse every JSONL line of `failures.log`, skipping blank or unparseable (legacy) lines.
pub fn read_events() -> Vec<FailureEvent> {
  read_events_from(&log_path())
}

pub fn read_events_from(path: &Path) -> Vec<FailureEvent> {
  let Ok(text) = std::fs::read_to_string(path) else {
    return Vec::new();
  };
  text.lines().filter_map(parse_event).collect()
}

fn parse_event(line: &str) -> Option<FailureEvent> {
  let obj = match parse(line.trim()).ok()? {
    Value::Object(o) => o,
    _ => return None,
  };
  let get = |key: &str| {
    obj.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
      Value::String(s) => Some(s.clone()),
      _ => None,
    })
  };
  let num = |key: &str| {
    obj.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
      Value::Number(n) => Some(*n as u64),
      _ => None,
    })
  };
  Some(FailureEvent {
    ts: num("ts")?,
    kind: get("kind")?,
    reason: get("reason")?,
    session: get("session"),
    skill: get("skill"),
    subject: get("subject"),
    harness: get("harness"),
    model: get("model"),
    profile: get("profile"),
    failed: num("failed"),
    total: num("total"),
    detail: get("detail"),
  })
}

/// Extra context when a skill's declared result file is missing from the run clone.
pub fn missing_result_context(run_dir: &Path, result_rel: &str) -> String {
  let produced = run_dir.join(result_rel);
  let parent = produced.parent();
  let mut out = String::new();
  if let Some(dir) = parent.filter(|d| d.is_dir()) {
    let _ = write!(out, "; looked in {}", dir.display());
    if let Ok(read) = std::fs::read_dir(dir) {
      let names: Vec<_> = read.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
      if names.is_empty() {
        out.push_str(" (directory empty)");
      } else if names.len() <= 8 {
        out.push_str(&format!(" (contains: {})", names.join(", ")));
      } else {
        out.push_str(&format!(" (contains {} entries, e.g. {})", names.len(), names[..4].join(", ")));
      }
    }
  } else {
    let _ = write!(
      out,
      "; parent directory {} does not exist",
      produced.parent().map(|p| p.display().to_string()).unwrap_or_default()
    );
  }
  let log = run_dir.join(crate::runtime::RUN_LOG_REL);
  if log.is_file() {
    let _ = write!(out, "; harness log at {}", log.display());
  }
  out
}

pub const FAILURE_TAIL_LINES: usize = 12;
const FAILURE_EXCERPT_MAX_CHARS: usize = 1200;
pub fn failure_excerpt(last: Option<&str>, tail: &[String], fallback: &str) -> String {
  error_excerpt(last, tail).unwrap_or_else(|| fallback.to_string())
}

fn error_excerpt(last: Option<&str>, tail: &[String]) -> Option<String> {
  for line in tail.iter().rev() {
    if looks_like_error(line) {
      return Some(truncate_excerpt(line));
    }
  }
  if let Some(l) = last.filter(|s| !s.trim().is_empty()) {
    return Some(truncate_excerpt(l));
  }
  if !tail.is_empty() {
    let start = tail.len().saturating_sub(FAILURE_TAIL_LINES);
    let joined = tail[start..].join("\n");
    if !joined.trim().is_empty() {
      return Some(truncate_excerpt(&joined));
    }
  }
  None
}

fn looks_like_error(line: &str) -> bool {
  let lower = line.to_lowercase();
  lower.contains("error")
    || lower.contains("failed")
    || lower.contains("fatal")
    || lower.contains("cannot ")
    || lower.contains("can't ")
    || lower.contains("denied")
    || lower.contains("not found")
    || lower.contains("no such file")
    || lower.contains("exit code")
    || lower.contains("returned a non-zero")
    || lower.contains("buildkit")
    || lower.contains("executor failed")
}

fn truncate_excerpt(s: &str) -> String {
  if s.chars().count() <= FAILURE_EXCERPT_MAX_CHARS {
    return s.to_string();
  }
  format!("{}…", s.chars().take(FAILURE_EXCERPT_MAX_CHARS).collect::<String>())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn format_detail_prefixes_reason() {
    assert_eq!(format_detail(reason::CLONE, "network error"), "[clone_failed] network error");
    assert_eq!(format_detail(reason::THREAD_PANICKED, ""), "[skill_thread_panicked]");
  }

  #[test]
  fn failure_excerpt_prefers_error_line_in_tail() {
    let tail =
      vec!["STEP 1/3 : FROM debian".into(), "ERROR: failed to solve: process did not complete successfully".into()];
    let excerpt = failure_excerpt(None, &tail, "no output");
    assert!(excerpt.contains("failed to solve"));
  }

  #[test]
  fn failure_excerpt_joins_tail_when_no_error_keyword() {
    let tail = vec!["line one".into(), "line two".into()];
    assert_eq!(failure_excerpt(None, &tail, "fallback"), "line one\nline two");
  }

  #[test]
  fn event_json_roundtrips_through_parse_event() {
    let ev = FailureEvent {
      ts: 1234,
      kind: "skill".into(),
      reason: reason::CONTAINER_TIMEOUT.into(),
      session: Some("abc123".into()),
      skill: Some("add".into()),
      harness: Some("opencode".into()),
      model: Some("openai/gpt-5.5".into()),
      detail: Some("timed out after 60s\nrun dir: /tmp/x".into()),
      ..Default::default()
    };
    let back = parse_event(&event_json(&ev)).unwrap();
    assert_eq!(back, ev);
  }

  #[test]
  fn parse_event_skips_legacy_plain_text_lines() {
    assert!(parse_event("1234 skill failed: reason=clone_failed subject=add").is_none());
    assert!(parse_event("").is_none());
  }

  #[test]
  fn transient_reasons_are_retryable_and_deterministic_ones_are_not() {
    assert!(is_transient(reason::CONTAINER_TIMEOUT));
    assert!(is_transient(reason::CONTAINER_RUN));
    assert!(is_transient(reason::CLONE));
    assert!(is_transient(reason::GIT_DAEMON));
    assert!(!is_transient(reason::ENV_UNRESOLVED));
    assert!(!is_transient(reason::RESULT_MISSING));
    assert!(!is_transient(reason::HARNESS_NONZERO));
    assert!(!is_transient(reason::BUILD_FAILED));
  }
}
