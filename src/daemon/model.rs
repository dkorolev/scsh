//! Pure event model for the scsh session browser daemon.

use std::collections::BTreeMap;

use super::workflow::WorkflowMeta;

/// Default HTTP port (`scsh` on a numeric keypad: 7→s, 2→c, 7→s, 4→h).
pub const DEFAULT_PORT: u16 = 7274;

/// Ephemeral daemon idle timeout before shutdown when no clients are connected.
pub const EPHEMERAL_IDLE_SECS: u64 = 300;

/// Grace period with no alive clients before the browser shows an ephemeral shutdown countdown.
pub const EPHEMERAL_COUNTDOWN_AFTER_SECS: u64 = 5;

/// A registered job must start at least one proc within this window. Before work starts,
/// silence is a startup failure rather than evidence that a long-running proc is dead.
pub const SESSION_START_TIMEOUT_SECS: u64 = 30;

/// Once any proc has started, only sustained job-wide inactivity is a liveness failure. Use the
/// same allowance as the harness watchdog so the executor and browser share one running rule.
pub const SESSION_IDLE_TIMEOUT_SECS: u64 = crate::config::DEFAULT_INACTIVITY_TIMEOUT_SECS;

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
}

impl SessionLifecycle {
  pub fn label(self) -> &'static str {
    match self {
      SessionLifecycle::Running => "running",
      SessionLifecycle::Completed => "completed",
      SessionLifecycle::Failed => "failed",
      SessionLifecycle::Cancelled => "cancelled",
    }
  }

  pub fn css_class(self) -> &'static str {
    match self {
      SessionLifecycle::Running => "running",
      SessionLifecycle::Completed => "completed",
      SessionLifecycle::Failed => "failed",
      SessionLifecycle::Cancelled => "cancelled",
    }
  }
}

/// Lifecycle status of one proc row (build or skill).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcStatus {
  Waiting,
  Running,
  Ok,
  /// The harness produced a valid durable result, but its own exit or its container teardown was
  /// unreliable. Successful for dependency and job outcomes; orange in the UI so the
  /// infrastructure wrinkle remains visible.
  Graceful,
  Fail,
  /// Decided but never run — a workflow step gated off (or downstream of a skipped step).
  Skipped,
}

impl ProcStatus {
  pub fn as_str(self) -> &'static str {
    match self {
      ProcStatus::Waiting => "waiting",
      ProcStatus::Running => "running",
      ProcStatus::Ok => "ok",
      ProcStatus::Graceful => "graceful",
      ProcStatus::Fail => "fail",
      ProcStatus::Skipped => "skipped",
    }
  }

  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "waiting" => Some(ProcStatus::Waiting),
      "running" => Some(ProcStatus::Running),
      "ok" => Some(ProcStatus::Ok),
      "graceful" => Some(ProcStatus::Graceful),
      "fail" => Some(ProcStatus::Fail),
      "skipped" => Some(ProcStatus::Skipped),
      _ => None,
    }
  }
}

/// Build vs skill vs annotate row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcKind {
  Build,
  Skill,
  Annotate,
}

impl ProcKind {
  pub fn as_str(self) -> &'static str {
    match self {
      ProcKind::Build => "build",
      ProcKind::Skill => "skill",
      ProcKind::Annotate => "annotate",
    }
  }

  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "build" => Some(ProcKind::Build),
      "skill" => Some(ProcKind::Skill),
      "annotate" => Some(ProcKind::Annotate),
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
  /// The immediately preceding attempt of this same logical run. Retries form an explicit
  /// chain; `None` means the first attempt (or a record written before attempt lineage was
  /// introduced). The daemon validates this edge when the replacement proc registers.
  pub previous_attempt: Option<usize>,
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
  /// Stable machine-readable state/reason code (e.g. `container_timeout`). Browser stop
  /// and restart requests use their `*_requested` code while teardown/replacement is pending.
  pub fail_reason: Option<String>,
  pub elapsed: Option<f64>,
  pub lines: Vec<OutputLine>,
  pub container_name: Option<String>,
  /// Runtime that owns `container_name`: `container` (Apple Containers), `docker`, or
  /// `podman`. Absent on cached work and sessions persisted by older scsh builds.
  pub container_runtime: Option<String>,
  /// Host path of this proc's asciinema recording: the live run-dir file while the
  /// container runs (grows in real time; a prefix is a valid partial cast), then the
  /// durable copy under the daemon dir after the skill finishes.
  pub cast_path: Option<String>,
  /// Host path of the packdiff-packed review page for the commits this step brought into
  /// the caller's branch (`$SCSH_HOME/sessions/<session>/diffs/…`). Set after the run
  /// integrates a commit-enabled skill's commits; `None` for steps that committed nothing.
  pub diff_path: Option<String>,
  /// Manifest skill key this invocation came from (`add`, `conventions-reviewer`). Shared
  /// across a matrix fleet so the job page can group routes side-by-side. `None` for builds.
  pub skill_source: Option<String>,
  /// Matrix route name (`codex-terra`); `None` for a direct (non-matrix) skill or builds.
  pub route: Option<String>,
  /// Durable copy of the skill's result JSON under `$SCSH_HOME/sessions/<id>/results/`.
  pub result_path: Option<String>,
  /// Host path of the cast an `Annotate` proc is summarizing. Lets the chapters endpoint
  /// point a still-chapterless recording at the job doing its annotation (the "chapters:
  /// summarizing…" link on the job page). `None` on every other proc kind.
  pub annotate_target: Option<String>,
}

/// One skill listed in a session's start payload.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillMeta {
  pub name: String,
  pub harness: String,
}

/// Job restarts the supervisor may spend on one job before giving up. Every job gets
/// this budget unless its start says otherwise (`scsh run --retries N`, or `"retries"`
/// on `jobs/start`); `0` opts a job out of supervision entirely.
pub const DEFAULT_JOB_RETRIES: u32 = 10;
/// Consecutive supervisor restarts failing with the SAME step + reason before the
/// job-level breaker trips — scsh's own bug or a deterministic workflow failure should
/// not burn ten fleets overnight.
pub const JOB_FAIL_STREAK_CAP: u32 = 3;

/// Supervisor state for one session. Every job is first-class: the daemon restarts a
/// terminal failure up to the job's retries budget, and the state is inherited
/// (attempt-incremented) by the fresh session each restart creates, so the budget spans
/// the whole chain. The all-zero default — what sessions persisted before this feature
/// parse back to — means "no retries budget", so a daemon upgrade never resurrects
/// history; new sessions are stamped with their budget at creation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SupervisorState {
  /// 1-based run ordinal within the restart chain (0 on old records = 1, the original).
  pub job_attempt: u32,
  /// Restart budget for the whole chain; `0` = never restarted by the daemon.
  pub retries: u32,
  /// When the supervisor will restart this failed job (unix secs); `None` = nothing scheduled.
  pub next_retry_at: Option<u64>,
  /// The failed step + reason of the run this state was inherited from, and how many
  /// consecutive runs failed exactly that way — the job-level breaker's memory.
  pub fail_signature: Option<String>,
  pub fail_streak: u32,
  /// Set when the supervisor stopped retrying (ceiling or breaker), with the reason —
  /// the loud, permanent, explained terminal state.
  pub gave_up: Option<String>,
  /// The session that replaced this one after a restart (manual or supervisor's).
  pub restarted_as: Option<String>,
}

impl SupervisorState {
  /// The state every fresh job starts with: attempt 1 of `retries` restarts.
  pub fn fresh(retries: u32) -> SupervisorState {
    SupervisorState { job_attempt: 1, retries, ..Default::default() }
  }

  /// Whether the daemon restarts this job on terminal failure.
  pub fn supervised(&self) -> bool {
    self.retries > 0
  }

  pub fn attempt(&self) -> u32 {
    self.job_attempt.max(1)
  }

  /// The state a restart's fresh session starts with: one attempt later, the retry
  /// schedule cleared, breaker memory carried.
  pub fn inherited(&self) -> SupervisorState {
    SupervisorState {
      job_attempt: self.attempt() + 1,
      retries: self.retries,
      next_retry_at: None,
      fail_signature: self.fail_signature.clone(),
      fail_streak: self.fail_streak,
      gave_up: None,
      restarted_as: None,
    }
  }
}

/// One `scsh run` invocation — grouped by session id (six lowercase letters).
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
  pub id: String,
  pub started_at: u64,
  /// Unix seconds when the `scsh run` client deregistered (run finished).
  pub ended_at: Option<u64>,
  pub profile: Option<String>,
  /// How the run was invoked — `"profile"`, `"definition"`, `"workflow"`, or `"build"` —
  /// so the UI can label the session honestly (a workflow is not a profile). `None` on
  /// sessions persisted by older builds; render those as `"profile"`.
  pub kind: Option<String>,
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
  /// Optional workflow dependency graph (`needs` DAG). `None` for flat jobs, builds, and
  /// sessions persisted before this field existed.
  pub workflow: Option<WorkflowMeta>,
  /// When this session was spawned as a follow-on job (e.g. standalone `annotate-cast` for
  /// recordings under `$SCSH_HOME/sessions/<id>/`), the parent session id. `None` otherwise.
  pub parent_session: Option<String>,
  /// Unattended-supervisor state; [`SupervisorState::default`] for attended jobs.
  pub supervisor: SupervisorState,
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
  /// Synthetic repo labels (`(image builds)`, `(internal)`) never collide with a real path,
  /// so those sessions never block a real repo.
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

/// Top-level sessions sorted for the index page: running first, then by start time descending.
/// Follow-on annotation sessions stay addressable and live in the store, but belong to their
/// `parent_session` and must never be promoted to peer jobs in a listing.
pub fn sessions_for_index(sessions: &BTreeMap<String, Session>, now: u64) -> Vec<&Session> {
  let mut list: Vec<&Session> = sessions.values().filter(|session| session.parent_session.is_none()).collect();
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

  /// Whether this job crossed the start boundary. Predeclared workflow rows may wait on
  /// dependencies for a long time after another row started, so `Waiting` alone is not a
  /// startup signal; `started_at` and terminal proc states are.
  pub fn has_started_work(&self) -> bool {
    self.procs.iter().any(|p| p.started_at.is_some() || !matches!(p.status, ProcStatus::Waiting))
  }

  pub(crate) fn liveness_deadline(&self) -> u64 {
    if self.has_started_work() {
      self.last_seen_at.saturating_add(SESSION_IDLE_TIMEOUT_SECS)
    } else {
      self.started_at.saturating_add(SESSION_START_TIMEOUT_SECS)
    }
  }

  /// A failed attempt is superseded when its explicit replacement has registered. Older
  /// persisted records have no lineage edge, so [`Self::proc_next_attempt`] retains the
  /// former same-route inference strictly as a compatibility fallback.
  pub(crate) fn proc_is_superseded(&self, proc: &ProcRecord) -> bool {
    self.proc_next_attempt(proc).is_some()
  }

  /// The proc that explicitly replaced this attempt, if any. Sessions persisted before
  /// `previous_attempt` use the earliest later proc of the same kind and skill name.
  pub(crate) fn proc_next_attempt(&self, proc: &ProcRecord) -> Option<&ProcRecord> {
    if let Some(next) = self.procs.iter().find(|candidate| candidate.previous_attempt == Some(proc.index)) {
      return Some(next);
    }
    if proc.previous_attempt.is_some() {
      return None;
    }
    let name = proc.skill_name.as_deref().filter(|n| !n.is_empty())?;
    self
      .procs
      .iter()
      .filter(|later| {
        later.previous_attempt.is_none()
          && later.index > proc.index
          && later.kind == proc.kind
          && later.skill_name.as_deref() == Some(name)
      })
      .min_by_key(|later| later.index)
  }

  /// The attempt immediately before this one. Explicit lineage is authoritative; the
  /// same-route lookup exists only for records persisted before lineage was stored.
  pub(crate) fn proc_previous_attempt(&self, proc: &ProcRecord) -> Option<&ProcRecord> {
    if let Some(index) = proc.previous_attempt {
      return self.procs.iter().find(|candidate| candidate.index == index);
    }
    if self.procs.iter().any(|candidate| candidate.previous_attempt == Some(proc.index)) {
      return None;
    }
    let name = proc.skill_name.as_deref().filter(|name| !name.is_empty())?;
    self
      .procs
      .iter()
      .filter(|earlier| {
        earlier.index < proc.index
          && earlier.kind == proc.kind
          && earlier.skill_name.as_deref() == Some(name)
          && proc.previous_attempt.is_none()
      })
      .max_by_key(|earlier| earlier.index)
  }

  /// The immutable first attempt in this proc's lineage.
  pub(crate) fn proc_first_attempt<'a>(&'a self, proc: &'a ProcRecord) -> &'a ProcRecord {
    let mut first = proc;
    let mut seen = std::collections::BTreeSet::from([first.index]);
    while let Some(previous) = self.proc_previous_attempt(first) {
      if !seen.insert(previous.index) {
        break;
      }
      first = previous;
    }
    first
  }

  /// (ordinal, total) attempts for this proc's route: (2, 2) is the retry of a route
  /// attempted twice. (1, 1) — the overwhelmingly common case — means no retries.
  pub(crate) fn proc_attempt(&self, proc: &ProcRecord) -> (usize, usize) {
    if proc.previous_attempt.is_some()
      || self.procs.iter().any(|candidate| candidate.previous_attempt == Some(proc.index))
    {
      let mut root = proc;
      let mut seen = std::collections::BTreeSet::new();
      seen.insert(root.index);
      while let Some(previous) =
        root.previous_attempt.and_then(|index| self.procs.iter().find(|candidate| candidate.index == index))
      {
        if !seen.insert(previous.index) {
          break;
        }
        root = previous;
      }
      let mut ordinal = 1;
      let mut total = 1;
      let mut current = root;
      let mut forward_seen = std::collections::BTreeSet::from([root.index]);
      while let Some(next) = self.procs.iter().find(|candidate| candidate.previous_attempt == Some(current.index)) {
        if !forward_seen.insert(next.index) {
          break;
        }
        total += 1;
        if next.index <= proc.index {
          ordinal += 1;
        }
        current = next;
      }
      return (ordinal, total);
    }
    let Some(name) = proc.skill_name.as_deref().filter(|n| !n.is_empty()) else {
      return (1, 1);
    };
    let mut ordinal = 0;
    let mut total = 0;
    for p in &self.procs {
      if p.kind == proc.kind && p.skill_name.as_deref() == Some(name) {
        total += 1;
        if p.index <= proc.index {
          ordinal += 1;
        }
      }
    }
    (ordinal.max(1), total.max(1))
  }

  pub fn lifecycle_status(&self, now: u64) -> SessionLifecycle {
    if self.ended_at.is_some() {
      if self.has_incomplete_procs() {
        return SessionLifecycle::Cancelled;
      }
      let failed: Vec<&ProcRecord> =
        self.procs.iter().filter(|p| p.status == ProcStatus::Fail && !self.proc_is_superseded(p)).collect();
      let interrupted = !failed.is_empty()
        && failed.iter().all(|p| {
          matches!(
            p.fail_reason.as_deref(),
            Some(
              crate::failure::reason::FORCE_STOPPED
                | crate::failure::reason::FORCE_RESTARTED
                | crate::failure::reason::SESSION_END_INCOMPLETE
            )
          )
        });
      if interrupted {
        return SessionLifecycle::Cancelled;
      }
      if !failed.is_empty() {
        return SessionLifecycle::Failed;
      }
      return SessionLifecycle::Completed;
    }
    if now > self.liveness_deadline() {
      return SessionLifecycle::Failed;
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
    if lifecycle == SessionLifecycle::Failed {
      return Some(self.liveness_deadline().saturating_sub(self.started_at));
    }
    None
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn test_proc(status: ProcStatus) -> ProcRecord {
    ProcRecord {
      index: 0,
      previous_attempt: None,
      label: "skill".into(),
      kind: ProcKind::Skill,
      status,
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
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
    }
  }

  #[test]
  fn lifecycle_uses_the_terminal_proc_status_for_ended_jobs() {
    let session = Session {
      id: "done".into(),
      started_at: 100,
      ended_at: Some(200),
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![ProcRecord {
        index: 0,
        previous_attempt: None,
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
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
      }],
      last_seen_at: 200,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    assert_eq!(session.lifecycle_status(200), SessionLifecycle::Completed);
    assert_eq!(session.duration_secs(200), Some(100));

    let mut invalid_result = session.clone();
    invalid_result.procs[0].status = ProcStatus::Fail;
    invalid_result.procs[0].fail_reason = Some(crate::failure::reason::RESULT_INVALID.into());
    assert_eq!(invalid_result.lifecycle_status(200), SessionLifecycle::Failed);
  }

  #[test]
  fn a_recovered_retry_supersedes_its_failed_attempt() {
    // A transient container failure gets retried as a NEW proc with the same skill
    // name; the newest attempt is the route's authoritative outcome. A job whose only
    // failure was retried into success is a success — not "Job failed" over a route
    // that visibly succeeded.
    let mut first = test_proc(ProcStatus::Fail);
    first.skill_name = Some("conventions-reviewer-claude".into());
    first.fail_reason = Some(crate::failure::reason::CONTAINER_TIMEOUT.into());
    let mut retry = test_proc(ProcStatus::Ok);
    retry.index = 1;
    retry.previous_attempt = Some(0);
    retry.skill_name = Some("conventions-reviewer-claude".into());
    let mut session = Session {
      id: "retry".into(),
      started_at: 100,
      ended_at: Some(200),
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![first, retry],
      last_seen_at: 200,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    assert_eq!(session.lifecycle_status(200), SessionLifecycle::Completed);
    // If the retry ALSO failed, the newest attempt is a real failure: job failed.
    session.procs[1].status = ProcStatus::Fail;
    session.procs[1].fail_reason = Some(crate::failure::reason::CONTAINER_TIMEOUT.into());
    assert_eq!(session.lifecycle_status(200), SessionLifecycle::Failed);
    // A failed proc with no skill name (nothing can re-run it) still fails the job.
    session.procs[1].status = ProcStatus::Ok;
    session.procs[1].fail_reason = None;
    session.procs[1].previous_attempt = None;
    session.procs[0].skill_name = None;
    assert_eq!(session.lifecycle_status(200), SessionLifecycle::Failed);
  }

  #[test]
  fn explicit_attempt_lineage_reaches_the_original_from_a_third_attempt() {
    let mut first = test_proc(ProcStatus::Fail);
    first.skill_name = Some("review".into());
    let mut second = test_proc(ProcStatus::Fail);
    second.index = 1;
    second.previous_attempt = Some(0);
    second.skill_name = Some("review".into());
    let mut third = test_proc(ProcStatus::Running);
    third.index = 2;
    third.previous_attempt = Some(1);
    third.skill_name = Some("review".into());
    let session = Session {
      id: "third".into(),
      started_at: 100,
      ended_at: None,
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![first, second, third],
      last_seen_at: 200,
      client_connected: true,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };

    assert_eq!(session.proc_attempt(&session.procs[0]), (1, 3));
    assert_eq!(session.proc_attempt(&session.procs[1]), (2, 3));
    assert_eq!(session.proc_attempt(&session.procs[2]), (3, 3));
    assert_eq!(session.proc_first_attempt(&session.procs[2]).index, 0);
  }

  #[test]
  fn lifecycle_fails_start_after_thirty_seconds_without_work() {
    let session = Session {
      id: "stale".into(),
      started_at: 100,
      ended_at: None,
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 100,
      client_connected: true,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    assert_eq!(session.lifecycle_status(100 + SESSION_START_TIMEOUT_SECS), SessionLifecycle::Running);
    assert_eq!(session.lifecycle_status(100 + SESSION_START_TIMEOUT_SECS + 1), SessionLifecycle::Failed);
    assert_eq!(session.duration_secs(100 + SESSION_START_TIMEOUT_SECS + 1), Some(SESSION_START_TIMEOUT_SECS));
  }

  #[test]
  fn lifecycle_allows_thirty_minutes_idle_after_work_starts() {
    let mut session = Session {
      id: "idle".into(),
      started_at: 100,
      ended_at: None,
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 150,
      client_connected: true,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    let mut proc = test_proc(ProcStatus::Running);
    proc.started_at = Some(110);
    session.procs.push(proc);
    assert_eq!(session.lifecycle_status(150 + SESSION_IDLE_TIMEOUT_SECS), SessionLifecycle::Running);
    assert_eq!(session.lifecycle_status(150 + SESSION_IDLE_TIMEOUT_SECS + 1), SessionLifecycle::Failed);
    assert_eq!(session.duration_secs(150 + SESSION_IDLE_TIMEOUT_SECS + 1), Some(50 + SESSION_IDLE_TIMEOUT_SECS));
  }

  #[test]
  fn lifecycle_cancelled_when_ended_with_incomplete_procs() {
    let session = Session {
      id: "cancel".into(),
      started_at: 1,
      ended_at: Some(50),
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![ProcRecord {
        index: 0,
        previous_attempt: None,
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
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
      }],
      last_seen_at: 50,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
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
      kind: None,
      repo: "/repo".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![
        ProcRecord {
          index: 0,
          previous_attempt: None,
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
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 1,
          previous_attempt: None,
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
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
        },
      ],
      last_seen_at: 1,
      client_connected: true,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    assert!(session.has_incomplete_procs());
    assert_eq!(session.lifecycle_status(2), SessionLifecycle::Running);
  }

  #[test]
  fn sessions_for_index_puts_running_first_then_recent() {
    let mut running = Session {
      id: "run".into(),
      started_at: 10,
      ended_at: None,
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 100,
      client_connected: true,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    let mut running_proc = test_proc(ProcStatus::Running);
    running_proc.started_at = Some(10);
    running.procs.push(running_proc);
    let done = Session {
      id: "done".into(),
      started_at: 200,
      ended_at: Some(250),
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 250,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    let mut sessions = BTreeMap::new();
    sessions.insert(done.id.clone(), done);
    sessions.insert(running.id.clone(), running);
    let mut annotation = sessions["done"].clone();
    annotation.id = "annotate".into();
    annotation.started_at = 300;
    annotation.parent_session = Some("done".into());
    sessions.insert(annotation.id.clone(), annotation);
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
          kind: None,
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: Vec::new(),
          last_seen_at: i as u64,
          client_connected: false,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
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
  fn startup_timed_out_client_not_counted_alive() {
    let now = 100;
    let mut store = Store::new(DaemonMode::Ephemeral, DEFAULT_PORT, now);
    store.insert_session(
      "stale".into(),
      Session {
        id: "stale".into(),
        started_at: now,
        ended_at: None,
        profile: None,
        kind: None,
        repo: "/r".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: now,
        client_connected: true,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    assert_eq!(store.alive_clients(now + SESSION_START_TIMEOUT_SECS), 1);
    assert_eq!(store.alive_clients(now + SESSION_START_TIMEOUT_SECS + 1), 0);
    store.reconcile(now + SESSION_START_TIMEOUT_SECS + 1);
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
  fn proc_kind_annotate_round_trips() {
    assert_eq!(ProcKind::Annotate.as_str(), "annotate");
    assert_eq!(ProcKind::parse("annotate"), Some(ProcKind::Annotate));
    assert_eq!(ProcKind::parse("build"), Some(ProcKind::Build));
    assert_eq!(ProcKind::parse("skill"), Some(ProcKind::Skill));
    assert_eq!(ProcKind::parse("other"), None);
  }

  #[test]
  fn persistent_never_auto_shutdown() {
    let store = Store::new(DaemonMode::Persistent, DEFAULT_PORT, 0);
    assert!(!store.should_shutdown_ephemeral(u64::MAX));
  }
}
