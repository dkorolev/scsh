//! Zombie-container reaper: a `scsh run` that dies (Ctrl-C'd terminal, killed agent, a
//! crashed client) takes its inactivity watchdog with it, and its `--rm` containers keep
//! RUNNING forever — burning CPU, RAM, and disk. The daemon knows which containers belong
//! to genuinely live jobs, so it periodically sweeps every runtime for `scsh-*-run-*`
//! containers nobody claims and destroys them. Their `/tmp` run dirs are handed to the
//! existing [`PruneQueue`].
//!
//! Safety: a container is reaped only after staying unclaimed for
//! [`REAP_AFTER_UNCLAIMED_SWEEPS`] consecutive sweeps (~half an hour) — wide enough that
//! no registration lag, daemon restart, or transient ping gap can cost a live run its
//! container, while a day-old zombie still dies. `SCSH_REAP_CONTAINERS=0` disables the
//! reaper entirely.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use super::model::{SessionLifecycle, Store};
use super::prune::PruneQueue;
use crate::runtime;

/// Seconds between sweeps.
pub const REAP_INTERVAL_SECS: u64 = 60;

/// Consecutive unclaimed sweeps before a container is reaped — about half an hour of
/// grace. This is deliberately conservative: the enemy is the day-old zombie, not the
/// minutes-old one, and the wide margin means a run whose registration lags (or a daemon
/// that restarted mid-run and briefly sees no pings) can never lose a live container to a
/// race. Counting sweeps instead of parsing runtime-specific timestamps also means a
/// daemon restart simply restarts the clock.
pub const REAP_AFTER_UNCLAIMED_SWEEPS: u32 = 30;

/// `SCSH_REAP_CONTAINERS=0` (or `false`) turns the reaper off.
pub fn reaping_disabled() -> bool {
  matches!(std::env::var("SCSH_REAP_CONTAINERS").ok().as_deref(), Some("0") | Some("false"))
}

/// Container names owned by procs of sessions that are genuinely live right now (lifecycle
/// Running — a dead client's Terminated zombie claims nothing).
pub fn claimed_containers(store: &Store, now: u64) -> HashSet<String> {
  let mut set = HashSet::new();
  for s in store.sessions.values() {
    if s.lifecycle_status(now) != SessionLifecycle::Running {
      continue;
    }
    for p in &s.procs {
      if let Some(c) = p.container_name.as_deref().filter(|c| !c.is_empty()) {
        set.insert(c.to_string());
      }
    }
  }
  set
}

/// The pure sweep decision: bump each currently-unclaimed container's consecutive-sweep
/// counter (a claimed or vanished container forgets its count entirely), and return the
/// ones whose count reached [`REAP_AFTER_UNCLAIMED_SWEEPS`]. `counts` is the persistent
/// state between sweeps.
pub fn decide_reaps(
  unclaimed_now: &HashSet<(String, String)>, counts: &mut HashMap<(String, String), u32>,
) -> Vec<(String, String)> {
  counts.retain(|k, _| unclaimed_now.contains(k));
  let mut victims = Vec::new();
  for key in unclaimed_now {
    let n = counts.entry(key.clone()).or_insert(0);
    *n += 1;
    if *n >= REAP_AFTER_UNCLAIMED_SWEEPS {
      victims.push(key.clone());
    }
  }
  victims.sort();
  for v in &victims {
    counts.remove(v);
  }
  victims
}

/// `scsh-*-run-*` container names (running or stopped) known to one runtime.
/// docker/podman: `ps -a --format {{.Names}}`. Apple `container`: `ls -a`, first column.
fn list_scsh_run_containers(rt: &str) -> Vec<String> {
  let out = if rt == "container" {
    Command::new(rt).args(["ls", "-a"]).stderr(Stdio::null()).output()
  } else {
    Command::new(rt).args(["ps", "-a", "--format", "{{.Names}}"]).stderr(Stdio::null()).output()
  };
  let Ok(out) = out else {
    return Vec::new();
  };
  if !out.status.success() {
    return Vec::new();
  }
  String::from_utf8_lossy(&out.stdout)
    .lines()
    .filter_map(|l| l.split_whitespace().next())
    .filter(|n| runtime::is_scsh_run_dir_name(n))
    .map(str::to_string)
    .collect()
}

/// One full sweep: list every runtime, subtract the live jobs' claims, destroy what has
/// stayed unclaimed for [`REAP_AFTER_UNCLAIMED_SWEEPS`] consecutive sweeps, and enqueue
/// the victims' run dirs for the dir janitor. Returns how many containers were destroyed.
pub fn reap_pass(
  store: &Arc<Mutex<Store>>, prune: &Arc<Mutex<PruneQueue>>, counts: &Arc<Mutex<HashMap<(String, String), u32>>>,
  port: u16, now: u64,
) -> usize {
  let claimed = {
    let store = store.lock().unwrap_or_else(|e| e.into_inner());
    claimed_containers(&store, now)
  };
  let mut unclaimed: HashSet<(String, String)> = HashSet::new();
  for rt in runtime::available_runtimes() {
    for name in list_scsh_run_containers(rt) {
      if !claimed.contains(&name) {
        unclaimed.insert((rt.to_string(), name));
      }
    }
  }
  let victims = {
    let mut counts = counts.lock().unwrap_or_else(|e| e.into_inner());
    decide_reaps(&unclaimed, &mut counts)
  };
  for (rt, name) in &victims {
    crate::ui::signals::stop_container(rt, name);
    eprintln!("scsh daemon: reaped orphaned container {name} ({rt})");
  }
  if !victims.is_empty() {
    let mut queue = prune.lock().unwrap_or_else(|e| e.into_inner());
    for (rt, name) in &victims {
      queue.schedule(&format!("/tmp/{name}"), name, rt, false, now);
    }
    queue.save(port);
  }
  victims.len()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::model::{DaemonMode, ProcKind, ProcRecord, ProcStatus, Session, SESSION_IDLE_TIMEOUT_SECS};

  fn session_with_container(id: &str, container: &str, last_seen: u64) -> Session {
    Session {
      id: id.into(),
      started_at: 1,
      ended_at: None,
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      last_seen_at: last_seen,
      client_connected: true,
      run_pid: None,
      skills: vec![],
      procs: vec![ProcRecord {
        index: 0,
        previous_attempt: None,
        kind: ProcKind::Skill,
        label: "claude: add".into(),
        status: ProcStatus::Running,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: Some(container.into()),
        container_runtime: Some("container".into()),
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("claude".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: None,
        lines: vec![],
      }],
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    }
  }

  #[test]
  fn live_sessions_claim_their_containers_zombies_do_not() {
    let now = 2_000;
    let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
    store.sessions.insert("live".into(), session_with_container("live", "scsh-live-run-add", now));
    store
      .sessions
      .insert("dead".into(), session_with_container("dead", "scsh-dead-run-add", now - SESSION_IDLE_TIMEOUT_SECS - 1));
    let claimed = claimed_containers(&store, now);
    assert!(claimed.contains("scsh-live-run-add"), "a pinging session's container is protected");
    assert!(!claimed.contains("scsh-dead-run-add"), "a job past its running-idle deadline claims nothing");
  }

  #[test]
  fn reap_requires_many_consecutive_unclaimed_sweeps_and_a_claim_resets_the_count() {
    let a = ("docker".to_string(), "scsh-abcdef-run-add".to_string());
    let present: HashSet<_> = [a.clone()].into();
    let mut counts = HashMap::new();
    for sweep in 1..REAP_AFTER_UNCLAIMED_SWEEPS {
      assert!(decide_reaps(&present, &mut counts).is_empty(), "sweep {sweep} is still within grace");
    }
    // One claimed (or vanished) sweep forgets the whole history.
    assert!(decide_reaps(&HashSet::new(), &mut counts).is_empty());
    assert!(counts.is_empty(), "a break in the streak resets the count");
    for _ in 1..REAP_AFTER_UNCLAIMED_SWEEPS {
      assert!(decide_reaps(&present, &mut counts).is_empty());
    }
    assert_eq!(decide_reaps(&present, &mut counts), vec![a], "the streak completing reaps");
    assert!(counts.is_empty(), "a reaped container leaves no state behind");
  }

  #[test]
  fn reap_kill_switch_reads_env() {
    let _lock = crate::runtime::test_env_lock();
    std::env::remove_var("SCSH_REAP_CONTAINERS");
    assert!(!reaping_disabled());
    std::env::set_var("SCSH_REAP_CONTAINERS", "0");
    assert!(reaping_disabled());
    std::env::remove_var("SCSH_REAP_CONTAINERS");
  }
}
