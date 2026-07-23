//! The job supervisor: every job is first-class and presumed worth finishing — the
//! daemon itself restarts terminal failures, backed off, budgeted, and circuit-broken,
//! so a job left overnight eventually pushes through or fails loudly with the whole
//! story recorded. The one knob is the job's retries budget (default 25, `0` = fail
//! fast), set explicitly from `scsh run --retries N`; browser starts use the default.
//!
//! Two phases per tick. **Schedule** (under the store lock, pure over [`Store`]): a
//! newly-failed supervised session gets `next_retry_at = now + backoff`, or a `gave_up`
//! verdict when a ceiling trips (the retries budget, or the job-level identical-failure
//! breaker). **Fire** (lock released per restart): due sessions restart through the same
//! `jobs/restart` path the browser buttons use — `mode=resume` for workflow jobs, so the
//! fresh run restores every completed step and re-executes only the failed frontier.
//!
//! A manual browser stop settles the job as `cancelled`, which the supervisor never
//! touches — a human saying stop IS supervision, and it wins. Sessions that died with the
//! daemon (host reboot, crash) need no special adoption pass: persisted supervised
//! sessions whose liveness lapsed read as `failed` on the next tick and schedule normally.
//! Sessions persisted before the retries budget existed parse back to a zero budget, so a
//! daemon upgrade never resurrects history.

use std::sync::{Arc, Mutex};

use super::model::{ProcStatus, Session, SessionLifecycle, Store, JOB_FAIL_STREAK_CAP};

/// How often the daemon's run loop lets the supervisor look at the store.
pub const SUPERVISOR_INTERVAL_SECS: u64 = 15;

/// Backoff between job restarts: 5m · 2ⁿ capped at 60m, with the same deterministic ±20%
/// jitter the step-level retries use — restarting two failed fleets in lockstep would
/// re-create the thundering herd that helped kill them. `SCSH_JOB_BACKOFF_INITIAL_SECS`
/// shrinks the first delay so RESILIENCE-DEMO.md and tests run in minutes, not hours.
fn job_backoff_secs(restarts_done: u32, salt: u64) -> u64 {
  let initial = std::env::var("SCSH_JOB_BACKOFF_INITIAL_SECS")
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .filter(|n| *n >= 1)
    .unwrap_or(5 * 60);
  crate::failure::RetryPolicy {
    max_retries: u32::MAX,
    budget_secs: u64::MAX,
    backoff_initial_secs: initial,
    backoff_cap_secs: (60 * 60).max(initial),
    signature_cap: 1,
  }
  .backoff_delay_secs(restarts_done, salt)
}

fn salt_of(id: &str) -> u64 {
  use std::hash::{Hash, Hasher};
  let mut hasher = std::collections::hash_map::DefaultHasher::new();
  id.hash(&mut hasher);
  hasher.finish()
}

/// The failed step + reason that terminal-failed this session — the job-level breaker's
/// signature. Three consecutive runs dying at the same step for the same reason is a
/// deterministic failure (or an scsh bug), not a provider incident.
fn job_failure_signature(s: &Session) -> String {
  if let Some(p) = s.procs.iter().find(|p| p.status == ProcStatus::Fail && !s.proc_is_superseded(p)) {
    return format!("{}|{}", p.skill_name.as_deref().unwrap_or(&p.label), p.fail_reason.as_deref().unwrap_or(""));
  }
  // No proc failed, so the job died at the SESSION level — its heartbeat went stale while work
  // was still in hand. Name the step that was live when that happened. The old constant
  // "(no failed proc)" was the same string for every such job, which made the breaker read three
  // unrelated stalls as one deterministic failure repeating, and give up citing no step at all.
  let stalled = s
    .procs
    .iter()
    .find(|p| matches!(p.status, ProcStatus::Running | ProcStatus::Waiting))
    .map(|p| p.skill_name.as_deref().unwrap_or(&p.label));
  format!("{}|{}", stalled.unwrap_or("(no live step)"), "session_stale")
}

/// Phase one, under the store lock: decide. Returns the ids whose supervisor state
/// changed (they need persisting and a websocket tick).
pub fn schedule_pass(store: &mut Store, now: u64) -> Vec<String> {
  let mut dirty = Vec::new();
  let ids: Vec<String> = store.sessions.keys().cloned().collect();
  for id in ids {
    let Some(s) = store.sessions.get_mut(&id) else { continue };
    if !s.supervisor.supervised()
      || s.supervisor.gave_up.is_some()
      || s.supervisor.restarted_as.is_some()
      || s.supervisor.next_retry_at.is_some()
      || s.parent_session.is_some()
      || s.profile.is_none()
      || s.repo == super::server::IMAGE_BUILDS_REPO
      || s.repo == crate::daemon::INTERNAL_REPO
      || s.repo == crate::quota::QUOTA_REPO
    {
      continue;
    }
    if s.lifecycle_status(now) != SessionLifecycle::Failed {
      continue;
    }
    // Newly observed terminal failure: ceilings first, then the breaker, then schedule.
    let attempt = s.supervisor.attempt();
    let max = s.supervisor.retries;
    if attempt > max {
      let why = format!("retries budget exhausted ({max} restarts)");
      s.supervisor.gave_up = Some(why.clone());
      crate::failure::log_session_proc(&id, "supervisor_gave_up", "(supervisor)", &why);
      dirty.push(id);
      continue;
    }
    let signature = job_failure_signature(s);
    let streak =
      if s.supervisor.fail_signature.as_deref() == Some(signature.as_str()) { s.supervisor.fail_streak + 1 } else { 1 };
    s.supervisor.fail_signature = Some(signature.clone());
    s.supervisor.fail_streak = streak;
    if streak > JOB_FAIL_STREAK_CAP {
      let why = format!("{streak} consecutive runs failed identically at {signature}");
      s.supervisor.gave_up = Some(why.clone());
      crate::failure::log_session_proc(&id, "supervisor_gave_up", "(supervisor)", &why);
      dirty.push(id);
      continue;
    }
    let delay = job_backoff_secs(attempt.saturating_sub(1), salt_of(&id));
    s.supervisor.next_retry_at = Some(now + delay);
    crate::failure::log_session_proc(
      &id,
      "supervisor_scheduled",
      "(supervisor)",
      &format!("restart {attempt}/{max} scheduled after failure at {signature}; starting in {delay}s"),
    );
    dirty.push(id);
  }
  dirty
}

/// Phase two: fire the restarts whose time has come, one at a time, through the same
/// `jobs/restart` path the browser uses (workflow jobs resume; flat jobs restart from
/// scratch). The store lock is held only around bookkeeping — the restart itself stops
/// containers and spawns a process. Returns the ids it touched.
pub fn fire_due(store: &Arc<Mutex<Store>>, now: u64) -> Vec<String> {
  let due: Vec<(String, bool)> = {
    let guard = store.lock().unwrap_or_else(|e| e.into_inner());
    guard
      .sessions
      .values()
      .filter(|s| s.supervisor.supervised() && s.supervisor.gave_up.is_none() && s.supervisor.restarted_as.is_none())
      .filter(|s| s.supervisor.next_retry_at.is_some_and(|at| now >= at))
      .filter(|s| s.lifecycle_status(now) == SessionLifecycle::Failed)
      .map(|s| (s.id.clone(), s.kind.as_deref() == Some("workflow")))
      .collect()
  };
  let mut dirty = Vec::new();
  for (id, workflow) in due {
    let mode = if workflow { "resume" } else { "scratch" };
    let body = format!("{{\"session\":{},\"mode\":\"{mode}\"}}", crate::json::quote(&id));
    let (status, out, _) = super::server::jobs_restart_response(&body, store);
    // `jobs_restart_response` links restarted_as and inherits the supervisor state onto
    // the fresh session on success; here only the failure paths need bookkeeping.
    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = guard.sessions.get_mut(&id) else { continue };
    if status == 200 {
      s.supervisor.next_retry_at = None;
      crate::failure::log_session_proc(
        &id,
        "supervisor_restart",
        "(supervisor)",
        &format!(
          "restart {} of {} continued as {} ({mode})",
          s.supervisor.attempt(),
          s.supervisor.retries,
          s.supervisor.restarted_as.as_deref().unwrap_or("?")
        ),
      );
    } else if status == 409 {
      // The repo is busy (another job runs there) — wait, do not queue-jump.
      s.supervisor.next_retry_at = Some(now + 60);
    } else {
      // A refusal that waiting cannot fix (vanished def, unmet required param): terminal.
      let why = format!("restart refused (HTTP {status}): {out}");
      s.supervisor.gave_up = Some(why.clone());
      s.supervisor.next_retry_at = None;
      crate::failure::log_session_proc(&id, "supervisor_gave_up", "(supervisor)", &why);
    }
    dirty.push(id);
  }
  dirty
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::model::{DaemonMode, ProcKind, ProcRecord, SupervisorState, DEFAULT_JOB_RETRIES};

  fn failed_supervised_session(id: &str, now: u64) -> Session {
    Session {
      id: id.into(),
      started_at: now - 100,
      ended_at: Some(now - 10),
      profile: Some("greet".into()),
      kind: Some("workflow".into()),
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![ProcRecord {
        index: 0,
        previous_attempt: None,
        label: "claude: fix".into(),
        kind: ProcKind::Skill,
        status: ProcStatus::Fail,
        skill_name: Some("fix".into()),
        harness: Some("claude".into()),
        model: None,
        started_at: Some(now - 50),
        note: None,
        detail: None,
        fail_reason: Some(crate::failure::reason::HARNESS_NONZERO.into()),
        elapsed: Some(5.0),
        lines: Vec::new(),
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
      }],
      last_seen_at: now - 10,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: SupervisorState::fresh(DEFAULT_JOB_RETRIES),
    }
  }

  #[test]
  fn schedule_pass_schedules_failed_supervised_jobs_with_backoff() {
    let now = 1_000_000;
    let mut store = Store::new(DaemonMode::Persistent, 7274, now);
    store.insert_session("aaaaaa".into(), failed_supervised_session("aaaaaa", now));
    let dirty = schedule_pass(&mut store, now);
    assert_eq!(dirty, vec!["aaaaaa".to_string()]);
    let s = &store.sessions["aaaaaa"];
    let at = s.supervisor.next_retry_at.expect("scheduled");
    // First restart: 5m ± 20% jitter.
    assert!(at >= now + 4 * 60 && at <= now + 6 * 60, "5m±20%, got +{}s", at - now);
    assert_eq!(s.supervisor.fail_signature.as_deref(), Some("fix|harness_nonzero_exit"));
    assert_eq!(s.supervisor.fail_streak, 1);
    // A second pass changes nothing — the schedule is already set.
    assert!(schedule_pass(&mut store, now + 1).is_empty());
  }

  /// A job killed by the heartbeat rule has no failed proc, so the signature has to come from
  /// somewhere else. It used to be the constant "(no failed proc)" — identical for every stalled
  /// job, which made the breaker count unrelated stalls as one failure repeating and give up
  /// naming no step at all. It must name the step that was live instead.
  #[test]
  fn a_stall_is_signed_by_the_step_that_was_live_not_a_constant() {
    let now = 1_000_000;
    let mut stalled = failed_supervised_session("bbbbbb", now);
    stalled.procs[0].status = ProcStatus::Running;
    stalled.procs[0].fail_reason = None;
    stalled.procs[0].elapsed = None;
    let signature = job_failure_signature(&stalled);
    assert_eq!(signature, "fix|session_stale", "the stalled step is what identifies the failure");
    assert!(!signature.contains("no failed proc"), "the old catch-all constant is gone");

    // A different step stalling is a DIFFERENT failure, so it must not extend the streak.
    let mut elsewhere = stalled.clone();
    elsewhere.procs[0].skill_name = Some("prepare".into());
    assert_ne!(job_failure_signature(&elsewhere), signature);
  }

  #[test]
  fn schedule_pass_ignores_zero_budget_cancelled_and_settled_jobs() {
    let now = 1_000_000;
    let mut store = Store::new(DaemonMode::Persistent, 7274, now);
    // Zero retries budget (scsh run --retries 0, or a pre-feature record): fail fast.
    let mut opted_out = failed_supervised_session("attend", now);
    opted_out.supervisor = Default::default();
    store.insert_session("attend".into(), opted_out);
    // Manually stopped (cancelled): a human said stop — obey.
    let mut cancelled = failed_supervised_session("cancel", now);
    cancelled.procs[0].fail_reason = Some(crate::failure::reason::FORCE_STOPPED.into());
    store.insert_session("cancel".into(), cancelled);
    // Already restarted: its successor carries the chain.
    let mut chained = failed_supervised_session("chained", now);
    chained.supervisor.restarted_as = Some("newone".into());
    store.insert_session("chained".into(), chained);
    // Gave up: terminal.
    let mut done = failed_supervised_session("gaveup", now);
    done.supervisor.gave_up = Some("budget".into());
    store.insert_session("gaveup".into(), done);
    assert!(schedule_pass(&mut store, now).is_empty());
  }

  #[test]
  fn breaker_trips_after_identical_failures_and_budget_after_max_attempts() {
    let now = 1_000_000;
    let mut store = Store::new(DaemonMode::Persistent, 7274, now);
    // Third consecutive identical failure (streak inherited at 3) trips the breaker.
    let mut identical = failed_supervised_session("identic", now);
    identical.supervisor.fail_signature = Some("fix|harness_nonzero_exit".into());
    identical.supervisor.fail_streak = JOB_FAIL_STREAK_CAP;
    identical.supervisor.job_attempt = 5;
    store.insert_session("identic".into(), identical);
    schedule_pass(&mut store, now);
    let s = &store.sessions["identic"];
    assert!(s.supervisor.gave_up.as_deref().is_some_and(|w| w.contains("identically")), "{:?}", s.supervisor.gave_up);
    assert!(s.supervisor.next_retry_at.is_none());

    // A DIFFERENT failure resets the streak and schedules normally.
    let mut different = failed_supervised_session("differs", now);
    different.supervisor.fail_signature = Some("prepare|container_timeout".into());
    different.supervisor.fail_streak = JOB_FAIL_STREAK_CAP;
    store.insert_session("differs".into(), different);
    schedule_pass(&mut store, now);
    let s = &store.sessions["differs"];
    assert_eq!(s.supervisor.fail_streak, 1, "a new signature resets the streak");
    assert!(s.supervisor.next_retry_at.is_some());

    // The restart budget is a hard ceiling regardless of signatures.
    let mut spent = failed_supervised_session("spentit", now);
    spent.supervisor.job_attempt = DEFAULT_JOB_RETRIES + 1;
    store.insert_session("spentit".into(), spent);
    schedule_pass(&mut store, now);
    let s = &store.sessions["spentit"];
    assert!(
      s.supervisor.gave_up.as_deref().is_some_and(|w| w.contains("budget exhausted")),
      "{:?}",
      s.supervisor.gave_up
    );
  }

  #[test]
  fn job_backoff_doubles_from_five_minutes_and_caps_at_an_hour() {
    for (n, base) in [(0u32, 300u64), (1, 600), (2, 1200), (3, 2400), (4, 3600), (9, 3600)] {
      let d = job_backoff_secs(n, 42);
      let span = base / 5;
      assert!(d >= base - span && d <= base + span, "restart {n}: {d}s outside {base}±{span}");
    }
  }
}
