//! Runner-owned recovery state. Browsers display these deadlines; they never choose them.

use crate::json::Value;
use std::time::{Duration, Instant};

/// The next action the runner is actually waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
  /// No useful terminal activity before this instant stops the attempt.
  Inactivity(u64),
  /// Startup evidence is checked before deciding whether to stop.
  Startup(u64),
  /// A terminal connection error stayed at an idle prompt through the grace period.
  ConnectionError(u64),
  /// The configured wall-clock budget expires.
  Timeout(u64),
  /// The result must remain unchanged until this instant.
  ResultQuiet(u64),
  /// Native accounting must complete before this deadline.
  Accounting(u64),
  /// Generation is still active after writing the result; no accounting clock is armed.
  Finishing,
  /// The harness has been asked to exit; force cleanup follows this deadline.
  Exit(u64),
  /// Quota pauses ordinary watchdogs; this is the bounded wait's stop deadline.
  Quota(u64),
  /// An approved retry starts after backoff (or a provider reset).
  Retry(u64),
  /// Teardown is in progress; no honest completion ETA exists.
  Cleanup,
}

impl Recovery {
  pub fn parts(self) -> (&'static str, &'static str, Option<u64>) {
    match self {
      Self::Inactivity(at) => ("Inactivity", "Inactivity limit: stopping in", Some(at)),
      Self::Startup(at) => ("Startup", "Checking startup in", Some(at)),
      Self::ConnectionError(at) => ("ConnectionError", "Connection interrupted: stopping in", Some(at)),
      Self::Timeout(at) => ("Timeout", "Time limit: stopping in", Some(at)),
      Self::ResultQuiet(at) => ("ResultQuiet", "Result settling: checking in", Some(at)),
      Self::Accounting(at) => ("Accounting", "Waiting for accounting: deadline in", Some(at)),
      Self::Finishing => ("Finishing", "Result written: waiting for generation to finish", None),
      Self::Exit(at) => ("Exit", "Finishing: cleanup in", Some(at)),
      Self::Quota(at) => ("Quota", "Quota wait limit: stopping in", Some(at)),
      Self::Retry(at) => ("Retry", "Next attempt in", Some(at)),
      Self::Cleanup => ("Cleanup", "Cleaning up attempt", None),
    }
  }

  pub fn json(self) -> String {
    let (kind, _, at) = self.parts();
    let fields = at.map(|at| format!("\"at\":{at}")).unwrap_or_default();
    format!("{{\"{kind}\":{{{fields}}}}}")
  }

  pub fn parse(value: &Value) -> Option<Self> {
    let Value::Object(obj) = value else { return None };
    if obj.len() != 1 {
      return None;
    }
    let (kind, payload) = obj.iter().next()?;
    let Value::Object(fields) = payload else { return None };
    if fields.is_empty() {
      return match kind.as_str() {
        "Finishing" => Some(Self::Finishing),
        "Cleanup" => Some(Self::Cleanup),
        _ => None,
      };
    }
    if fields.len() != 1 {
      return None;
    }
    let (_, Value::Number(at)) = fields.iter().find(|(key, _)| key == "at")? else { return None };
    let at = *at;
    if !at.is_finite() || at < 0.0 || at.fract() != 0.0 {
      return None;
    }
    let at = at as u64;
    Some(match kind.as_str() {
      "Inactivity" => Self::Inactivity(at),
      "Startup" => Self::Startup(at),
      "ConnectionError" => Self::ConnectionError(at),
      "Timeout" => Self::Timeout(at),
      "ResultQuiet" => Self::ResultQuiet(at),
      "Accounting" => Self::Accounting(at),
      "Exit" => Self::Exit(at),
      "Quota" => Self::Quota(at),
      "Retry" => Self::Retry(at),
      _ => return None,
    })
  }
}

/// Convert a monotonic budget to an epoch for display only. Enforcement stays monotonic.
pub fn deadline(since: Instant, budget: Duration) -> u64 {
  let remaining = budget.saturating_sub(since.elapsed());
  std::time::SystemTime::now()
    .checked_add(remaining)
    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
    .map(|at| at.as_secs())
    .unwrap_or(u64::MAX)
}

/// A short grace exists only while fresh evidence says the terminal is still idle.
#[derive(Default)]
pub struct ErrorGrace {
  /// Start of the current uninterrupted idle-error observation, cleared on recovery.
  since: Option<Instant>,
}

impl ErrorGrace {
  pub fn observe(&mut self, now: Instant, idle_error: bool, activity: bool) -> Option<Instant> {
    if activity || !idle_error {
      self.since = None;
    }
    if idle_error {
      Some(*self.since.get_or_insert(now))
    } else {
      None
    }
  }
}

/// Avoid posting unchanged recovery states on every 100ms watchdog poll.
pub struct Reporter {
  /// The proc belongs to this client; absent when the session browser is disabled.
  client: Option<std::sync::Arc<crate::daemon::Client>>,
  /// Route attempt receiving the updates.
  index: usize,
  /// Last published state, shared with the completion controller.
  last: std::sync::Mutex<Option<Recovery>>,
}

impl Reporter {
  pub fn new(client: Option<std::sync::Arc<crate::daemon::Client>>, index: usize) -> Self {
    Self { client, index, last: std::sync::Mutex::new(None) }
  }

  pub fn report(&self, state: Recovery) {
    let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
    if *last != Some(state) {
      if let Some(client) = &self.client {
        client.proc_recovery(self.index, Some(state));
      }
      *last = Some(state);
    }
  }
}

/// An atomic recorder snapshot must be fresh; a dead recorder must not leave a stale
/// error banner authorizing a fast kill while its harness has moved on.
pub fn idle_connection_error_file(path: &std::path::Path, harness: crate::config::Harness) -> bool {
  std::fs::metadata(path)
    .and_then(|meta| meta.modified())
    .ok()
    .and_then(|at| at.elapsed().ok())
    .is_some_and(|age| age < Duration::from_secs(5))
    && std::fs::read_to_string(path).is_ok_and(|text| match harness {
      crate::config::Harness::Claude => claude_idle_connection_error(&text),
      crate::config::Harness::Cursor => cursor_idle_connection_error(&text),
      _ => false,
    })
}

/// Exact Claude terminal failure, followed by its empty input prompt. Quoted errors,
/// partial responses, provider retry banners, and an active turn must not shorten a budget.
pub fn claude_idle_connection_error(screen: &str) -> bool {
  let lines: Vec<_> = screen.lines().map(str::trim).collect();
  let Some(error) = lines.iter().rposition(|line| {
    matches!(
      *line,
      "● API Error: Connection lost mid-response. The response above may be incomplete."
        | "● API Error: Connection dropped (ECONNRESET)"
    )
  }) else {
    return false;
  };
  let after = &lines[error + 1..];
  after.contains(&"❯")
    && after.iter().any(|line| line.starts_with('✻') && line.contains(" · done "))
    && !after.iter().any(|line| line.starts_with('●') || line.contains("esc to interrupt") || line.contains("Retrying"))
}

/// Cursor's terminal ping failure after returning to the follow-up prompt. Require the
/// error at the end of the snapshot so an old failure above resumed work cannot stop it.
pub fn cursor_idle_connection_error(screen: &str) -> bool {
  let lines: Vec<_> = screen.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
  lines.ends_with(&["Error: Something went wrong. Please try again.", "[unavailable] PING timed out"])
    && lines.iter().rev().nth(2).is_some_and(|line| *line == "→ Add a follow-up")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recovery_grace_moves_with_activity_and_disappears_on_recovery() {
    let now = Instant::now();
    assert_eq!(deadline(now, Duration::MAX), u64::MAX);
    let mut grace = ErrorGrace::default();
    assert_eq!(grace.observe(now, false, false), None);
    assert_eq!(grace.observe(now, true, false), Some(now));
    assert_eq!(grace.observe(now + Duration::from_secs(14), true, false), Some(now));
    let moved = now + Duration::from_secs(14);
    assert_eq!(grace.observe(moved, true, true), Some(moved));
    assert_eq!(grace.observe(moved, false, false), None);
    assert_eq!(grace.observe(moved, true, false), Some(moved));
  }

  #[test]
  fn recovery_roundtrips_and_rejects_unknown_fields() {
    for state in [
      Recovery::Inactivity(100),
      Recovery::Startup(100),
      Recovery::ConnectionError(100),
      Recovery::Timeout(100),
      Recovery::ResultQuiet(100),
      Recovery::Accounting(100),
      Recovery::Finishing,
      Recovery::Exit(100),
      Recovery::Quota(100),
      Recovery::Retry(100),
      Recovery::Cleanup,
    ] {
      assert_eq!(Recovery::parse(&crate::json::parse(&state.json()).unwrap()), Some(state));
    }
    for bad in [r#"{"Retry":{"at":1,"typo":2}}"#, r#"{"Retry":{"at":-1}}"#, r#"{"Retry":{}}"#] {
      assert_eq!(Recovery::parse(&crate::json::parse(bad).unwrap()), None);
    }
  }

  #[test]
  fn only_a_terminal_error_at_an_idle_prompt_arms_fast_recovery() {
    let screen = "● API Error: Connection lost mid-response. The response above may be incomplete.
✻ Crunched for 4m 43s · done 11:09 AM
────────────────
❯
────────────────";
    assert!(claude_idle_connection_error(screen));
    assert!(claude_idle_connection_error(&screen.replace(
      "Connection lost mid-response. The response above may be incomplete.",
      "Connection dropped (ECONNRESET)",
    )));
    assert!(!claude_idle_connection_error(&screen.replace("● API Error:", "The log said API Error:")));
    assert!(!claude_idle_connection_error(&screen.replace("❯", "❯ continue")));
    assert!(!claude_idle_connection_error(&format!("{screen}\n● Continuing work")));
    assert!(!claude_idle_connection_error(&format!("{screen}\nRetrying in 3 seconds")));
    assert!(!claude_idle_connection_error(&format!("{screen}\nesc to interrupt")));
  }

  #[test]
  fn cursor_ping_failure_requires_a_terminal_idle_screen() {
    let screen = "Reading files
→ Add a follow-up
Error: Something went wrong. Please try again.
[unavailable] PING timed out
";
    assert!(cursor_idle_connection_error(screen));
    assert!(!cursor_idle_connection_error(&screen.replace("→ Add a follow-up", "Working")));
    assert!(!cursor_idle_connection_error(&format!("{screen}Reading files")));
    assert!(!cursor_idle_connection_error(&format!("{screen}Retrying in 3 seconds")));
    assert!(!cursor_idle_connection_error(&screen.replace("[unavailable]", "The log said [unavailable]")));
  }

  #[test]
  fn a_stale_recorder_snapshot_cannot_authorize_a_fast_stop() {
    let path = std::env::temp_dir().join(format!("scsh-error-screen-{}", crate::runtime::random_nonce_6()));
    std::fs::write(
      &path,
      "● API Error: Connection lost mid-response. The response above may be incomplete.
✻ Crunched for 4m · done 11:09 AM
❯
",
    )
    .unwrap();
    assert!(idle_connection_error_file(&path, crate::config::Harness::Claude));
    std::fs::File::options()
      .write(true)
      .open(&path)
      .unwrap()
      .set_modified(std::time::SystemTime::now() - Duration::from_secs(10))
      .unwrap();
    assert!(!idle_connection_error_file(&path, crate::config::Harness::Claude));
    std::fs::remove_file(&path).unwrap();
    assert!(!idle_connection_error_file(&path, crate::config::Harness::Claude));
  }
}
