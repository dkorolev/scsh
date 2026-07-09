//! Pure event model for the scsh session browser daemon.

use std::collections::BTreeMap;

/// Default HTTP port (`scsh` on a numeric keypad: 7→s, 2→c, 7→s, 4→h).
pub const DEFAULT_PORT: u16 = 7274;

/// Ephemeral daemon idle timeout before shutdown when no clients are connected.
pub const EPHEMERAL_IDLE_SECS: u64 = 300;

/// Grace period with no alive clients before the browser shows an ephemeral shutdown countdown.
pub const EPHEMERAL_COUNTDOWN_AFTER_SECS: u64 = 5;

/// Silence threshold before a session without `ended_at` is marked terminated.
pub const SESSION_STALE_SECS: u64 = 10;

/// Maximum sessions retained in daemon state.
pub const MAX_STORED_SESSIONS: usize = 200;

/// How a daemon was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonMode {
  /// `scsh daemon start` — runs until `scsh daemon stop`.
  Persistent,
  /// Auto-started alongside a `scsh run` — exits after idle timeout.
  Ephemeral,
}

impl DaemonMode {
  pub fn as_str(self) -> &'static str {
    match self {
      DaemonMode::Persistent => "persistent",
      DaemonMode::Ephemeral => "ephemeral",
    }
  }

  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "persistent" => Some(DaemonMode::Persistent),
      "ephemeral" => Some(DaemonMode::Ephemeral),
      _ => None,
    }
  }
}

/// Index-page lifecycle for a `scsh run` session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLifecycle {
  Running,
  Completed,
  Failed,
  Cancelled,
  Terminated,
}

impl SessionLifecycle {
  pub fn label(self) -> &'static str {
    match self {
      SessionLifecycle::Running => "running",
      SessionLifecycle::Completed => "completed",
      SessionLifecycle::Failed => "failed",
      SessionLifecycle::Cancelled => "cancelled",
      SessionLifecycle::Terminated => "terminated abruptly",
    }
  }

  pub fn css_class(self) -> &'static str {
    match self {
      SessionLifecycle::Running => "running",
      SessionLifecycle::Completed => "completed",
      SessionLifecycle::Failed => "failed",
      SessionLifecycle::Cancelled => "cancelled",
      SessionLifecycle::Terminated => "terminated",
    }
  }
}

/// Lifecycle status of one proc row (build or skill).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcStatus {
  Waiting,
  Running,
  Ok,
  Fail,
}

impl ProcStatus {
  pub fn as_str(self) -> &'static str {
    match self {
      ProcStatus::Waiting => "waiting",
      ProcStatus::Running => "running",
      ProcStatus::Ok => "ok",
      ProcStatus::Fail => "fail",
    }
  }

  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "waiting" => Some(ProcStatus::Waiting),
      "running" => Some(ProcStatus::Running),
      "ok" => Some(ProcStatus::Ok),
      "fail" => Some(ProcStatus::Fail),
      _ => None,
    }
  }
}

/// Build vs skill row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcKind {
  Build,
  Skill,
}

impl ProcKind {
  pub fn as_str(self) -> &'static str {
    match self {
      ProcKind::Build => "build",
      ProcKind::Skill => "skill",
    }
  }

  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "build" => Some(ProcKind::Build),
      "skill" => Some(ProcKind::Skill),
      _ => None,
    }
  }
}

/// One timestamped output line from a proc.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputLine {
  pub at: f64,
  pub text: String,
}

/// A collapsible row on the live board (image build or skill).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcRecord {
  pub index: usize,
  pub label: String,
  pub kind: ProcKind,
  pub status: ProcStatus,
  pub skill_name: Option<String>,
  pub harness: Option<String>,
  pub model: Option<String>,
  /// Unix seconds when the proc entered `running` (for live elapsed / idle in the browser).
  pub started_at: Option<u64>,
  pub note: Option<String>,
  pub detail: Option<String>,
  /// Stable reason code when `status == fail` (e.g. `container_timeout`).
  pub fail_reason: Option<String>,
  pub elapsed: Option<f64>,
  pub lines: Vec<OutputLine>,
  pub container_name: Option<String>,
  /// Host path of this proc's asciinema recording: the live run-dir file while the
  /// container runs (grows in real time; a prefix is a valid partial cast), then the
  /// durable copy under the daemon dir after the skill finishes.
  pub cast_path: Option<String>,
}

/// One skill listed in a session's start payload.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillMeta {
  pub name: String,
  pub harness: String,
}

/// One `scsh run` invocation — grouped by session id (six lowercase letters).
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
  pub id: String,
  pub started_at: u64,
  /// Unix seconds when the `scsh run` client deregistered (run finished).
  pub ended_at: Option<u64>,
  pub profile: Option<String>,
  pub repo: String,
  /// Git branch checked out in the repo when the run started (`rev-parse --abbrev-ref HEAD`).
  pub branch: String,
  pub skills: Vec<SkillMeta>,
  pub procs: Vec<ProcRecord>,
  /// Last ping or session-scoped API event (unix seconds).
  pub last_seen_at: u64,
  /// True while the `scsh run` client is registered (between register and deregister).
  pub client_connected: bool,
  /// Host PID of the `scsh run` / `scsh build-images` process, when known — so the web UI can
  /// force-stop a stalled job (SIGTERM the run; its signal handler tears containers down).
  pub run_pid: Option<u32>,
}

/// A repository opened from the daemon UI, ready to start jobs in. Kept in memory only (a
/// convenience list for the browser; the jobs themselves are [`Session`]s keyed on `repo`).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenRepo {
  /// Absolute git top-level of the repository.
  pub path: String,
  /// Unix seconds when it was opened.
  pub opened_at: u64,
  /// Whether the working tree was clean (no uncommitted changes) when last opened.
  pub clean: bool,
}

/// Full daemon state persisted to disk and served over HTTP.
#[derive(Debug, Clone, PartialEq)]
pub struct Store {
  pub mode: DaemonMode,
  pub port: u16,
  /// When this daemon process started (unix seconds).
  pub started_at: u64,
  pub active_clients: u32,
  pub last_activity: u64,
  /// When `alive_clients` last dropped to zero (unix seconds); drives ephemeral shutdown.
  pub no_alive_since: Option<u64>,
  pub sessions: BTreeMap<String, Session>,
  /// Repositories opened from the web UI, keyed by absolute path. In-memory only (rebuilt
  /// empty on restart via `..Store::new(..)`); persistence stays session-scoped in `db`.
  pub open_repos: BTreeMap<String, OpenRepo>,
}

impl Store {
  pub fn new(mode: DaemonMode, port: u16, now: u64) -> Store {
    Store {
      mode,
      port,
      started_at: now,
      active_clients: 0,
      last_activity: now,
      no_alive_since: Some(now),
      sessions: BTreeMap::new(),
      open_repos: BTreeMap::new(),
    }
  }

  pub fn touch(&mut self, now: u64) {
    self.last_activity = now;
  }

  /// Remember a repository opened from the web UI (replacing any prior entry for that path).
  pub fn open_repo(&mut self, repo: OpenRepo) {
    self.open_repos.insert(repo.path.clone(), repo);
  }

  /// The one-job-per-directory guard: true while a job (session) is still running in `repo`.
  /// Image-build sessions use a synthetic repo label, so they never block a real repo.
  pub fn job_running_in(&self, repo: &str, now: u64) -> bool {
    self.sessions.values().any(|s| s.repo == repo && s.lifecycle_status(now) == SessionLifecycle::Running)
  }

  /// Registered `scsh run` clients that are still sending pings (not stale / terminated).
  pub fn alive_clients(&self, now: u64) -> u32 {
    self
      .sessions
      .values()
      .filter(|s| s.client_connected && s.lifecycle_status(now) == SessionLifecycle::Running)
      .count() as u32
  }

  /// Drop stale registrations and refresh ephemeral idle tracking.
  pub fn reconcile(&mut self, now: u64) {
    for session in self.sessions.values_mut() {
      if session.client_connected && session.lifecycle_status(now) != SessionLifecycle::Running {
        session.client_connected = false;
      }
    }
    self.active_clients = self.sessions.values().filter(|s| s.client_connected).count() as u32;
    if self.alive_clients(now) > 0 {
      self.no_alive_since = None;
    } else if self.no_alive_since.is_none() {
      self.no_alive_since = Some(now);
    }
  }

  /// Seconds until ephemeral shutdown, once the no-alive grace period has elapsed.
  pub fn ephemeral_shutdown_in_secs(&self, now: u64) -> Option<u64> {
    if self.mode != DaemonMode::Ephemeral {
      return None;
    }
    let since = self.no_alive_since?;
    let idle = now.saturating_sub(since);
    if idle < EPHEMERAL_COUNTDOWN_AFTER_SECS {
      return None;
    }
    Some(EPHEMERAL_IDLE_SECS.saturating_sub(idle))
  }

  pub fn should_shutdown_ephemeral(&self, now: u64) -> bool {
    self.mode == DaemonMode::Ephemeral
      && self.alive_clients(now) == 0
      && self.no_alive_since.is_some_and(|since| now.saturating_sub(since) >= EPHEMERAL_IDLE_SECS)
  }

  pub fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
    self.sessions.get_mut(id)
  }

  pub fn proc_mut(&mut self, session_id: &str, proc_index: usize) -> Option<&mut ProcRecord> {
    self.session_mut(session_id).and_then(|s| s.procs.iter_mut().find(|p| p.index == proc_index))
  }

  pub fn insert_session(&mut self, id: String, session: Session) {
    self.sessions.insert(id, session);
    trim_sessions_to_cap(&mut self.sessions);
  }
}

/// Drop oldest sessions when the map exceeds [`MAX_STORED_SESSIONS`] (same rule as `insert_session`).
pub fn trim_sessions_to_cap(sessions: &mut std::collections::BTreeMap<String, Session>) {
  while sessions.len() > MAX_STORED_SESSIONS {
    let Some(old_id) = sessions.iter().min_by_key(|(_, s)| s.started_at).map(|(id, _)| id.clone()) else {
      break;
    };
    sessions.remove(&old_id);
  }
}

/// Sessions sorted for the index page: running first, then by start time descending.
pub fn sessions_for_index<'a>(sessions: &'a BTreeMap<String, Session>, now: u64) -> Vec<&'a Session> {
  let mut list: Vec<&Session> = sessions.values().collect();
  list.sort_by(|a, b| {
    let a_live = a.lifecycle_status(now) == SessionLifecycle::Running;
    let b_live = b.lifecycle_status(now) == SessionLifecycle::Running;
    match (a_live, b_live) {
      (true, false) => std::cmp::Ordering::Less,
      (false, true) => std::cmp::Ordering::Greater,
      _ => b.started_at.cmp(&a.started_at),
    }
  });
  list
}

impl Session {
  /// True while any proc has not reached a terminal state (ok/fail).
  pub fn has_incomplete_procs(&self) -> bool {
    self.procs.iter().any(|p| p.status == ProcStatus::Running || p.status == ProcStatus::Waiting)
  }

  pub fn lifecycle_status(&self, now: u64) -> SessionLifecycle {
    if self.ended_at.is_some() {
      if self.has_incomplete_procs() {
        return SessionLifecycle::Cancelled;
      }
      if self.procs.iter().any(|p| p.status == ProcStatus::Fail) {
        return SessionLifecycle::Failed;
      }
      return SessionLifecycle::Completed;
    }
    if now.saturating_sub(self.last_seen_at) > SESSION_STALE_SECS {
      return SessionLifecycle::Terminated;
    }
    SessionLifecycle::Running
  }

  pub fn duration_secs(&self, now: u64) -> Option<u64> {
    if let Some(end) = self.ended_at {
      return Some(end.saturating_sub(self.started_at));
    }
    let lifecycle = self.lifecycle_status(now);
    if lifecycle == SessionLifecycle::Running {
      return Some(now.saturating_sub(self.started_at));
    }
    if lifecycle == SessionLifecycle::Terminated {
      return Some(self.last_seen_at.saturating_sub(self.started_at));
    }
    None
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lifecycle_completed_when_ended_cleanly() {
    let session = Session {
      id: "done".into(),
      started_at: 100,
      ended_at: Some(200),
      profile: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![ProcRecord {
        index: 0,
        label: "skill".into(),
        kind: ProcKind::Skill,
        status: ProcStatus::Ok,
        skill_name: None,
        harness: None,
        model: None,
        started_at: Some(100),
        note: None,
        detail: None,
        fail_reason: None,
        elapsed: Some(5.0),
        lines: Vec::new(),
        container_name: None,
        cast_path: None,
      }],
      last_seen_at: 200,
      client_connected: false,
      run_pid: None,
    };
    assert_eq!(session.lifecycle_status(200), SessionLifecycle::Completed);
    assert_eq!(session.duration_secs(200), Some(100));
  }

  #[test]
  fn lifecycle_terminated_when_stale_without_ended_at() {
    let session = Session {
      id: "stale".into(),
      started_at: 100,
      ended_at: None,
      profile: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 100,
      client_connected: true,
      run_pid: None,
    };
    assert_eq!(session.lifecycle_status(110), SessionLifecycle::Running);
    assert_eq!(session.lifecycle_status(111), SessionLifecycle::Terminated);
  }

  #[test]
  fn lifecycle_cancelled_when_ended_with_incomplete_procs() {
    let session = Session {
      id: "cancel".into(),
      started_at: 1,
      ended_at: Some(50),
      profile: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![ProcRecord {
        index: 0,
        label: "skill".into(),
        kind: ProcKind::Skill,
        status: ProcStatus::Running,
        skill_name: None,
        harness: None,
        model: None,
        started_at: Some(1),
        note: None,
        detail: None,
        fail_reason: None,
        elapsed: None,
        lines: Vec::new(),
        container_name: None,
        cast_path: None,
      }],
      last_seen_at: 50,
      client_connected: false,
      run_pid: None,
    };
    assert_eq!(session.lifecycle_status(50), SessionLifecycle::Cancelled);
  }

  #[test]
  fn lifecycle_running_while_incomplete_procs_and_recent() {
    let session = Session {
      id: "test".into(),
      started_at: 1,
      ended_at: None,
      profile: None,
      repo: "/repo".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![
        ProcRecord {
          index: 0,
          label: "done".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: None,
          harness: None,
          model: None,
          started_at: None,
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: Vec::new(),
          container_name: None,
          cast_path: None,
        },
        ProcRecord {
          index: 1,
          label: "still going".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Waiting,
          skill_name: None,
          harness: None,
          model: None,
          started_at: None,
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: Vec::new(),
          container_name: None,
          cast_path: None,
        },
      ],
      last_seen_at: 1,
      client_connected: true,
      run_pid: None,
    };
    assert!(session.has_incomplete_procs());
    assert_eq!(session.lifecycle_status(2), SessionLifecycle::Running);
  }

  #[test]
  fn sessions_for_index_puts_running_first_then_recent() {
    let running = Session {
      id: "run".into(),
      started_at: 10,
      ended_at: None,
      profile: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 100,
      client_connected: true,
      run_pid: None,
    };
    let done = Session {
      id: "done".into(),
      started_at: 200,
      ended_at: Some(250),
      profile: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 250,
      client_connected: false,
      run_pid: None,
    };
    let mut sessions = BTreeMap::new();
    sessions.insert(done.id.clone(), done);
    sessions.insert(running.id.clone(), running);
    let ordered = sessions_for_index(&sessions, 100);
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].id, "run");
    assert_eq!(ordered[1].id, "done");
  }

  #[test]
  fn insert_session_evicts_oldest_when_over_cap() {
    let mut store = Store::new(DaemonMode::Persistent, DEFAULT_PORT, 0);
    for i in 0..=MAX_STORED_SESSIONS {
      store.insert_session(
        format!("{i:06}"),
        Session {
          id: format!("{i:06}"),
          started_at: i as u64,
          ended_at: None,
          profile: None,
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: Vec::new(),
          last_seen_at: i as u64,
          client_connected: false,
          run_pid: None,
        },
      );
    }
    assert_eq!(store.sessions.len(), MAX_STORED_SESSIONS);
    assert!(!store.sessions.contains_key("000000"));
    assert!(store.sessions.contains_key(&format!("{MAX_STORED_SESSIONS:06}")));
  }

  #[test]
  fn session_id_is_six_lowercase_letters() {
    let id = crate::runtime::random_nonce_6();
    assert_eq!(id.len(), 6);
    assert!(id.chars().all(|c| c.is_ascii_lowercase()));
  }

  #[test]
  fn ephemeral_shutdown_after_idle() {
    let now = 1_000_000;
    let mut store = Store::new(DaemonMode::Ephemeral, DEFAULT_PORT, now);
    store.reconcile(now + 100);
    assert!(!store.should_shutdown_ephemeral(now + 100));
    assert!(!store.should_shutdown_ephemeral(now + EPHEMERAL_IDLE_SECS - 1));
    assert!(store.should_shutdown_ephemeral(now + EPHEMERAL_IDLE_SECS));
  }

  #[test]
  fn terminated_client_not_counted_alive() {
    let now = 100;
    let mut store = Store::new(DaemonMode::Ephemeral, DEFAULT_PORT, now);
    store.insert_session(
      "stale".into(),
      Session {
        id: "stale".into(),
        started_at: now,
        ended_at: None,
        profile: None,
        repo: "/r".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: now,
        client_connected: true,
        run_pid: None,
      },
    );
    assert_eq!(store.alive_clients(now + SESSION_STALE_SECS), 1);
    assert_eq!(store.alive_clients(now + SESSION_STALE_SECS + 1), 0);
    store.reconcile(now + SESSION_STALE_SECS + 1);
    assert_eq!(store.active_clients, 0);
    assert!(store.no_alive_since.is_some());
  }

  #[test]
  fn ephemeral_countdown_after_no_alive_grace() {
    let now = 0;
    let store = Store::new(DaemonMode::Ephemeral, DEFAULT_PORT, now);
    assert!(store.ephemeral_shutdown_in_secs(now + EPHEMERAL_COUNTDOWN_AFTER_SECS - 1).is_none());
    assert_eq!(
      store.ephemeral_shutdown_in_secs(now + EPHEMERAL_COUNTDOWN_AFTER_SECS),
      Some(EPHEMERAL_IDLE_SECS - EPHEMERAL_COUNTDOWN_AFTER_SECS)
    );
    assert_eq!(store.ephemeral_shutdown_in_secs(now + EPHEMERAL_IDLE_SECS), Some(0));
  }

  #[test]
  fn persistent_never_auto_shutdown() {
    let store = Store::new(DaemonMode::Persistent, DEFAULT_PORT, 0);
    assert!(!store.should_shutdown_ephemeral(u64::MAX));
  }
}
