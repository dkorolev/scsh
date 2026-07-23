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
  /// A `scsh quota` run could not produce a quota answer for its harness (no login,
  /// lapsed token, throttled or failing provider endpoint). Deterministic from the
  /// run's point of view — never auto-retried; the fix is a human signing in or waiting.
  pub const QUOTA_UNAVAILABLE: &str = "quota_unavailable";
  pub const ENV_UNRESOLVED: &str = "env_unresolved";
  pub const RUN_DIR: &str = "run_dir_prepare_failed";
  pub const GIT_TRANSPORT: &str = "git_transport_prepare_failed";
  pub const GIT_DAEMON: &str = "git_daemon_start_failed";
  pub const CLONE: &str = "clone_failed";
  pub const CONTAINER_TIMEOUT: &str = "container_timeout";
  pub const CONTAINER_INACTIVE: &str = "container_inactive";
  /// The run stalled during its launch phase — no terminal output at all in the first
  /// seconds, or output that stopped dead while the startup window was still open. Nothing
  /// of value was in flight, so the route is force-restarted immediately (no backoff),
  /// still under the retry count and identical-failure breaker.
  pub const STARTUP_STALLED: &str = "harness_startup_stalled";
  pub const HARNESS_NONZERO: &str = "harness_nonzero_exit";
  pub const HARNESS_OVERLOADED: &str = "harness_overloaded";
  /// The harness lost its backend connection (its output says so) and exited non-zero —
  /// e.g. cursor's "Reconnecting to …" giving up after half an hour. Always retryable.
  pub const HARNESS_DISCONNECTED: &str = "harness_disconnected";
  /// The provider definitively rejected the credentials on the FIRST attempt. Permanent:
  /// no overnight budget can log the user back in.
  pub const HARNESS_AUTH_REJECTED: &str = "harness_auth_rejected";
  /// The route kept failing with the SAME failure signature until the circuit breaker
  /// tripped — retrying further would burn tokens on a deterministic failure.
  pub const RETRIES_EXHAUSTED_IDENTICAL: &str = "retries_exhausted_identical";
  pub const CONTAINER_RUN: &str = "container_run_failed";
  pub const RESULT_MISSING: &str = "result_file_missing";
  pub const RESULT_INVALID: &str = "result_schema_invalid";
  pub const THREAD_PANICKED: &str = "skill_thread_panicked";
  /// A browser stop request was accepted and container teardown is still in progress.
  pub const STOP_REQUESTED: &str = "stop_requested";
  /// A browser restart request was accepted and container teardown is still in progress;
  /// the owning `scsh run` respawns the route as a fresh attempt once this one settles.
  pub const RESTART_REQUESTED: &str = "restart_requested";
  pub const SESSION_END_INCOMPLETE: &str = "session_end_before_proc_finish";
  pub const ANNOTATION_TIMED_OUT: &str = "annotation_timed_out";
  /// The annotation PROCESS vanished without reporting completion — killed by a terminal
  /// or harness teardown, a crash, a reboot — as opposed to running past its model
  /// watchdog ([`ANNOTATION_TIMED_OUT`]). The recording is unchanged either way.
  pub const ANNOTATION_INTERRUPTED: &str = "annotation_interrupted";
  pub const FORCE_STOPPED: &str = "force_stopped";
  /// Settled counterpart of [`RESTART_REQUESTED`]: this attempt was stopped from the
  /// session browser to make room for the fresh attempt that supersedes it.
  pub const FORCE_RESTARTED: &str = "force_restarted";
  pub const DAEMON_DRAIN_TIMEOUT: &str = "daemon_poster_drain_timeout";
  pub const DAEMON_POST_FAILED: &str = "daemon_post_failed";
}

const LOG_NAME: &str = "failures.log";

/// Reasons worth automatic retries: failures that a fresh clone + container plausibly
/// fix. A non-zero harness exit counts — a harness that crashes seconds after startup
/// (no output, no result) is indistinguishable from any other infrastructure hiccup, and
/// a genuine agent failure just trips the identical-signature breaker. Deterministic
/// preflight failures (bad env, missing result file, failed build) are never retried.
/// Opt out with `SCSH_NO_RETRY=1`.
pub fn is_transient(reason: &str) -> bool {
  matches!(
    reason,
    reason::CONTAINER_TIMEOUT
      | reason::CONTAINER_INACTIVE
      | reason::STARTUP_STALLED
      | reason::CONTAINER_RUN
      | reason::HARNESS_OVERLOADED
      | reason::HARNESS_DISCONNECTED
      | reason::HARNESS_NONZERO
      | reason::CLONE
      | reason::GIT_DAEMON
  )
}

/// The retry verdict for one failed attempt: whether the retry machinery may spend
/// budget on another attempt at all. `Retryable` is still subject to the
/// [`RetryPolicy`]'s wall-clock budget and identical-signature breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
  /// Never retried, in either mode: deterministic preflight/config failures, definitive
  /// first-attempt credential rejections, and human stop/restart controls.
  Permanent,
  /// Worth another attempt within the policy's budget and signature cap.
  Retryable,
}

/// Classify a failed attempt. In-run retries stay conservative — the needle-list of
/// retryable reasons plus a TUI result-miss — because persistence beyond one run is the
/// job supervisor's business: a job that fails terminally is restarted (resuming its
/// completed steps) up to its retries budget, so an over-eager in-run verdict would just
/// double-spend. `first_attempt` scopes the auth verdict: credentials rejected out of
/// the gate are permanent (no retry budget can log the user back in), but an auth error
/// appearing only on a LATER attempt (the first one got further) is provider flakiness
/// and stays retryable.
pub fn verdict(reason: &str, tui: bool, first_attempt: bool) -> Verdict {
  if reason == reason::HARNESS_AUTH_REJECTED {
    return if first_attempt { Verdict::Permanent } else { Verdict::Retryable };
  }
  if is_transient(reason) || (reason == reason::RESULT_MISSING && tui) {
    return Verdict::Retryable;
  }
  Verdict::Permanent
}

/// Automatic-retry policy for one task: count- and wall-clock-budgeted, exponentially
/// backed off with jitter, and circuit-broken on identical consecutive failures. The
/// count is the primary user-facing ceiling; elapsed time prevents several slow attempts
/// from holding a run indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
  /// Automatic retries allowed for one task before the run escalates to the job supervisor.
  pub max_retries: u32,
  /// Wall clock allowed for retries of one route, measured from its FIRST failure.
  pub budget_secs: u64,
  /// First backoff delay; doubles per retry up to [`RetryPolicy::backoff_cap_secs`].
  pub backoff_initial_secs: u64,
  pub backoff_cap_secs: u64,
  /// Consecutive identical failure signatures before the breaker trips — what stops
  /// "the repo doesn't compile" from burning tokens until dawn.
  pub signature_cap: u32,
}

/// Default in-run retry policy: most provider blips resolve within five retries and half
/// an hour, and anything longer is the job supervisor's business — it restarts the whole job
/// (resuming completed steps) on its own backoff, so the two layers together ride out a
/// multi-hour incident without one run holding containers all night.
pub const DEFAULT_RETRY_BUDGET_SECS: u64 = 30 * 60;
pub const DEFAULT_TASK_RETRIES: u32 = 5;
pub const DEFAULT_RETRY_SIGNATURE_CAP: u32 = 5;
pub const RETRY_BACKOFF_INITIAL_SECS: u64 = 60;
pub const RETRY_BACKOFF_CAP_SECS: u64 = 15 * 60;

impl RetryPolicy {
  /// The policy for one route: explicit config (route > skill > def step) beats the
  /// `SCSH_RETRY_FOR` / `SCSH_RETRY_SIGNATURE_CAP` environment, which beats the
  /// defaults (30m budget, breaker at 5).
  pub fn resolve(retry_for: Option<u64>, signature_cap: Option<u32>) -> RetryPolicy {
    let env_budget = std::env::var("SCSH_RETRY_FOR").ok().and_then(|s| parse_duration_secs(&s));
    let env_cap = std::env::var("SCSH_RETRY_SIGNATURE_CAP").ok().and_then(|s| s.trim().parse().ok());
    RetryPolicy {
      max_retries: DEFAULT_TASK_RETRIES,
      budget_secs: retry_for.or(env_budget).unwrap_or(DEFAULT_RETRY_BUDGET_SECS),
      backoff_initial_secs: RETRY_BACKOFF_INITIAL_SECS,
      backoff_cap_secs: RETRY_BACKOFF_CAP_SECS,
      signature_cap: signature_cap.or(env_cap).unwrap_or(DEFAULT_RETRY_SIGNATURE_CAP),
    }
  }

  /// Delay before retry number `retries_used + 1`: exponential with a cap, plus ±20%
  /// deterministic jitter keyed on `salt` so a fleet failing together fans back out
  /// instead of retrying as a thundering herd. No RNG dependency; same inputs, same
  /// delay — testable.
  pub fn backoff_delay_secs(&self, retries_used: u32, salt: u64) -> u64 {
    let doubled = self.backoff_initial_secs.saturating_mul(2u64.saturating_pow(retries_used.min(32)));
    let base = doubled.min(self.backoff_cap_secs).max(1);
    let span = base / 5;
    if span == 0 {
      return base;
    }
    let hashed = salt.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(31).wrapping_add(retries_used as u64);
    base - span + (hashed % (2 * span + 1))
  }
}

/// Parse a human duration: `90s`, `45m`, `8h`, `2d`, or bare seconds (`900`).
pub fn parse_duration_secs(s: &str) -> Option<u64> {
  let t = s.trim();
  if t.is_empty() {
    return None;
  }
  let (digits, unit) = match t.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
    Some((i, _)) => t.split_at(i),
    None => (t, ""),
  };
  let n: u64 = digits.parse().ok()?;
  match unit.trim() {
    "" | "s" | "sec" | "secs" => Some(n),
    "m" | "min" | "mins" => n.checked_mul(60),
    "h" | "hr" | "hrs" => n.checked_mul(3600),
    "d" => n.checked_mul(86400),
    _ => None,
  }
}

/// A stable fingerprint of one failed attempt, for the identical-failure breaker:
/// the reason code plus the first detail line (the human "why", before the run-dir and
/// log pointers, which change every attempt), lowercased, digits erased, whitespace
/// collapsed, truncated. Two attempts failing the same way — same compiler error, same
/// missing file — collide; a different failure resets the breaker.
pub fn failure_signature(reason: &str, detail: Option<&str>) -> String {
  let first_line = detail.unwrap_or("").lines().next().unwrap_or("");
  let mut normalized = String::with_capacity(first_line.len().min(200) + reason.len() + 1);
  normalized.push_str(reason);
  normalized.push('|');
  let mut last_space = false;
  for c in first_line.chars() {
    if normalized.len() >= 200 {
      break;
    }
    let mapped = if c.is_ascii_digit() {
      '#'
    } else if c.is_whitespace() {
      ' '
    } else {
      c.to_ascii_lowercase()
    };
    if mapped == ' ' {
      if last_space {
        continue;
      }
      last_space = true;
    } else {
      last_space = false;
    }
    normalized.push(mapped);
  }
  normalized
}

/// Provider-capacity messages are transient even when the harness exits cleanly enough to
/// return a non-zero status instead of waiting for the watchdog. Keep this deliberately narrow:
/// ordinary command/model failures remain deterministic and do not earn a retry.
pub fn harness_reported_overload(text: &str) -> bool {
  let lower = text.to_ascii_lowercase();
  [
    "overloaded",
    "try again later",
    "rate limit",
    "rate_limit",
    "too many requests",
    "temporarily unavailable",
    "status 529",
    "error 529",
    "status 503",
    "503 service unavailable",
    "usage limit will reset",
  ]
  .iter()
  .any(|needle| lower.contains(needle))
}

/// Connectivity-loss messages: a harness that spent its last screen saying "reconnecting"
/// and then exited non-zero did not fail the task — it lost its provider. The 2026-07-16
/// overnight failure was literally "Reconnecting to agentn.global.api5.cursor.sh
/// (attempt 2)" for half an hour before a non-zero exit that earned zero retries.
pub fn harness_reported_disconnect(text: &str) -> bool {
  let lower = text.to_ascii_lowercase();
  [
    "reconnecting",
    "connection lost",
    "connection reset",
    "connection refused",
    "econnreset",
    "etimedout",
    "network error",
    "stream disconnected",
    "disconnected from",
    "socket hang up",
    "fetch failed",
    "dns error",
    "dns resolution",
    "tls handshake",
    "502 bad gateway",
    "504 gateway",
    "internal server error",
    "overloaded_error",
  ]
  .iter()
  .any(|needle| lower.contains(needle))
}

/// Definitive credential rejections — the strict needle set that makes a first-attempt
/// failure [`Verdict::Permanent`]: no retry budget can log the user back in, so refuse
/// loudly instead of burning the night.
pub fn harness_reported_auth_rejection(text: &str) -> bool {
  let lower = text.to_ascii_lowercase();
  [
    "invalid api key",
    "authentication failed",
    "401 unauthorized",
    "unauthorized: ",
    "token revoked",
    "token expired",
    "please run /login",
    "oauth token has expired",
  ]
  .iter()
  .any(|needle| lower.contains(needle))
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

/// A failed attempt is being retried (recorded so stats can distinguish recovered routes).
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
  write_event(&ev, &format!("retrying: reason={reason} session={session} skill={skill}"), None);
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
    assert!(is_transient(reason::CONTAINER_INACTIVE));
    assert!(is_transient(reason::STARTUP_STALLED));
    assert!(is_transient(reason::CONTAINER_RUN));
    assert!(is_transient(reason::HARNESS_OVERLOADED));
    assert!(is_transient(reason::HARNESS_DISCONNECTED));
    assert!(is_transient(reason::HARNESS_NONZERO));
    assert!(is_transient(reason::CLONE));
    assert!(is_transient(reason::GIT_DAEMON));
    assert!(!is_transient(reason::ENV_UNRESOLVED));
    assert!(!is_transient(reason::RESULT_MISSING));
    assert!(!is_transient(reason::BUILD_FAILED));
    assert!(harness_reported_overload("API Error: service overloaded; try again later"));
    assert!(harness_reported_overload("HTTP 429: Too Many Requests"));
    assert!(harness_reported_overload("status 529"));
    assert!(harness_reported_overload("Your usage limit will reset at 3am"));
    assert!(!harness_reported_overload("tool exited with status 1"));
  }

  #[test]
  fn disconnect_sniffer_matches_the_overnight_cursor_line_and_not_test_failures() {
    // The literal line from qtsiuf's fix step (2026-07-16): rendered TUI tail.
    assert!(harness_reported_disconnect("Reconnecting to agentn.global.api5.cursor.sh (attempt 2)"));
    assert!(harness_reported_disconnect("fetch failed: socket hang up"));
    assert!(harness_reported_disconnect("upstream returned 502 Bad Gateway"));
    assert!(harness_reported_disconnect("read tcp 10.0.0.2:443: connection reset by peer"));
    // A failing go test suite must never read as transient.
    assert!(!harness_reported_disconnect("--- FAIL: TestGateway (0.03s)\nexit status 1"));
    assert!(!harness_reported_disconnect("assertion failed: expected 5, got 4"));
  }

  #[test]
  fn auth_sniffer_matches_rejections_only() {
    assert!(harness_reported_auth_rejection("API Error: 401 Unauthorized · Please run /login"));
    assert!(harness_reported_auth_rejection("OAuth token has expired"));
    assert!(harness_reported_auth_rejection("Invalid API key provided"));
    assert!(!harness_reported_auth_rejection("warning: nearing usage limit"));
    assert!(!harness_reported_auth_rejection("committed as author t@example.com"));
  }

  #[test]
  fn verdict_retries_transients_and_fails_fast_on_the_rest() {
    use Verdict::{Permanent, Retryable};
    // Retryable: the transient set + TUI result-miss.
    assert_eq!(verdict(reason::HARNESS_NONZERO, true, true), Retryable);
    assert_eq!(verdict(reason::HARNESS_DISCONNECTED, false, true), Retryable);
    assert_eq!(verdict(reason::RESULT_MISSING, true, true), Retryable);
    // Everything else is permanent IN-RUN — the job supervisor's restarts (with resume)
    // are the persistence layer for these, not another container in the same run.
    assert_eq!(verdict(reason::RESULT_MISSING, false, true), Permanent);
    assert_eq!(verdict(reason::RESULT_INVALID, true, true), Permanent);
    assert_eq!(verdict(reason::THREAD_PANICKED, true, true), Permanent);
    assert_eq!(verdict(reason::ENV_UNRESOLVED, true, true), Permanent);
    assert_eq!(verdict(reason::BUILD_FAILED, true, true), Permanent);
    assert_eq!(verdict(reason::RETRIES_EXHAUSTED_IDENTICAL, true, true), Permanent);
    assert_eq!(verdict(reason::FORCE_STOPPED, true, true), Permanent);
    // Auth: first-attempt rejection is permanent; a LATER-attempt one is flakiness.
    assert_eq!(verdict(reason::HARNESS_AUTH_REJECTED, true, true), Permanent);
    assert_eq!(verdict(reason::HARNESS_AUTH_REJECTED, true, false), Retryable);
  }

  #[test]
  fn retry_policy_resolves_config_over_env_over_default() {
    let _lock = crate::runtime::test_env_lock();
    std::env::remove_var("SCSH_RETRY_FOR");
    std::env::remove_var("SCSH_RETRY_SIGNATURE_CAP");
    let p = RetryPolicy::resolve(None, None);
    assert_eq!(p.max_retries, DEFAULT_TASK_RETRIES);
    assert_eq!(p.budget_secs, DEFAULT_RETRY_BUDGET_SECS);
    assert_eq!(p.signature_cap, DEFAULT_RETRY_SIGNATURE_CAP);
    std::env::set_var("SCSH_RETRY_FOR", "45m");
    std::env::set_var("SCSH_RETRY_SIGNATURE_CAP", "2");
    let p = RetryPolicy::resolve(None, None);
    assert_eq!(p.budget_secs, 45 * 60, "env beats the mode default");
    assert_eq!(p.signature_cap, 2);
    let p = RetryPolicy::resolve(Some(90), Some(7));
    assert_eq!(p.budget_secs, 90, "explicit config beats the env");
    assert_eq!(p.signature_cap, 7);
    std::env::remove_var("SCSH_RETRY_FOR");
    std::env::remove_var("SCSH_RETRY_SIGNATURE_CAP");
  }

  #[test]
  fn backoff_doubles_with_bounded_jitter_and_caps() {
    let p = RetryPolicy::resolve(Some(8 * 3600), None);
    for (retries, base) in [(0u32, 60u64), (1, 120), (2, 240), (3, 480), (4, 900), (10, 900)] {
      for salt in [0u64, 1, 42, u64::MAX] {
        let d = p.backoff_delay_secs(retries, salt);
        let span = base / 5;
        assert!(d >= base - span && d <= base + span, "retry {retries} salt {salt}: {d}s outside {base}±{span}");
      }
    }
    // Deterministic: same inputs, same delay.
    assert_eq!(p.backoff_delay_secs(2, 7), p.backoff_delay_secs(2, 7));
    // Different salts spread the herd (at least sometimes).
    assert_ne!(p.backoff_delay_secs(3, 1), p.backoff_delay_secs(3, 2));
  }

  #[test]
  fn durations_parse_like_humans_write_them() {
    assert_eq!(parse_duration_secs("90s"), Some(90));
    assert_eq!(parse_duration_secs("45m"), Some(45 * 60));
    assert_eq!(parse_duration_secs("8h"), Some(8 * 3600));
    assert_eq!(parse_duration_secs("2d"), Some(2 * 86400));
    assert_eq!(parse_duration_secs("900"), Some(900), "bare numbers are seconds");
    assert_eq!(parse_duration_secs(" 15 m "), Some(15 * 60), "outer and pre-unit spaces are fine");
    assert_eq!(parse_duration_secs("m15"), None, "the number leads");
    assert_eq!(parse_duration_secs("h"), None);
    assert_eq!(parse_duration_secs("8w"), None, "unknown unit");
    assert_eq!(parse_duration_secs(""), None);
  }

  #[test]
  fn failure_signatures_collide_for_same_failures_and_differ_otherwise() {
    // Same compiler error across attempts — different run dirs ride in LATER detail
    // lines, so they never enter the signature.
    let a = failure_signature(
      reason::HARNESS_NONZERO,
      Some("error[E0308]: mismatched types at line 42\nrun dir: /tmp/scsh-abcdef-run-fix"),
    );
    let b = failure_signature(
      reason::HARNESS_NONZERO,
      Some("error[E0308]: mismatched types at line 57\nrun dir: /tmp/scsh-uvwxyz-run-fix"),
    );
    assert_eq!(a, b, "digits and later lines are erased");
    let c = failure_signature(reason::HARNESS_NONZERO, Some("error[E0433]: unresolved import `foo`"));
    assert_ne!(a, c, "a different first line is a different failure");
    assert_ne!(
      failure_signature(reason::CONTAINER_TIMEOUT, Some("x")),
      failure_signature(reason::CONTAINER_INACTIVE, Some("x")),
      "the reason code is part of the signature"
    );
    assert!(failure_signature("r", Some(&"y".repeat(500))).len() <= 201);
  }
}
