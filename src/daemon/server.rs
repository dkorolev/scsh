//! HTTP server for the session browser daemon.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::castprobe::{cast_probe_snapshot, probe_growth_messages, CastProbe};
use super::db::StoreDb;
use super::html;
use super::jsonio::{field_bool, field_num, field_str, tick_json, tick_json_light};
use super::model::{
  DaemonMode, OpenRepo, OutputLine, ProcKind, ProcRecord, ProcStatus, Session, SessionLifecycle, SkillMeta, Store,
};
use super::paths::{now_unix_secs, pid_file};
use super::prune::{schedule_from_api, schedule_orphans_from_session, PruneQueue};
use super::websocket::{self, Hub};
use crate::json::{parse, quote, Value};

const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);
const WS_TICK: Duration = Duration::from_millis(500);
const PRUNE_TICK: Duration = Duration::from_secs(30);
const MAX_PROC_LINES: usize = 5000;
const MAX_HTTP_BODY: usize = 512 * 1024;
/// Connections accepted per main-loop tick before yielding to housekeeping. Far above any
/// browser burst (each request is its own connection); only a flood ever hits it.
const ACCEPT_BURST_CAP: usize = 256;
const MAX_HTTP_HEADER: usize = 64 * 1024;

fn lock_store(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
  store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_last_persist(last: &Mutex<Option<Instant>>) -> MutexGuard<'_, Option<Instant>> {
  last.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct Server {
  store: Arc<Mutex<Store>>,
  prune: Arc<Mutex<PruneQueue>>,
  port: u16,
  dirty: Arc<AtomicBool>,
  ws_dirty: Arc<AtomicBool>,
  /// Session ids mutated since the last persist — the persist step writes only these.
  dirty_sessions: Arc<Mutex<HashSet<String>>>,
  /// The redb-backed session store. `None` when it could not be opened (persistence disabled,
  /// daemon still serves from memory) — best-effort, never fatal.
  db: Option<StoreDb>,
  last_persist: Mutex<Option<Instant>>,
  last_prune_tick: Mutex<Instant>,
  ws_hub: Arc<Hub>,
}

impl Server {
  pub fn new(mode: DaemonMode, port: u16) -> Server {
    let db = match StoreDb::open(port) {
      Ok(db) => Some(db),
      Err(e) => {
        eprintln!("scsh daemon: store DB unavailable ({e}); serving from memory without persistence");
        None
      }
    };
    Self::with_db(mode, port, db)
  }

  /// Build a server around an already-opened store DB. `new` resolves the DB from the port's
  /// `~/.scsh` path; tests pass an explicit temp-file DB so they touch neither the real home
  /// nor the process-global `SCSH_HOME`.
  fn with_db(mode: DaemonMode, port: u16, db: Option<StoreDb>) -> Server {
    let now = now_unix_secs();
    let mut store = Store::new(mode, port, now);
    if let Some(db) = &db {
      store.sessions = db.load_sessions();
    }
    // Reload keeps session history but starts the daemon's own runtime state fresh: no clients
    // are connected yet, and uptime restarts from now.
    store.mode = mode;
    store.port = port;
    store.started_at = now;
    store.active_clients = 0;
    store.last_activity = now;
    store.no_alive_since = Some(now);
    for session in store.sessions.values_mut() {
      session.client_connected = false;
    }
    // The previous daemon's wait/reconcile threads are gone. Any session whose recorded
    // run_pid is already dead will never get an exit callback — end it now so waiting
    // skills do not sit as "Ready" until the heartbeat stale window trips.
    for session in store.sessions.values_mut() {
      if session.ended_at.is_some() {
        settle_loaded_incomplete_procs(session);
        continue;
      }
      let dead = match session.run_pid {
        Some(pid) => !crate::daemon::paths::pid_alive(pid),
        None => false,
      };
      if dead {
        session.ended_at = Some(session.last_seen_at.max(session.started_at));
        session.run_pid = None;
        settle_loaded_incomplete_procs(session);
      }
    }
    Server {
      store: Arc::new(Mutex::new(store)),
      prune: Arc::new(Mutex::new(PruneQueue::load(port))),
      port,
      dirty: Arc::new(AtomicBool::new(false)),
      ws_dirty: Arc::new(AtomicBool::new(false)),
      dirty_sessions: Arc::new(Mutex::new(HashSet::new())),
      db,
      last_persist: Mutex::new(None),
      last_prune_tick: Mutex::new(Instant::now()),
      ws_hub: Hub::new(),
    }
  }

  pub fn run(&self) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::daemon::paths::daemon_dir())?;
    // Record this daemon's mode where the CLI can read it cross-process (redb is exclusive).
    crate::daemon::paths::write_mode_marker(self.port, lock_store(&self.store).mode);
    self.persist_now();
    {
      let now = now_unix_secs();
      let mut queue = self.prune.lock().unwrap_or_else(|e| e.into_inner());
      let _ = queue.tick(now);
      queue.save(self.port);
    }

    // Serve the local machine only. We bind every interface rather than just loopback so a
    // remote caller gets an explicit, readable denial (see [`peer_is_local`]) instead of a
    // silent connection refusal — every non-loopback peer is turned away in `handle_connection`
    // before its request is read or routed.
    let addr = format!("0.0.0.0:{}", self.port);
    let listener = TcpListener::bind(&addr)?;
    listener.set_nonblocking(true)?;
    let pid_path = pid_file(self.port);
    crate::atomic_write(&pid_path, std::process::id().to_string().as_bytes())?;

    let mut last_ws_tick = Instant::now();
    // Per-proc incremental cast probes (parse offsets cached across ticks) — see `castprobe`.
    let mut cast_probes: std::collections::HashMap<(String, usize), CastProbe> = std::collections::HashMap::new();
    // Zombie-container reaper state: the sweep runs on its own thread (container kills
    // sleep a second each), one pass at a time, tracking each unclaimed container's
    // consecutive-sweep count.
    let mut last_reap = Instant::now();
    let reap_running = Arc::new(AtomicBool::new(false));
    let reap_counts: Arc<Mutex<std::collections::HashMap<(String, String), u32>>> =
      Arc::new(Mutex::new(Default::default()));
    // Job supervisor: schedule restarts for failed supervised sessions and
    // fire due ones. Firing stops containers and spawns processes, so it runs on its own
    // thread, one pass at a time (same discipline as the reaper).
    let mut last_supervise = Instant::now();
    let supervise_running = Arc::new(AtomicBool::new(false));

    loop {
      // Drain the whole accept backlog every tick. A single accept per 100ms tick capped
      // the daemon at ten connections a second, so a job page's burst of fetches, or the
      // same page through an SSH tunnel, serialized at 100ms per request. Connections are
      // keep-alive (browsers and the CLI poster reuse them), but a fresh page load still
      // opens several at once. The cap only guards the housekeeping below from a
      // connection flood; a browser burst is drained in one pass.
      for _ in 0..ACCEPT_BURST_CAP {
        match listener.accept() {
          Ok((stream, _)) => {
            let store = Arc::clone(&self.store);
            let prune = Arc::clone(&self.prune);
            let dirty = Arc::clone(&self.dirty);
            let ws_dirty = Arc::clone(&self.ws_dirty);
            let dirty_sessions = Arc::clone(&self.dirty_sessions);
            let ws_hub = Arc::clone(&self.ws_hub);
            std::thread::spawn(move || {
              // Dirty flags are set inside the per-request loop: a keep-alive connection
              // outlives its mutations, which must be visible immediately, not at close.
              let _ = catch_unwind(AssertUnwindSafe(|| {
                let _ = handle_connection(stream, &store, &prune, &ws_hub, &ws_dirty, &dirty, &dirty_sessions);
              }));
            });
          }
          Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
          Err(e) => return Err(e),
        }
      }

      self.persist_if_due();
      self.prune_if_due();

      if last_reap.elapsed() >= Duration::from_secs(super::reap::REAP_INTERVAL_SECS) {
        last_reap = Instant::now();
        if !super::reap::reaping_disabled() && !reap_running.swap(true, Ordering::SeqCst) {
          let store = Arc::clone(&self.store);
          let prune = Arc::clone(&self.prune);
          let counts = Arc::clone(&reap_counts);
          let flag = Arc::clone(&reap_running);
          let port = self.port;
          std::thread::spawn(move || {
            let _ = catch_unwind(AssertUnwindSafe(|| {
              super::reap::reap_pass(&store, &prune, &counts, port, now_unix_secs());
            }));
            flag.store(false, Ordering::SeqCst);
          });
        }
      }

      if last_supervise.elapsed() >= Duration::from_secs(super::supervisor::SUPERVISOR_INTERVAL_SECS) {
        last_supervise = Instant::now();
        let now = now_unix_secs();
        let scheduled = {
          let mut store = lock_store(&self.store);
          super::supervisor::schedule_pass(&mut store, now)
        };
        let due = {
          let store = lock_store(&self.store);
          store.sessions.values().any(|s| {
            s.supervisor.supervised()
              && s.supervisor.gave_up.is_none()
              && s.supervisor.restarted_as.is_none()
              && s.supervisor.next_retry_at.is_some_and(|at| now >= at)
          })
        };
        if !scheduled.is_empty() {
          self.dirty.store(true, Ordering::Relaxed);
          self.ws_dirty.store(true, Ordering::Relaxed);
          self.dirty_sessions.lock().unwrap_or_else(|e| e.into_inner()).extend(scheduled);
        }
        if due && !supervise_running.swap(true, Ordering::SeqCst) {
          let store = Arc::clone(&self.store);
          let dirty = Arc::clone(&self.dirty);
          let ws_dirty = Arc::clone(&self.ws_dirty);
          let dirty_sessions = Arc::clone(&self.dirty_sessions);
          let flag = Arc::clone(&supervise_running);
          std::thread::spawn(move || {
            let touched = catch_unwind(AssertUnwindSafe(|| super::supervisor::fire_due(&store, now_unix_secs())))
              .unwrap_or_default();
            if !touched.is_empty() {
              dirty.store(true, Ordering::Relaxed);
              ws_dirty.store(true, Ordering::Relaxed);
              dirty_sessions.lock().unwrap_or_else(|e| e.into_inner()).extend(touched);
            }
            flag.store(false, Ordering::SeqCst);
          });
        }
      }

      if last_ws_tick.elapsed() >= WS_TICK {
        let now = now_unix_secs();
        let mut include_sessions = self.ws_dirty.load(Ordering::Relaxed);
        // The snapshot of casts to probe is taken under the store lock; the file stats and
        // tail-parses below run with the lock released, and only when someone is listening.
        let probe_casts = self.ws_hub.client_count() > 0;
        let (json, casts, dead_sessions) = {
          let mut store = lock_store(&self.store);
          let dead_sessions = settle_dead_run_pids(&mut store, now);
          include_sessions |= !dead_sessions.is_empty();
          store.reconcile(now);
          let json = if include_sessions { tick_json(&store, now) } else { tick_json_light(&store, now) };
          (json, if probe_casts { cast_probe_snapshot(&store) } else { Vec::new() }, dead_sessions)
        };
        if !dead_sessions.is_empty() {
          self.dirty.store(true, Ordering::Relaxed);
          self.dirty_sessions.lock().unwrap_or_else(|e| e.into_inner()).extend(dead_sessions);
        }
        self.ws_hub.broadcast_tick(&json);
        if probe_casts {
          for ((session, proc_index), msg) in probe_growth_messages(&casts, &mut cast_probes) {
            self.ws_hub.broadcast_growth(&session, proc_index, &msg);
          }
        }
        if include_sessions {
          self.ws_dirty.store(false, Ordering::Relaxed);
        }
        last_ws_tick = Instant::now();
      }

      let now = now_unix_secs();
      let shutdown = {
        let mut store = lock_store(&self.store);
        store.reconcile(now);
        store.should_shutdown_ephemeral(now)
      };

      if shutdown {
        self.persist_now();
        break;
      }
      std::thread::sleep(Duration::from_millis(100));
    }

    let _ = std::fs::remove_file(pid_path);
    Ok(())
  }

  fn persist_if_due(&self) {
    if !self.dirty.load(Ordering::Relaxed) {
      return;
    }
    let last = lock_last_persist(&self.last_persist);
    let due = match *last {
      None => true,
      Some(t) => t.elapsed() >= PERSIST_DEBOUNCE,
    };
    if due {
      drop(last);
      self.persist_now();
    }
  }

  /// Write through the sessions that changed since the last persist to the store DB, and let
  /// it drop any session no longer live (evicted past the cap). Only dirty sessions are
  /// serialized, and the DB I/O happens after the store lock is released — so a mutation-heavy
  /// run never re-serializes the whole store or holds the lock across disk I/O.
  fn persist_now(&self) {
    self.dirty.store(false, Ordering::Relaxed);
    *lock_last_persist(&self.last_persist) = Some(Instant::now());
    let Some(db) = &self.db else { return };
    let dirty_ids: Vec<String> = {
      let mut set = self.dirty_sessions.lock().unwrap_or_else(|e| e.into_inner());
      set.drain().collect()
    };
    // Snapshot (serialize) the dirty sessions and the full live-id set under the lock, then
    // release it before touching disk.
    let (dirty, keep) = {
      let store = lock_store(&self.store);
      let dirty: Vec<(String, String)> = dirty_ids
        .into_iter()
        .filter_map(|id| store.sessions.get(&id).map(|s| (id, crate::daemon::jsonio::session_json_store(s))))
        .collect();
      let keep: HashSet<String> = store.sessions.keys().cloned().collect();
      (dirty, keep)
    };
    if let Err(e) = db.sync(&dirty, &keep) {
      eprintln!("scsh daemon: store DB write failed: {e}");
    }
  }

  fn prune_if_due(&self) {
    let mut last = self.last_prune_tick.lock().unwrap_or_else(|e| e.into_inner());
    if last.elapsed() < PRUNE_TICK {
      return;
    }
    *last = Instant::now();
    drop(last);
    let now = now_unix_secs();
    let mut queue = self.prune.lock().unwrap_or_else(|e| e.into_inner());
    let _ = queue.tick(now);
    queue.save(self.port);
  }
}

/// A browser kill request (Force stop or Force restart) that was accepted but never
/// finalized, with the terminal reason/detail it settles to when the session ends first.
struct PendingBrowserKill {
  settled_reason: &'static str,
  settled_detail: &'static str,
}

fn pending_browser_kill(fail_reason: Option<&str>) -> Option<PendingBrowserKill> {
  match fail_reason {
    Some(crate::failure::reason::STOP_REQUESTED) => Some(PendingBrowserKill {
      settled_reason: crate::failure::reason::FORCE_STOPPED,
      settled_detail: "stopped from the session browser",
    }),
    Some(crate::failure::reason::RESTART_REQUESTED) => Some(PendingBrowserKill {
      settled_reason: crate::failure::reason::FORCE_RESTARTED,
      settled_detail: "restarted from the session browser",
    }),
    _ => None,
  }
}

/// Persisted sessions must never claim work is still running after the owning process or
/// session ended. An orphaned annotation row means its PROCESS vanished mid-work (killed,
/// crashed, host rebooted) — a real model timeout reports itself as `annotation_timed_out`
/// before the process exits — so it settles as `annotation_interrupted`; other proc kinds
/// retain the generic incomplete-session reason used by normal deregistration.
fn settle_loaded_incomplete_procs(session: &mut Session) {
  let ended = session.ended_at.unwrap_or(session.last_seen_at);
  for proc in &mut session.procs {
    if let Some(pending) = pending_browser_kill(proc.fail_reason.as_deref()) {
      proc.status = ProcStatus::Fail;
      proc.fail_reason = Some(pending.settled_reason.into());
      proc.detail = Some(pending.settled_detail.into());
      if proc.elapsed.is_none() {
        proc.elapsed = proc.started_at.map(|started| ended.saturating_sub(started) as f64);
      }
      continue;
    }
    if !matches!(proc.status, ProcStatus::Running | ProcStatus::Waiting) {
      continue;
    }
    proc.status = ProcStatus::Fail;
    if proc.kind == ProcKind::Annotate {
      proc.fail_reason = Some(crate::failure::reason::ANNOTATION_INTERRUPTED.into());
      proc.detail =
        Some("annotation process exited without reporting completion; the recording is unchanged and will be re-annotated on a later run".into());
    } else {
      proc.fail_reason = Some(crate::failure::reason::SESSION_END_INCOMPLETE.into());
      if proc.detail.is_none() {
        proc.detail = Some("session ended before this proc reported finish".into());
      }
    }
    if proc.elapsed.is_none() {
      proc.elapsed = proc.started_at.map(|started| ended.saturating_sub(started) as f64);
    }
  }
}

/// End sessions whose owning process disappeared without deregistering. The exact child
/// watcher covers jobs spawned by this daemon; this periodic check also covers CLI-started
/// runs and a watcher lost to an earlier daemon build. A dead PID is stronger evidence than
/// the heartbeat timeout, so the browser must not advertise the job as still running.
fn settle_dead_run_pids(store: &mut Store, now: u64) -> Vec<String> {
  let mut changed = Vec::new();
  for (id, session) in &mut store.sessions {
    if session.ended_at.is_some() {
      continue;
    }
    let Some(pid) = session.run_pid else { continue };
    if crate::daemon::paths::pid_alive(pid) {
      continue;
    }
    session.ended_at = Some(now);
    session.client_connected = false;
    session.run_pid = None;
    settle_loaded_incomplete_procs(session);
    changed.push(id.clone());
  }
  changed
}

/// Handle one request. Returns `(mutated, session_id)`: `mutated` drives the persist + WS
/// refresh, and `session_id` (extracted from a mutating POST body) is the one session to
/// write through to the store DB — so a mutation persists just that session, not the store.
/// Whether a connecting peer is on the local machine and may be served. Loopback covers both
/// families — `127.0.0.0/8` and IPv6 `::1` — so a browser reaching `localhost` over either is
/// served, while anything arriving over a routable interface is denied.
fn peer_is_local(peer: SocketAddr) -> bool {
  peer.ip().is_loopback()
}

/// How long a keep-alive connection may sit idle between requests before the daemon closes
/// it. Also bounds a slow request's header/body read. Browsers and the CLI poster reconnect
/// transparently after an idle close.
const KEEP_ALIVE_IDLE: Duration = Duration::from_secs(5);

/// True when the request asks the server to close after this response. HTTP/1.1 defaults to
/// keep-alive; per-connection sockets parked half the machine's ephemeral ports in TIME_WAIT
/// (a browser fetch burst, or a run's poster thread, each request its own connection).
fn connection_close(headers: &[(String, String)]) -> bool {
  // The Connection header is a comma-separated token list ("close, upgrade"); missing the
  // `close` token would leave an EOF-framed client hanging until the idle timeout.
  headers.iter().any(|(n, v)| {
    n.eq_ignore_ascii_case("connection") && v.split(',').any(|token| token.trim().eq_ignore_ascii_case("close"))
  })
}

fn handle_connection(
  mut stream: TcpStream, store: &Arc<Mutex<Store>>, prune: &Arc<Mutex<PruneQueue>>, ws_hub: &Arc<Hub>,
  ws_dirty: &AtomicBool, dirty: &AtomicBool, dirty_sessions: &Mutex<std::collections::HashSet<String>>,
) -> std::io::Result<()> {
  // Accepted sockets inherit the listener's non-blocking mode on macOS; block for reads.
  stream.set_nonblocking(false)?;
  // The daemon serves the local machine only. Turn away any non-loopback peer here — before
  // the request is read or routed — with a clear denial instead of the silent connection
  // refusal a loopback-only bind would give (see the listener bind for why we bind wider).
  match stream.peer_addr() {
    Ok(peer) if peer_is_local(peer) => {}
    peer => {
      let from = peer.map(|p| p.ip().to_string()).unwrap_or_else(|_| "an unknown address".to_string());
      let body = format!("scsh daemon serves the local machine only.\nThis request came from {from} and was denied.\n");
      write_response(&mut stream, 403, &body, "text/plain; charset=utf-8", true)?;
      return Ok(());
    }
  }
  stream.set_read_timeout(Some(KEEP_ALIVE_IDLE))?;
  // Requests ride one keep-alive connection until the client closes, asks to close, or the
  // idle timeout fires. Every response is Content-Length-framed, so the boundary is exact.
  loop {
    let req = match read_request(&mut stream) {
      Ok(req) => req,
      // Client close, idle timeout, or a malformed request all end the connection quietly.
      Err(_) => return Ok(()),
    };
    if websocket::wants_upgrade(&req.method, &req.path, &req.headers) {
      websocket::accept_handshake(&mut stream, &req.headers)?;
      let mailbox = ws_hub.subscribe();
      // A fresh client needs a full snapshot on its first tick — on a quiet daemon nothing
      // else would ever mark the store dirty, and the page would get light ticks forever.
      ws_dirty.store(true, Ordering::Relaxed);
      websocket::serve(stream, mailbox);
      return Ok(());
    }
    // HTTP/1.0 has no keep-alive by default and its clients may frame responses by EOF;
    // 1.1 defaults to keep-alive unless the request carries a `close` token.
    let close = req.http1_0 || connection_close(&req.headers);
    let bare_path = req.path.split('?').next().unwrap_or("");
    if req.method == "GET" && req.path.starts_with("/cast/") && bare_path.ends_with("/chapters") {
      let (status, body) = chapters_response(bare_path, store);
      write_response(&mut stream, status, &body, "application/json", close)?;
    } else if req.method == "GET" && req.path.starts_with("/cast/") && bare_path.ends_with("/export.html") {
      let (status, body, disposition) = export_response(bare_path, store);
      write_download_response(&mut stream, status, &body, "text/html; charset=utf-8", disposition.as_deref(), close)?;
    } else if req.method == "GET"
      && (req.path.starts_with("/job/") || req.path.starts_with("/session/"))
      && bare_path.ends_with("/export.html")
    {
      let (status, body, disposition) = session_export_response(bare_path, store);
      write_download_response(&mut stream, status, &body, "text/html; charset=utf-8", disposition.as_deref(), close)?;
    } else if req.method == "GET" && req.path.starts_with("/cast/") && !bare_path.ends_with("/play") {
      let (status, body, disposition) = cast_response(&req.path, store);
      write_download_response(
        &mut stream,
        status,
        &body,
        "application/x-asciicast; charset=utf-8",
        disposition.as_deref(),
        close,
      )?;
    } else if req.method == "GET" && req.path.starts_with("/diff/") {
      let (status, body, disposition) = diff_response(&req.path, store);
      write_download_response(&mut stream, status, &body, "text/html; charset=utf-8", disposition.as_deref(), close)?;
    } else {
      let (status, body, content_type, mutated) = route(&req, store, prune, ws_dirty);
      write_response(&mut stream, status, &body, content_type, close)?;
      if mutated {
        dirty.store(true, Ordering::Relaxed);
        ws_dirty.store(true, Ordering::Relaxed);
        if let Some(id) = mutated_session_id(&req) {
          dirty_sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
        }
      }
    }
    if close {
      return Ok(());
    }
  }
}

/// The `session` field of a mutating API POST body (all session-touching endpoints carry it),
/// so the persist step knows which session changed. `None` for mutations without one (e.g.
/// prune scheduling) — those write no session but still refresh the WS view.
fn mutated_session_id(req: &HttpRequest) -> Option<String> {
  if req.method != "POST" {
    return None;
  }
  match parse(&req.body).ok()? {
    Value::Object(o) => field_str(&o, "session").filter(|s| !s.is_empty()),
    _ => None,
  }
}

/// The `<session>/<proc>` tail of a `/cast/…` path, parsed. `None` on a malformed path.
fn parse_cast_route(rest: &str) -> Option<(&str, usize)> {
  let (session_id, proc_str) = rest.split_once('/')?;
  Some((session_id, proc_str.parse::<usize>().ok()?))
}

/// The registered cast path of a session's proc, shared by the cast, chapters, and export
/// endpoints. `None` covers unknown session/proc and a proc without a recording alike.
fn proc_cast_path(store: &Arc<Mutex<Store>>, session_id: &str, proc_index: usize) -> Option<String> {
  let store = lock_store(store);
  store.sessions.get(session_id)?.procs.iter().find(|p| p.index == proc_index).and_then(|p| p.cast_path.clone())
}

/// Read a cast file truncated to its last complete line (a cast still being written by a
/// live container stays a valid partial asciicast), or the 404 the caller serves as-is.
fn read_complete_cast_lines(cast_path: &str) -> Result<String, (u16, String)> {
  let Ok(bytes) = std::fs::read(cast_path) else {
    return Err((404, "cast file not available (not started yet, or pruned)".into()));
  };
  let end = bytes.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
  if end == 0 {
    return Err((404, "no recorded output yet".into()));
  }
  Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// `GET /cast/<session>/<proc>[?dl=1]` — the proc's asciinema recording. The file is read
/// at request time and truncated to its last complete line, so a cast still being written
/// by a live container downloads and replays as a valid (partial) asciicast. `dl=1` adds a
/// Content-Disposition attachment header for a browser "download" link.
fn cast_response(path_and_query: &str, store: &Arc<Mutex<Store>>) -> (u16, String, Option<String>) {
  let (path, query) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));
  let Some((session_id, proc_index)) = parse_cast_route(path.strip_prefix("/cast/").unwrap_or("")) else {
    return (404, "not found".into(), None);
  };
  let Some(cast_path) = proc_cast_path(store, session_id, proc_index) else {
    return (404, "no cast recorded for this proc".into(), None);
  };
  let body = match read_complete_cast_lines(&cast_path) {
    Ok(body) => body,
    Err((status, message)) => return (status, message, None),
  };
  let disposition = query
    .split('&')
    .any(|kv| kv == "dl=1")
    .then(|| format!("attachment; filename=\"scsh-{session_id}-p{proc_index}.cast\""));
  (200, body, disposition)
}

/// `GET /diff/<session>/<proc>[?dl=1]` — the packdiff-packed review page for the commits
/// this step brought into the caller's branch. The page is self-contained (CSS, the diff,
/// and the comment engine are all inside the one file), so without `dl=1` it renders
/// inline in a new tab; `dl=1` turns it into a download attachment.
fn diff_response(path_and_query: &str, store: &Arc<Mutex<Store>>) -> (u16, String, Option<String>) {
  let (path, query) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));
  if let Some(session_id) = path.strip_prefix("/diff/").and_then(|rest| rest.strip_suffix("/all")) {
    if session_id.is_empty() || session_id.contains('/') {
      return (404, "not found".into(), None);
    }
    let known = lock_store(store).sessions.contains_key(session_id);
    if !known {
      return (404, "not found".into(), None);
    }
    let path = crate::runtime::session_diffs_dir(session_id).join("job.html");
    let Ok(body) = std::fs::read_to_string(path) else {
      return (404, "whole-job commits diff is not available".into(), None);
    };
    let disposition = query
      .split('&')
      .any(|kv| kv == "dl=1")
      .then(|| format!("attachment; filename=\"scsh-job-{session_id}-diff.html\""));
    return (200, body, disposition);
  }
  let Some((session_id, proc_index)) = parse_cast_route(path.strip_prefix("/diff/").unwrap_or("")) else {
    return (404, "not found".into(), None);
  };
  let diff_path = {
    let store = lock_store(store);
    store
      .sessions
      .get(session_id)
      .and_then(|s| s.procs.iter().find(|p| p.index == proc_index))
      .and_then(|p| p.diff_path.clone())
  };
  let Some(diff_path) = diff_path else {
    return (404, "no commits diff packed for this step".into(), None);
  };
  let Ok(body) = std::fs::read_to_string(&diff_path) else {
    return (404, "diff page not available (pruned or moved)".into(), None);
  };
  let disposition = query
    .split('&')
    .any(|kv| kv == "dl=1")
    .then(|| format!("attachment; filename=\"scsh-job-{session_id}-p{proc_index}-diff.html\""));
  (200, body, disposition)
}

/// `GET /cast/<session>/<proc>/export.html` — the recording rendered into one
/// self-contained offline HTML player page: the exact rendering `scsh export-cast` does
/// ([`crate::export::render_page_from_texts`]), served as a download attachment named
/// `<cast stem>.html`. The chapters sidecar is folded in when present; an absent or
/// malformed sidecar exports without summary/chapters, never an error. A recording with no
/// complete frames yet is a 404 with an actionable body — the UI hides the button until
/// frames exist, so only a hand-typed URL sees it.
fn export_response(bare_path: &str, store: &Arc<Mutex<Store>>) -> (u16, String, Option<String>) {
  let rest = bare_path.strip_prefix("/cast/").unwrap_or("").strip_suffix("/export.html").unwrap_or("");
  let Some((session_id, proc_index)) = parse_cast_route(rest) else {
    return (404, "not found".into(), None);
  };
  let Some(cast_path) = proc_cast_path(store, session_id, proc_index) else {
    return (404, "no cast recorded for this proc".into(), None);
  };
  let ndjson = match read_complete_cast_lines(&cast_path) {
    Ok(ndjson) => ndjson,
    Err((status, message)) => return (status, message, None),
  };
  // The header alone is not exportable — the offline player errors on a zero-frame cast.
  if !ndjson.lines().any(|l| l.trim_start().starts_with('[')) {
    return (404, "no recorded frames yet — retry once the recording has output".into(), None);
  }
  let sidecar = chapters_sidecar_path(&cast_path).and_then(|p| std::fs::read_to_string(p).ok());
  let stem = crate::export::cast_stem(std::path::Path::new(&cast_path));
  match crate::export::render_page_from_texts(&ndjson, sidecar.as_deref(), &stem) {
    Ok(page) => (200, page, Some(format!("attachment; filename=\"{stem}.html\""))),
    Err(e) => (404, format!("cannot export this recording: {e}"), None),
  }
}

/// `GET /job/<id>/export.html` — EVERY recording of the job assembled into ONE
/// self-contained offline HTML page, served as a download attachment named
/// `scsh-job-<id>.html`. Each recording embeds as the exact per-cast export page
/// ([`crate::export::render_page_from_texts`]) in an attribute-escaped `<iframe srcdoc>`
/// — see [`html::session_export_page`] for the composition rationale. Procs with no cast
/// or no frames become note rows, never errors. `/session/…/export.html` remains accepted
/// as a compatibility alias.
fn session_export_response(bare_path: &str, store: &Arc<Mutex<Store>>) -> (u16, String, Option<String>) {
  let id = bare_path
    .strip_prefix("/job/")
    .or_else(|| bare_path.strip_prefix("/session/"))
    .unwrap_or("")
    .strip_suffix("/export.html")
    .unwrap_or("");
  // Clone the session under the lock, then do all file I/O (casts + sidecars) unlocked.
  let Some(session) = lock_store(store).sessions.get(id).cloned() else {
    return (404, "job not found".into(), None);
  };
  let exports: Vec<html::CastExport> = session.procs.iter().map(gather_proc_export).collect();
  // The snapshot freezes lifecycle, duration, and workflow-node states at this instant.
  let page = html::session_export_page(&session, &exports, now_unix_secs());
  (200, page, Some(format!("attachment; filename=\"scsh-job-{id}.html\"")))
}

/// One proc's contribution to the session export: the rendered per-cast page (frames on
/// disk → the same rendering `/cast/…/export.html` serves, sidecar summary alongside), or
/// the note explaining why there is nothing to embed. Never an error — a vanished file, a
/// frameless cast, and a proc that was never recorded all degrade to notes. When the proc
/// has a packed commits-diff on disk, its HTML rides along for offline review.
fn gather_proc_export(proc: &ProcRecord) -> html::CastExport {
  let diff_html = proc
    .diff_path
    .as_deref()
    .filter(|p| std::path::Path::new(p).is_file())
    .and_then(|p| std::fs::read_to_string(p).ok());
  const NO_RECORDING: &str = "no recording — skipped/failed before output";
  let Some(cast_path) = proc.cast_path.as_deref() else {
    return html::CastExport::Note { text: NO_RECORDING.into(), diff_html };
  };
  let Ok(ndjson) = read_complete_cast_lines(cast_path) else {
    return html::CastExport::Note { text: NO_RECORDING.into(), diff_html };
  };
  if !ndjson.lines().any(|l| l.trim_start().starts_with('[')) {
    return html::CastExport::Note { text: NO_RECORDING.into(), diff_html };
  }
  let sidecar = chapters_sidecar_path(cast_path).and_then(|p| std::fs::read_to_string(p).ok());
  let annotation = sidecar.as_deref().and_then(crate::annotate::parse_annotation);
  let (summary, chapters) = match annotation {
    Some(a) => (Some(a.summary), a.chapters.into_iter().map(|c| (c.t, c.title)).collect()),
    None => (None, Vec::new()),
  };
  html::CastExport::Cast { ndjson, summary, chapters, diff_html }
}

/// `GET /cast/<session>/<proc>/chapters` — the cast's analysis sidecar
/// (`{ "summary": …, "chapters": [{ "t", "title" }] }`), written next to the cast file by
/// the cursor/Composer analysis pass as `<cast-basename>.chapters.json`. While no sidecar
/// exists yet the body is `{}` — or `{ "summarizing_job": "<id>" }` when a live annotate
/// proc covering this cast is registered, so the page's "chapters: summarizing…" note can
/// deep-link to the job doing the work.
fn chapters_response(bare_path: &str, store: &Arc<Mutex<Store>>) -> (u16, String) {
  let rest = bare_path.strip_prefix("/cast/").unwrap_or("").strip_suffix("/chapters").unwrap_or("");
  let Some((session_id, proc_index)) = parse_cast_route(rest) else {
    return (404, "{}".into());
  };
  let Some(cast_path) = proc_cast_path(store, session_id, proc_index) else {
    return (200, "{}".into());
  };
  let annotation = annotation_for_cast(store, &cast_path);
  let relation = annotation.as_ref().map(|(job, proc, status)| {
    format!("\"annotation_job\": {}, \"annotation_proc\": {proc}, \"annotation_status\": {}", quote(job), quote(status))
  });
  match chapters_sidecar_path(&cast_path).and_then(|p| std::fs::read_to_string(p).ok()) {
    Some(json) => match relation {
      Some(fields) => {
        let trimmed = json.trim_end();
        match trimmed.strip_suffix('}') {
          Some(prefix) => (200, format!("{prefix}, {fields} }}")),
          None => (200, json),
        }
      }
      None => (200, json),
    },
    None => match relation {
      Some(fields) => (200, format!("{{ {fields} }}")),
      None => (200, "{}".into()),
    },
  }
}

/// The id of the job whose LIVE annotate proc is summarizing `cast_path` right now —
/// post-run annotation registers those procs on the same session, a standalone
/// `scsh annotate-cast` on its own `(internal)` one; either way the id is a job page the
/// pending note can link to. Paths are compared whole and by file name: the two sides may
/// spell the same file differently (relative CLI argument vs the absolute registered
/// path), and the nonce-stamped stem (`add-…-utc-ufakca.cast`) is unique per recording.
fn annotation_for_cast(store: &Arc<Mutex<Store>>, cast_path: &str) -> Option<(String, usize, &'static str)> {
  let file_name = std::path::Path::new(cast_path).file_name();
  let store = lock_store(store);
  let now = now_unix_secs();
  for (id, session) in store.sessions.iter() {
    for proc in &session.procs {
      if proc.kind != ProcKind::Annotate {
        continue;
      }
      let Some(target) = proc.annotate_target.as_deref() else {
        continue;
      };
      if target == cast_path || (file_name.is_some() && std::path::Path::new(target).file_name() == file_name) {
        let status = match proc.status {
          ProcStatus::Waiting | ProcStatus::Running
            if session.lifecycle_status(now) == crate::daemon::model::SessionLifecycle::Running =>
          {
            "running"
          }
          ProcStatus::Waiting | ProcStatus::Running => "fail",
          ProcStatus::Ok | ProcStatus::Graceful => "ok",
          ProcStatus::Fail | ProcStatus::Skipped => "fail",
        };
        return Some((id.clone(), proc.index, status));
      }
    }
  }
  None
}

/// The chapters-sidecar path for a cast file: `<dir>/<stem>.chapters.json`
/// (e.g. `…/foo.cast` → `…/foo.chapters.json`).
pub fn chapters_sidecar_path(cast_path: &str) -> Option<std::path::PathBuf> {
  let p = std::path::Path::new(cast_path);
  let stem = p.file_name()?.to_str()?.strip_suffix(".cast")?;
  Some(p.with_file_name(format!("{stem}.chapters.json")))
}

struct HttpRequest {
  method: String,
  path: String,
  body: String,
  headers: Vec<(String, String)>,
  /// The request line said `HTTP/1.0` — such clients predate keep-alive and may read the
  /// response to EOF, so the connection closes after answering them.
  http1_0: bool,
}

/// Read exactly one HTTP request. The connection is strictly ping-pong (request, response,
/// request, …) — clients here never pipeline, and any extra bytes a read happens to pull in
/// past the framed request would be discarded, not replayed to the next `read_request`.
fn read_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
  let mut buf = Vec::new();
  let mut chunk = [0u8; 4096];
  let mut header_end: Option<usize> = None;
  let mut content_length = 0usize;

  loop {
    if let Some(end) = header_end {
      let body_start = end + 4;
      if buf.len() >= body_start + content_length {
        break;
      }
    } else if let Some(end) = find_header_end(&buf) {
      header_end = Some(end);
      content_length = parse_content_length(&buf[..end]);
      if content_length > MAX_HTTP_BODY {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "request body too large"));
      }
      continue;
    }

    if buf.len() > MAX_HTTP_HEADER + content_length {
      break;
    }

    let n = stream.read(&mut chunk)?;
    if n == 0 {
      break;
    }
    buf.extend_from_slice(&chunk[..n]);
  }

  // A clean client close between keep-alive requests reads zero bytes; without this it
  // would parse as an empty `GET /` and the connection loop would serve a dead socket.
  if buf.is_empty() {
    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed"));
  }

  let header_end = header_end.unwrap_or(buf.len());
  let text = String::from_utf8_lossy(&buf[..header_end]);
  let mut lines = text.split("\r\n");
  let first = lines.next().unwrap_or("");
  let parts: Vec<&str> = first.split_whitespace().collect();
  let method = parts.first().unwrap_or(&"GET").to_string();
  let path = parts.get(1).unwrap_or(&"/").to_string();
  let http1_0 = parts.get(2).is_some_and(|v| v.eq_ignore_ascii_case("HTTP/1.0"));

  let mut headers = Vec::new();
  for line in lines {
    if line.is_empty() {
      break;
    }
    if let Some((name, value)) = line.split_once(':') {
      headers.push((name.trim().to_string(), value.trim().to_string()));
    }
  }

  let body_start = header_end + 4;
  let body = if body_start < buf.len() {
    let available = &buf[body_start..];
    if content_length > 0 {
      String::from_utf8_lossy(&available[..available.len().min(content_length)]).into_owned()
    } else {
      String::from_utf8_lossy(available).into_owned()
    }
  } else {
    String::new()
  };
  Ok(HttpRequest { method, path, body, headers, http1_0 })
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
  buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(header_bytes: &[u8]) -> usize {
  let text = String::from_utf8_lossy(header_bytes);
  for line in text.split("\r\n").skip(1) {
    let Some((name, value)) = line.split_once(':') else {
      continue;
    };
    if name.eq_ignore_ascii_case("Content-Length") {
      return value.trim().parse().unwrap_or(0);
    }
  }
  0
}

fn route(
  req: &HttpRequest, store: &Arc<Mutex<Store>>, prune: &Arc<Mutex<PruneQueue>>, ws_dirty: &AtomicBool,
) -> (u16, String, &'static str, bool) {
  // The images-build endpoint returns a custom body (the spawned session id), so it does not
  // go through the generic `{"ok":…}` POST handler.
  if req.method == "POST" && req.path == "/api/v1/images/build" {
    let (status, body, mutated) = images_build_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/setup/tests" {
    let (status, body, mutated) = setup_tests_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  // The "open a repository" + "start a job" endpoints return custom bodies (validation result,
  // discovered definitions, the spawned session id), so they bypass the generic POST handler.
  if req.method == "POST" && req.path == "/api/v1/repos/open" {
    let (status, body, mutated) = repos_open_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/projects/create" {
    let (status, body, mutated) = projects_create_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/harness-defs" {
    let (status, body) = harness_defs_response(&req.body);
    return (status, body, "application/json", false);
  }
  if req.method == "POST" && req.path == "/api/v1/jobs/start" {
    let (status, body, mutated) = jobs_start_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/jobs/restart" {
    let (status, body, mutated) = jobs_restart_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/session/stop" {
    let (status, body, mutated) = session_stop_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/proc/stop" {
    let (status, body, mutated) =
      proc_stop_response_notifying(&req.body, store, || ws_dirty.store(true, Ordering::Relaxed));
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/proc/restart" {
    let (status, body, mutated) =
      proc_restart_response_notifying(&req.body, store, || ws_dirty.store(true, Ordering::Relaxed));
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/harness/stop" {
    let (status, body, mutated) =
      harness_stop_response_notifying(&req.body, store, || ws_dirty.store(true, Ordering::Relaxed));
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path == "/api/v1/repos/pick" {
    return (200, repos_pick_response(), "application/json", false);
  }
  if req.method == "POST" && req.path.starts_with("/api/v1/") {
    let ok = handle_api_post(&req.path, &req.body, store, prune);
    let body = if ok { "{\"ok\":true}" } else { "{\"ok\":false}" };
    return (if ok { 200 } else { 400 }, body.to_string(), "application/json", ok);
  }
  if req.method != "GET" {
    return (405, "method not allowed".into(), "text/plain", false);
  }
  match req.path.split('?').next().unwrap_or(req.path.as_str()) {
    "/" => {
      let html = html::index_page(&lock_store(store));
      (200, html, "text/html; charset=utf-8", false)
    }
    path @ ("/run" | "/jobs" | "/projects" | "/stats" | "/setup" | "/images") => {
      let tab = html::IndexTab::from_path(path).unwrap_or(html::IndexTab::Run);
      let html = html::index_page_for(&lock_store(store), None, tab);
      (200, html, "text/html; charset=utf-8", false)
    }
    path if path.starts_with("/project") || path.starts_with("/repo") => {
      // Filtered Projects view. Extra slashes are normalized by parse_index_filter.
      // Bare `/project` or `/repo` (no name/path) falls through to the unfiltered Projects tab.
      let filter = html::parse_index_filter(path);
      let html = if filter.is_some() {
        html::index_page_with_filter(&lock_store(store), filter)
      } else {
        html::index_page_for(&lock_store(store), None, html::IndexTab::Projects)
      };
      (200, html, "text/html; charset=utf-8", false)
    }
    path if path.starts_with("/job/") || path.starts_with("/session/") => {
      // Canonical page URL is `/job/<id>`; `/session/<id>` is kept as a compatibility alias.
      let id = path.strip_prefix("/job/").or_else(|| path.strip_prefix("/session/")).unwrap_or("");
      let store = lock_store(store);
      if let Some(page) = html::session_page(&store, id) {
        (200, page, "text/html; charset=utf-8", false)
      } else {
        (404, "job not found".into(), "text/plain", false)
      }
    }
    "/assets/scsh-cast-player.js" => (200, html::PLAYER_JS.to_string(), "application/javascript; charset=utf-8", false),
    "/assets/scsh-cast-player.css" => (200, html::PLAYER_CSS.to_string(), "text/css; charset=utf-8", false),
    path if path.starts_with("/cast/") && path.ends_with("/play") => {
      let rest = path.strip_prefix("/cast/").unwrap_or("").strip_suffix("/play").unwrap_or("");
      let page = rest.split_once('/').and_then(|(sid, proc)| {
        let proc_index = proc.parse::<usize>().ok()?;
        html::cast_player_page(&lock_store(store), sid, proc_index)
      });
      match page {
        Some(page) => (200, page, "text/html; charset=utf-8", false),
        None => (404, "cast not found".into(), "text/plain", false),
      }
    }
    "/api/v1/sessions" => {
      let store = lock_store(store);
      let ids: Vec<String> = store.sessions.keys().cloned().collect();
      let parts: Vec<String> = ids.iter().map(|id| quote(id)).collect();
      (200, format!("{{ \"sessions\": [{}] }}", parts.join(", ")), "application/json", false)
    }
    // The running daemon's own version — so `scsh daemon status` can report what is
    // actually serving (which may lag the installed CLI until a restart), not just the
    // caller's version.
    "/api/v1/version" => {
      // `commit` is the bare stamp (null when unknown); `version` keeps the human line
      // (`1.30.3 (d5e2270)`) existing callers parse.
      let commit = crate::version::git_stamp();
      let commit_json = if commit.is_empty() { "null".to_string() } else { quote(&commit) };
      (
        200,
        format!("{{ \"version\": {}, \"commit\": {commit_json} }}", quote(&crate::version::display())),
        "application/json",
        false,
      )
    }
    // The match scrutinee is the query-stripped path, so query params must be read
    // from `req.path` — a `path.split_once("runtime=")` here can never match.
    "/api/v1/images" => {
      let runtime = req.path.split_once("runtime=").map(|(_, v)| v.split('&').next().unwrap_or(v));
      (200, images_json(runtime), "application/json", false)
    }
    "/api/v1/setup" => {
      let runtime = req.path.split_once("runtime=").map(|(_, v)| v.split('&').next().unwrap_or(v));
      (200, super::setup::setup_json(runtime), "application/json", false)
    }
    "/api/v1/repos" => (200, repos_json(&lock_store(store), now_unix_secs()), "application/json", false),
    // Flaky-route dashboard data: reliability + latency percentiles per route and per
    // skill × route, aggregated from the durable stats file (no store state involved).
    "/api/v1/stats" => (200, html::stats_json(), "application/json", false),
    // Fleet aggregation for scripts and reduce steps: per-skill rollups (the same shape
    // as the end-of-run `<skill>-rollup.json` files) plus the job-level verdict, computed
    // live so it also serves mid-run.
    path if path.starts_with("/api/v1/session/") && path.ends_with("/fleet") => {
      let id = path.strip_prefix("/api/v1/session/").unwrap_or("").strip_suffix("/fleet").unwrap_or("");
      let store = lock_store(store);
      if let Some(s) = store.sessions.get(id) {
        (200, crate::fleet::fleet_json(&s.id, &s.procs), "application/json", false)
      } else {
        (404, "{\"error\":\"not found\"}".into(), "application/json", false)
      }
    }
    path if path.starts_with("/api/v1/session/") => {
      let id = path.strip_prefix("/api/v1/session/").unwrap_or("");
      let store = lock_store(store);
      if let Some(s) = store.sessions.get(id) {
        (200, crate::daemon::jsonio::session_json_api(s), "application/json", false)
      } else {
        (404, "{\"error\":\"not found\"}".into(), "application/json", false)
      }
    }
    _ => (404, "not found".into(), "text/plain", false),
  }
}

fn handle_api_post(path: &str, body: &str, store: &Arc<Mutex<Store>>, prune: &Arc<Mutex<PruneQueue>>) -> bool {
  let obj = match parse(body).ok() {
    Some(Value::Object(o)) => o,
    _ => return false,
  };
  let now = now_unix_secs();

  if path == "/api/v1/prune/schedule" {
    let port = lock_store(store).port;
    let mut queue = prune.lock().unwrap_or_else(|e| e.into_inner());
    let ok = schedule_from_api(body, &mut queue, now);
    if ok {
      queue.save(port);
    }
    return ok;
  }

  // Forced janitor pass (`scsh prune --now`): delete every eligible run dir immediately.
  if path == "/api/v1/prune/tick" {
    let port = lock_store(store).port;
    let mut queue = prune.lock().unwrap_or_else(|e| e.into_inner());
    let _ = queue.tick(now);
    queue.save(port);
    return true;
  }

  let mut store = lock_store(store);
  store.touch(now);
  let port = store.port;
  let mut orphan_containers: Vec<(String, String)> = Vec::new();

  let ok = match path {
    "/api/v1/session/start" => {
      let id = field_str(&obj, "session").unwrap_or_default();
      let repo = display_or_absolute_repo(&field_str(&obj, "repo").unwrap_or_default());
      let branch = field_str(&obj, "branch").unwrap_or_default();
      let profile = field_str(&obj, "profile");
      let kind = field_str(&obj, "kind");
      let skills = parse_skills_array(&obj);
      let run_pid = field_num(&obj, "run_pid").and_then(|n| if n > 0.0 { Some(n as u32) } else { None });
      let workflow =
        crate::daemon::workflow::parse_workflow_value(obj.iter().find(|(k, _)| k == "workflow").map(|(_, v)| v));
      let parent_session = field_str(&obj, "parent_session");
      let retries = field_num(&obj, "retries").map(|n| n as u32);
      if id.is_empty() {
        return false;
      }
      if let Some(s) = store.session_mut(&id) {
        s.ended_at = None;
        s.last_seen_at = now;
        if !repo.is_empty() {
          s.repo = repo;
        }
        if !branch.is_empty() {
          s.branch = branch;
        }
        s.profile = profile;
        s.kind = kind;
        if !skills.is_empty() {
          s.skills = skills;
        }
        if run_pid.is_some() {
          s.run_pid = run_pid;
        }
        if workflow.is_some() {
          s.workflow = workflow;
        }
        if parent_session.is_some() {
          s.parent_session = parent_session;
        }
        // An explicit CLI --retries wins; a client that omits the field never demotes
        // daemon-set state (jobs/start budgets, restart-chain inheritance).
        if let Some(n) = retries {
          s.supervisor.retries = n;
          s.supervisor.job_attempt = s.supervisor.job_attempt.max(1);
        }
        return true;
      }
      let session = Session {
        id: id.clone(),
        started_at: now,
        ended_at: None,
        profile,
        kind,
        repo,
        branch,
        skills,
        procs: Vec::new(),
        last_seen_at: now,
        client_connected: false,
        run_pid,
        workflow,
        parent_session,
        supervisor: crate::daemon::model::SupervisorState::fresh(
          retries.unwrap_or(crate::daemon::model::DEFAULT_JOB_RETRIES),
        ),
      };
      store.insert_session(id, session);
      true
    }
    "/api/v1/register" => {
      store.active_clients += 1;
      let session_id = field_str(&obj, "session").unwrap_or_default();
      if let Some(s) = store.session_mut(&session_id) {
        s.client_connected = true;
        s.last_seen_at = now;
      }
      true
    }
    "/api/v1/deregister" => {
      store.active_clients = store.active_clients.saturating_sub(1);
      let session_id = field_str(&obj, "session").unwrap_or_default();
      if !session_id.is_empty() {
        if let Some(s) = store.session_mut(&session_id) {
          orphan_containers = s
            .procs
            .iter()
            .filter_map(|p| {
              p.container_name.as_ref().map(|n| (n.clone(), p.container_runtime.clone().unwrap_or_default()))
            })
            .collect();
          s.client_connected = false;
          s.last_seen_at = now;
          if s.ended_at.is_none() {
            s.ended_at = Some(now);
            s.run_pid = None;
            for p in &mut s.procs {
              if let Some(pending) = pending_browser_kill(p.fail_reason.as_deref()) {
                p.status = ProcStatus::Fail;
                p.fail_reason = Some(pending.settled_reason.into());
                p.detail = Some(pending.settled_detail.into());
                continue;
              }
              if p.status == ProcStatus::Running || p.status == ProcStatus::Waiting {
                p.status = ProcStatus::Fail;
                p.fail_reason = Some(crate::failure::reason::SESSION_END_INCOMPLETE.into());
                if p.detail.is_none() {
                  p.detail = Some(deregister_incomplete_detail(p));
                }
                crate::failure::log_session_proc(
                  &session_id,
                  crate::failure::reason::SESSION_END_INCOMPLETE,
                  &p.label,
                  p.detail.as_deref().unwrap_or(""),
                );
              }
            }
          }
        }
      }
      true
    }
    "/api/v1/ping" => {
      let session_id = field_str(&obj, "session").unwrap_or_default();
      if let Some(s) = store.session_mut(&session_id) {
        s.last_seen_at = now;
      }
      true
    }
    "/api/v1/proc/add" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let label = field_str(&obj, "label").unwrap_or_default();
      let kind = ProcKind::parse(field_str(&obj, "kind").as_deref().unwrap_or("skill")).unwrap_or(ProcKind::Skill);
      let s = match store.session_mut(&session) {
        Some(s) => s,
        None => return false,
      };
      let skill_name = field_str(&obj, "skill_name");
      let harness = field_str(&obj, "harness");
      let model = field_str(&obj, "model");
      let skill_source = field_str(&obj, "skill_source");
      let route = field_str(&obj, "route");
      let annotate_target = field_str(&obj, "annotate_target");
      let previous_attempt = field_num(&obj, "previous_attempt").map(|value| value as usize);
      if let Some(previous) = previous_attempt {
        let valid_predecessor = previous < proc_index
          && s
            .procs
            .iter()
            .any(|p| p.index == previous && p.kind == kind && p.skill_name.as_deref() == skill_name.as_deref());
        let child_available = !s.procs.iter().any(|p| p.index != proc_index && p.previous_attempt == Some(previous));
        if !valid_predecessor || !child_available {
          return false;
        }
      }
      if let Some(p) = s.procs.iter_mut().find(|p| p.index == proc_index) {
        p.previous_attempt = previous_attempt;
        p.label = label;
        p.kind = kind;
        p.skill_name = skill_name.clone();
        p.harness = harness;
        p.model = model;
        p.skill_source = skill_source;
        p.route = route;
        p.annotate_target = annotate_target;
      } else {
        s.procs.push(ProcRecord {
          index: proc_index,
          previous_attempt,
          label,
          kind,
          status: ProcStatus::Waiting,
          skill_name: skill_name.clone(),
          harness,
          model,
          started_at: None,
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source,
          route,
          result_path: None,
          annotate_target,
          lines: Vec::new(),
        });
      }
      if let Some(previous) = previous_attempt {
        if let Some(p) = s.procs.iter_mut().find(|p| p.index == previous) {
          if p.fail_reason.as_deref() == Some(crate::failure::reason::RESTART_REQUESTED) {
            p.status = ProcStatus::Fail;
            p.fail_reason = Some(crate::failure::reason::FORCE_RESTARTED.into());
            p.detail = Some("restarted from the session browser; superseded by the linked attempt".into());
          }
        }
      }
      if let (Some(meta), Some(step_id)) = (s.workflow.as_mut(), skill_name.as_deref()) {
        crate::daemon::workflow::bind_workflow_proc(meta, step_id, proc_index, kind);
      }
      true
    }
    "/api/v1/proc/start" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      if let Some(p) = store.proc_mut(&session, proc_index) {
        p.status = ProcStatus::Running;
        if p.started_at.is_none() {
          p.started_at = Some(now);
        }
        true
      } else {
        false
      }
    }
    "/api/v1/proc/note" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let note = field_str(&obj, "note").unwrap_or_default();
      if let Some(p) = store.proc_mut(&session, proc_index) {
        p.note = Some(note);
        true
      } else {
        false
      }
    }
    "/api/v1/proc/cast" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let path = field_str(&obj, "path").unwrap_or_default();
      if let Some(p) = store.proc_mut(&session, proc_index) {
        p.cast_path = if path.is_empty() { None } else { Some(path.clone()) };
        if matches!(
          p.fail_reason.as_deref(),
          Some(crate::failure::reason::FORCE_STOPPED) | Some(crate::failure::reason::FORCE_RESTARTED)
        ) && !path.is_empty()
        {
          crate::annotate::suppress_automatic_annotation(std::path::Path::new(&path));
        }
        true
      } else {
        false
      }
    }
    "/api/v1/proc/diff" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let path = field_str(&obj, "path").unwrap_or_default();
      if let Some(p) = store.proc_mut(&session, proc_index) {
        p.diff_path = if path.is_empty() { None } else { Some(path) };
        true
      } else {
        false
      }
    }
    "/api/v1/proc/result" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let path = field_str(&obj, "path").unwrap_or_default();
      if let Some(p) = store.proc_mut(&session, proc_index) {
        p.result_path = if path.is_empty() { None } else { Some(path) };
        true
      } else {
        false
      }
    }
    "/api/v1/proc/line" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let at = field_num(&obj, "at").unwrap_or(0.0);
      let line = field_str(&obj, "line").unwrap_or_default();
      if let Some(p) = store.proc_mut(&session, proc_index) {
        push_proc_lines(p, &[(at, line)]);
        true
      } else {
        false
      }
    }
    "/api/v1/proc/lines" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let lines = parse_lines_field(&obj);
      if lines.is_empty() {
        return false;
      }
      if let Some(p) = store.proc_mut(&session, proc_index) {
        push_proc_lines(p, &lines);
        true
      } else {
        false
      }
    }
    "/api/v1/proc/finish" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let status =
        ProcStatus::parse(field_str(&obj, "status").as_deref().unwrap_or("fail")).unwrap_or(ProcStatus::Fail);
      let detail = field_str(&obj, "detail");
      let fail_reason = field_str(&obj, "fail_reason");
      let elapsed = field_num(&obj, "elapsed");
      if let Some(p) = store.proc_mut(&session, proc_index) {
        // A failed finish from the container being torn down must not overwrite an accepted
        // browser restart. A valid result that won the race is different: the runner consumes
        // the marker without respawning, so that successful attempt remains authoritative.
        let restart_won_by_result = p.fail_reason.as_deref() == Some(crate::failure::reason::RESTART_REQUESTED)
          && matches!(status, ProcStatus::Ok | ProcStatus::Graceful);
        if matches!(
          p.fail_reason.as_deref(),
          Some(crate::failure::reason::STOP_REQUESTED)
            | Some(crate::failure::reason::FORCE_STOPPED)
            | Some(crate::failure::reason::RESTART_REQUESTED)
            | Some(crate::failure::reason::FORCE_RESTARTED)
        ) && !restart_won_by_result
        {
          return true;
        }
        p.status = status;
        p.detail = detail;
        p.fail_reason = fail_reason;
        p.elapsed = elapsed;
        true
      } else {
        false
      }
    }
    "/api/v1/container" => {
      let session = field_str(&obj, "session").unwrap_or_default();
      touch_session_liveness(&mut store, &session, now);
      let proc_index = field_num(&obj, "proc").unwrap_or(0.0) as usize;
      let action = field_str(&obj, "action").unwrap_or_default();
      let name = field_str(&obj, "name").unwrap_or_default();
      let runtime = field_str(&obj, "runtime");
      if let Some(p) = store.proc_mut(&session, proc_index) {
        if action == "start" {
          p.container_name = Some(name);
          p.container_runtime = runtime;
        } else if action == "stop" {
          p.container_name = None;
        }
        true
      } else {
        false
      }
    }
    _ => false,
  };

  drop(store);
  if !orphan_containers.is_empty() {
    let mut queue = prune.lock().unwrap_or_else(|e| e.into_inner());
    schedule_orphans_from_session(&mut queue, &orphan_containers, now);
    queue.save(port);
  }
  ok
}

/// Canonicalize path-like repo values; pass display labels (empty, or non-path strings such as
/// `build-images`' "(image builds)" and annotate's "(internal)") through untouched. Clients
/// already absolutize real paths; the server-side pass is only the defensive second
/// canonicalization for those.
fn display_or_absolute_repo(repo: &str) -> String {
  if repo.starts_with('/') {
    super::paths::absolutize_repo_path(std::path::Path::new(repo))
  } else {
    repo.to_string()
  }
}

/// `GET /api/v1/images` — status of every scsh image (base + one per harness) on the detected
/// container runtime, for the dashboard's images panel. No runtime degrades to an `error` field
/// rather than an HTTP failure, so the panel can render the reason.
fn images_json(runtime_override: Option<&str>) -> String {
  // Apple `container` and docker/podman are SEPARATE worlds with separate image stores; the
  // browser picks which to inspect/build. Only installed runtimes are offered (Apple
  // `container` never appears off macOS).
  let available = crate::runtime::available_runtimes();
  let rt_name: String = match runtime_override.filter(|r| !r.is_empty()) {
    Some(rt) if available.contains(&rt) => rt.to_string(),
    Some(rt) => {
      return format!(
        "{{ \"error\": {} }}",
        quote(&format!("runtime '{rt}' is not installed (available: {})", available.join(", ")))
      )
    }
    None => match crate::runtime::detect_runtime() {
      Some(rt) => rt.name,
      None => return r#"{ "error": "no container runtime found (docker, podman, or Apple container)" }"#.to_string(),
    },
  };
  let available_json: Vec<String> = available.iter().map(|r| quote(r)).collect();
  let rows: Vec<String> = crate::runtime::image_statuses(&rt_name)
    .iter()
    .map(|s| {
      format!(
        "{{ \"name\": {}, \"tag\": {}, \"exists\": {}, \"up_to_date\": {}, \"created\": {}, \"size\": {} }}",
        quote(&s.name),
        quote(&s.tag),
        s.exists,
        s.up_to_date,
        s.created.as_deref().map(quote).unwrap_or_else(|| "null".into()),
        s.size.as_deref().map(quote).unwrap_or_else(|| "null".into()),
      )
    })
    .collect();
  format!(
    "{{ \"runtime\": {}, \"available\": [{}], \"images\": [{}] }}",
    quote(&rt_name),
    available_json.join(", "),
    rows.join(", ")
  )
}

/// `POST /api/v1/images/build` — body `{"harnesses": [name…], "rebuild_base": bool, "force":
/// bool}` (all fields optional; no harnesses = all). Spawns a detached `scsh build-images
/// --session <id>` and pre-creates that session in the store, so the returned
/// `{"ok":true,"session":id}` deep-links to a live page before the child registers. One image
/// build at a time; a concurrent request gets 409. Stderr is piped and the session is
/// reconciled on exit (same fate-binding as `jobs/start`), so a silent startup failure never
/// leaves a stranded "running" build.
fn images_build_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => Vec::new(), // an empty/absent body means "build everything with defaults"
  };
  let harnesses = parse_string_array(&obj, "harnesses");
  for h in &harnesses {
    // `base` is a first-class image name — `["base"]` builds only the shared base.
    if h != "base" && crate::config::Harness::parse(h).is_none() {
      let msg = format!("unknown image '{h}' (known: base, {})", crate::config::Harness::known().join(", "));
      return (400, format!("{{\"ok\":false,\"error\":{}}}", quote(&msg)), false);
    }
  }
  let rebuild_base = field_bool(&obj, "rebuild_base").unwrap_or(false);
  let force = field_bool(&obj, "force").unwrap_or(false);
  // Optional runtime override, limited to what is actually installed on this host.
  let runtime = field_str(&obj, "runtime").filter(|r| !r.is_empty());
  if let Some(rt) = runtime.as_deref() {
    if !crate::runtime::available_runtimes().contains(&rt) {
      let msg =
        format!("runtime '{rt}' is not installed (available: {})", crate::runtime::available_runtimes().join(", "));
      return (400, format!("{{\"ok\":false,\"error\":{}}}", quote(&msg)), false);
    }
  }
  let now = now_unix_secs();
  let port = {
    let store = lock_store(store);
    let build_running = store.sessions.values().any(|s| {
      s.profile.as_deref() == Some(BUILD_IMAGES_PROFILE) && s.lifecycle_status(now) == SessionLifecycle::Running
    });
    if build_running {
      return (409, "{\"ok\":false,\"error\":\"an image build is already running\"}".to_string(), false);
    }
    store.port
  };
  let exe = match super::client::scsh_executable() {
    Ok(exe) => exe,
    Err(e) => {
      let msg = format!("cannot locate the scsh binary to spawn: {e}");
      return (500, format!("{{\"ok\":false,\"error\":{}}}", quote(&msg)), false);
    }
  };
  let session_id = crate::runtime::random_nonce_6();
  let mut cmd = std::process::Command::new(exe);
  cmd.arg("build-images");
  cmd.args(&harnesses);
  if force {
    cmd.arg("--force");
  }
  if rebuild_base {
    cmd.arg("--rebuild-base");
  }
  cmd.args(["--session", &session_id]);
  if let Some(rt) = runtime.as_deref() {
    cmd.env("SCSH_RUNTIME", rt);
  }
  cmd.env(super::paths::PORT_ENV, port.to_string());
  cmd.env("NO_COLOR", "1"); // plain stderr, so a captured startup failure reads cleanly
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::piped()); // captured, so a failure before registration is not silent
  match cmd.spawn() {
    Ok(mut child) => {
      let run_pid = Some(child.id());
      // Same fate-binding as jobs/start: drain stderr, wait, reconcile — so a build that dies
      // before it registers becomes a *failed* session (with the error), never a hidden "running".
      let store_reap = Arc::clone(store);
      let sid = session_id.clone();
      let stderr = child.stderr.take();
      std::thread::spawn(move || {
        let mut tail = String::new();
        if let Some(mut e) = stderr {
          let _ = e.read_to_string(&mut tail);
        }
        let code = child.wait().ok().and_then(|s| s.code());
        reconcile_finished_job(&store_reap, &sid, code, &tail);
      });
      let mut store = lock_store(store);
      store.touch(now);
      store.insert_session(
        session_id.clone(),
        Session {
          id: session_id.clone(),
          started_at: now,
          ended_at: None,
          profile: Some(BUILD_IMAGES_PROFILE.to_string()),
          kind: None,
          repo: IMAGE_BUILDS_REPO.to_string(),
          branch: String::new(),
          skills: Vec::new(),
          procs: Vec::new(),
          last_seen_at: now,
          client_connected: false,
          run_pid,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
      (200, format!("{{\"ok\":true,\"session\":{}}}", quote(&session_id)), true)
    }
    Err(e) => {
      let msg = format!("failed to spawn scsh build-images: {e}");
      (500, format!("{{\"ok\":false,\"error\":{}}}", quote(&msg)), false)
    }
  }
}

/// The `profile` label `scsh build-images` registers its sessions under; the build guard and
/// the spawn above must agree on it.
const BUILD_IMAGES_PROFILE: &str = "build-images";

/// The synthetic `repo` label image-build sessions carry, so they never appear as a real
/// repository in the jobs-per-directory view or block a repo's one-job guard.
pub(crate) const IMAGE_BUILDS_REPO: &str = "(image builds)";

/// Synthetic `repo` for standalone annotate catch-up sessions (Projects → Internal). Same
/// one-job-per-repo exemption as [`IMAGE_BUILDS_REPO`]: the label never matches a real path.
pub(crate) const INTERNAL_REPO: &str = "(internal)";

// ---------------------------------------------------------------------------
// Harness definitions: open a repo, list its definitions, start a job in it.
// ---------------------------------------------------------------------------

/// A `{"ok":false,"error":…}` body for a client-side problem.
fn err_body(msg: &str) -> String {
  format!("{{\"ok\":false,\"error\":{}}}", quote(msg))
}

/// `POST /api/v1/repos/open` — body `{"path":"…"}`. Validate the path is a git repo, report
/// whether it is clean, discover the harness definitions available to it, and remember it as an
/// open repo. `{ok:true,repo,clean,dirty:[…],defs:[…]}`, or `{ok:false,error}` (still HTTP 200
/// for a "not a repo" so the UI can show the message inline).
fn repos_open_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object with a 'path'"), false),
  };
  let path = match field_str(&obj, "path") {
    Some(p) if !p.trim().is_empty() => p.trim().to_string(),
    _ => return (400, err_body("give a repository path"), false),
  };
  // A bare (slash-free) name is a PROJECT: it resolves under `$SCSH_HOME/projects/`, where
  // the "New project" flow scaffolds fresh repos — so the open box takes either form.
  let abs = if !path.contains('/') {
    super::paths::projects_dir().join(&path).to_string_lossy().into_owned()
  } else {
    super::paths::absolutize_repo_path(std::path::Path::new(&path))
  };
  open_validated_repo(&abs, store, false)
}

/// Validate `abs` as a runnable repo, record it as opened, and answer the browser with its
/// blockers + discovered definitions. Shared by "open" and "create project" (which differ only
/// in how the path came to exist). `created` is echoed so the UI can word its note.
fn open_validated_repo(abs: &str, store: &Arc<Mutex<Store>>, created: bool) -> (u16, String, bool) {
  let Some(root) = crate::git_root_of(std::path::Path::new(abs)) else {
    return (200, err_body(&format!("not a git repository: {abs}")), false);
  };
  // A repo is runnable only if the run itself would accept it (committed, clean, gitignored
  // scratch) — the UI uses this to disable Start and say why, so no doomed job is ever started.
  let blockers = crate::def_run_blockers(&root);
  let runnable = blockers.is_empty();
  let clean = !blockers.iter().any(|b| b.contains("uncommitted"));
  let discovery = crate::harness_def::discover(&root);
  let repo = root.to_string_lossy().into_owned();
  let now = now_unix_secs();
  {
    let mut s = lock_store(store);
    s.touch(now);
    s.open_repo(OpenRepo { path: repo.clone(), opened_at: now, clean: runnable });
  }
  let blockers_arr: Vec<String> = blockers.iter().map(|b| quote(b)).collect();
  let body = format!(
    "{{\"ok\":true,\"created\":{created},\"repo\":{},\"runnable\":{},\"clean\":{},\"blockers\":[{}],\"defs\":[{}],\"global\":[{}]}}",
    quote(&repo),
    runnable,
    clean,
    blockers_arr.join(","),
    defs_json(&discovery.defs).join(","),
    global_profiles_json().join(",")
  );
  (200, body, true)
}

/// `POST /api/v1/projects/create` — body `{"name":"…"}`. Scaffold a fresh PROJECT at
/// `$SCSH_HOME/projects/<name>`: a new git repository whose FIRST commit gitignores `/tmp`
/// (plus the physical `tmp/` dir), i.e. born runnable — so tests and demos start from the
/// web UI with no terminal at all. The reply is the same shape as `repos/open` (the project
/// is opened immediately), with `"created":true`. An existing name is opened in place
/// (`"created":false`, HTTP 200) — create-or-open, not a conflict.
fn projects_create_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object with a 'name'"), false),
  };
  let name = match field_str(&obj, "name") {
    Some(n) if !n.trim().is_empty() => n.trim().to_string(),
    _ => return (400, err_body("give a project name"), false),
  };
  let valid = name.len() <= 64
    && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
  if !valid {
    return (
      400,
      err_body("project names are 1–64 chars: letters, digits, '-', '_' (no dots or slashes; start with alphanumeric)"),
      false,
    );
  }
  let projects = super::paths::projects_dir();
  if let Err(e) = std::fs::create_dir_all(&projects) {
    return (500, err_body(&format!("could not create {}: {e}", projects.display())), false);
  }
  let path = projects.join(&name);
  if path.exists() {
    // Already there — open it (idempotent create). No 409: the browser treats non-2xx as
    // console noise even when the app handled the conflict.
    return open_validated_repo(&path.to_string_lossy(), store, false);
  }
  if let Err(e) = scaffold_project(&path) {
    let _ = std::fs::remove_dir_all(&path); // leave nothing half-made; a retry starts clean
    return (500, err_body(&e), false);
  }
  open_validated_repo(&path.to_string_lossy(), store, true)
}

/// Create the project directory as a git repo whose first commit is the gitignored-`/tmp`
/// contract every `scsh run` preflight requires (committed, clean, scratch ignored).
fn scaffold_project(path: &std::path::Path) -> Result<(), String> {
  let git = |args: &[&str]| -> Result<(), String> {
    let out = crate::git_command()
      .arg("-C")
      .arg(path)
      .args(args)
      .output()
      .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
    if out.status.success() {
      Ok(())
    } else {
      Err(format!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
    }
  };
  std::fs::create_dir(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
  git(&["init", "-q"])?;
  std::fs::write(path.join(".gitignore"), "# scsh scratch — results, logs, cache. Never tracked.\n/tmp\n")
    .map_err(|e| format!("could not write .gitignore: {e}"))?;
  std::fs::create_dir_all(path.join("tmp")).map_err(|e| format!("could not create tmp/: {e}"))?;
  git(&["add", ".gitignore"])?;
  git(&[
    "-c",
    &format!("user.name={}", crate::SCSH_COMMIT_NAME),
    "-c",
    &format!("user.email={}", crate::SCSH_COMMIT_EMAIL),
    "commit",
    "-qm",
    "Init.",
  ])
}

/// `POST /api/v1/repos/pick` — pop the host's native folder chooser (the daemon is local) and
/// return the chosen absolute path. `{ok:true,path}`, `{ok:false,cancelled:true}`, or
/// `{ok:false,error}` when no picker is available (the browser then falls back to typing a path).
fn repos_pick_response() -> String {
  match pick_directory() {
    Ok(Some(path)) => format!("{{\"ok\":true,\"path\":{}}}", quote(&path)),
    Ok(None) => "{\"ok\":false,\"cancelled\":true}".to_string(),
    Err(e) => err_body(&e),
  }
}

/// Pop the native OS directory chooser and return the chosen absolute path (`None` on cancel).
/// macOS uses AppleScript; Linux uses zenity or kdialog. Requires a display — headless daemons
/// get an error and the UI falls back to a typed path.
fn pick_directory() -> Result<Option<String>, String> {
  let picked = |out: std::process::Output| -> Option<String> {
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|p| !p.is_empty())
  };
  if cfg!(target_os = "macos") {
    let out = std::process::Command::new("osascript")
      .args(["-e", "POSIX path of (choose folder with prompt \"Pick a repository for scsh\")"])
      .output()
      .map_err(|e| format!("could not run osascript: {e}"))?;
    if out.status.success() {
      return Ok(picked(out));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    // -128 / "User canceled" is a normal cancel, not an error.
    if err.contains("User canceled") || err.contains("-128") {
      return Ok(None);
    }
    return Err(format!("folder picker failed: {}", err.trim()));
  }
  if crate::runtime::which("zenity").is_some() {
    let out = std::process::Command::new("zenity")
      .args(["--file-selection", "--directory", "--title=Pick a repository for scsh"])
      .output()
      .map_err(|e| format!("could not run zenity: {e}"))?;
    return Ok(picked(out)); // non-zero = cancel
  }
  if crate::runtime::which("kdialog").is_some() {
    let out = std::process::Command::new("kdialog")
      .args(["--getexistingdirectory", "."])
      .output()
      .map_err(|e| format!("could not run kdialog: {e}"))?;
    return Ok(picked(out));
  }
  Err("no folder picker on this host (install zenity or kdialog) — type or paste the path instead".into())
}

/// `POST /api/v1/harness-defs` — body `{"repo":"…"}`. Re-discover the definitions for an
/// already-open repo (a refresh). `{defs:[…]}`.
fn harness_defs_response(body: &str) -> (u16, String) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object with a 'repo'")),
  };
  let repo = match field_str(&obj, "repo") {
    Some(r) if !r.trim().is_empty() => r.trim().to_string(),
    _ => return (400, err_body("give a repository path")),
  };
  let discovery = crate::harness_def::discover(std::path::Path::new(&repo));
  (
    200,
    format!(
      "{{\"defs\":[{}],\"global\":[{}]}}",
      defs_json(&discovery.defs).join(","),
      global_profiles_json().join(",")
    ),
  )
}

/// `POST /api/v1/setup/tests` — body `{"runtime":"…","tests":[{"harness":"…","model":"…"}]}`.
/// Writes a batch harness def under `~/.scsh/projects/setup-tests` and spawns `scsh run`
/// (same credential forwarding / scrubbing as a normal job). Returns `{ok,session}`.
fn setup_tests_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object"), false),
  };
  let tests = match super::setup::parse_setup_tests(&obj) {
    Ok(t) => t,
    Err(e) => return (400, err_body(&e), false),
  };
  let runtime = field_str(&obj, "runtime").filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string());
  if let Some(ref rt) = runtime {
    let available = crate::runtime::available_runtimes();
    if !available.contains(&rt.as_str()) {
      return (400, err_body(&format!("runtime '{rt}' is not installed (available: {})", available.join(", "))), false);
    }
  }

  let root = match super::setup::prepare_setup_batch(&tests) {
    Ok(p) => p,
    Err(e) => return (500, err_body(&e), false),
  };
  let repo = root.to_string_lossy().into_owned();
  let def_name = super::setup::setup_batch_def_name().to_string();

  let now = now_unix_secs();
  let port = {
    let store = lock_store(store);
    if store.job_running_in(&repo, now) {
      return (409, err_body("a setup test is already running — open that job or wait for it to finish"), false);
    }
    store.port
  };

  let discovery = crate::harness_def::discover(&root);
  let Some(def) = discovery.find(&def_name) else {
    return (500, err_body("smoketest definition was not discovered after write"), false);
  };
  let planned = planned_skills(def, &def_name);

  let exe = match super::client::scsh_executable() {
    Ok(exe) => exe,
    Err(e) => return (500, err_body(&format!("cannot locate the scsh binary to spawn: {e}")), false),
  };
  let branch = crate::current_branch(&root);
  let session_id = crate::runtime::random_nonce_6();
  let mut cmd = std::process::Command::new(exe);
  cmd.arg("run").args(["--def", &def_name]);
  cmd.current_dir(&root);
  cmd.args(["--session", &session_id]);
  cmd.env(super::paths::PORT_ENV, port.to_string());
  cmd.env("NO_COLOR", "1");
  if let Some(rt) = runtime {
    cmd.env("SCSH_RUNTIME", rt);
  }
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::piped());
  match cmd.spawn() {
    Ok(mut child) => {
      let run_pid = Some(child.id());
      let store_reap = Arc::clone(store);
      let sid = session_id.clone();
      let stderr = child.stderr.take();
      std::thread::spawn(move || {
        let mut tail = String::new();
        if let Some(mut e) = stderr {
          let _ = e.read_to_string(&mut tail);
        }
        let code = child.wait().ok().and_then(|s| s.code());
        reconcile_finished_job(&store_reap, &sid, code, &tail);
      });
      let mut store = lock_store(store);
      store.touch(now);
      let workflow = crate::daemon::workflow::workflow_meta_from_def(def);
      let kind = if def.is_workflow() { Some("workflow".into()) } else { None };
      store.insert_session(
        session_id.clone(),
        Session {
          id: session_id.clone(),
          started_at: now,
          ended_at: None,
          profile: Some(def_name.clone()),
          kind,
          repo: repo.clone(),
          branch,
          skills: planned,
          procs: Vec::new(),
          last_seen_at: now,
          client_connected: false,
          run_pid,
          workflow,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
      (200, format!("{{\"ok\":true,\"session\":{}}}", quote(&session_id)), true)
    }
    Err(e) => (500, err_body(&format!("failed to spawn setup test: {e}")), false),
  }
}

/// `POST /api/v1/jobs/start` — body `{"repo":"…","def":"…","params":{…}}` for a harness
/// definition, or `{"repo":"…","profile":"…"}` for a named skill profile (the repo's own, or
/// one installed machine-wide via `scsh installskills --global`). Enforce one job per repo,
/// validate what will run, then spawn `scsh run --def <name>` / `scsh run <profile>` in the
/// repo with the params as environment and the pre-created session id. `{ok:true,session}`,
/// or 409/400.
fn jobs_start_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object"), false),
  };
  let def_name = field_str(&obj, "def").map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
  let profile_name = field_str(&obj, "profile").map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
  let repo_in = match field_str(&obj, "repo") {
    Some(r) if !r.trim().is_empty() => r.trim().to_string(),
    _ => return (400, err_body("give a repository path"), false),
  };
  let retries = field_num(&obj, "retries").map(|n| n as u32).unwrap_or(crate::daemon::model::DEFAULT_JOB_RETRIES);
  start_job_in_repo(&repo_in, def_name, profile_name, read_params(&obj), None, retries, store)
}

/// A validated, ready-to-spawn job: the repo root, what to run, and the planned tasks.
struct PlannedJob {
  root: std::path::PathBuf,
  run_name: String,
  planned: Vec<SkillMeta>,
  kind: Option<String>,
  workflow: Option<super::workflow::WorkflowMeta>,
  run_args: Vec<String>,
}

/// Everything `jobs/start` checks before touching anything: the path is a runnable git repo
/// (committed, clean, gitignored scratch — the SAME checks the run itself makes), and the
/// def/profile + params name a startable job. Split out so `jobs/restart` can refuse a
/// doomed restart BEFORE it stops the old run.
fn plan_job_request(
  repo_in: &str, def_name: &Option<String>, profile_name: &Option<String>, params: &[(String, String)],
) -> Result<PlannedJob, (u16, String, bool)> {
  let run_name = match (def_name, profile_name) {
    (Some(d), None) => d.clone(),
    (None, Some(p)) => p.clone(),
    _ => return Err((400, err_body("give a definition name or a profile name (exactly one)"), false)),
  };
  let Some(root) = crate::git_root_of(std::path::Path::new(&repo_in)) else {
    return Err((400, err_body(&format!("not a git repository: {repo_in}")), false));
  };
  let blockers = crate::def_run_blockers(&root);
  if !blockers.is_empty() {
    return Err((400, err_body(&format!("repository not ready: {}", blockers.join("; "))), false));
  }

  // Validate what will run, and pre-plan its tasks — pre-populated on the session so its
  // page shows them immediately (no blank "limbo" while the spawned run starts up and
  // registers).
  let (planned, kind, workflow, run_args) = if let Some(profile) = profile_name {
    if profile == "default" {
      return Err((
        400,
        err_body("the default profile is always the repo's own — start a NAMED profile, or a definition"),
        false,
      ));
    }
    let routes = profile_routes_for(&root, profile);
    if routes.is_empty() {
      return Err((
        400,
        err_body(&format!(
          "no skill profile named '{profile}' — neither this repo's .scsh.yml nor the global manifest (scsh installskills --global) declares it"
        )),
        false,
      ));
    }
    let planned: Vec<SkillMeta> =
      routes.iter().map(|r| SkillMeta { name: r.name.clone(), harness: r.harness.as_str().to_string() }).collect();
    (planned, None, None, vec!["run".to_string(), profile.clone()])
  } else {
    let discovery = crate::harness_def::discover(&root);
    let Some(def) = discovery.find(&run_name) else {
      return Err((400, err_body(&format!("no harness definition named '{run_name}'")), false));
    };
    if let Err(msg) = validate_job_params(def, params) {
      return Err((400, err_body(&msg), false));
    }
    let workflow = crate::daemon::workflow::workflow_meta_from_def(def);
    let kind = if def.is_workflow() { Some("workflow".to_string()) } else { None };
    (planned_skills(def, &run_name), kind, workflow, vec!["run".to_string(), "--def".to_string(), run_name.clone()])
  };
  Ok(PlannedJob { root, run_name, planned, kind, workflow, run_args })
}

/// The shared start path of `jobs/start` and `jobs/restart`: validate the repo, plan the
/// tasks, enforce one job per repo, spawn the run, pre-create its session, and persist the
/// start recipe so the job can later be force-restarted.
fn start_job_in_repo(
  repo_in: &str, def_name: Option<String>, profile_name: Option<String>, params: Vec<(String, String)>,
  resume_from: Option<String>, retries: u32, store: &Arc<Mutex<Store>>,
) -> (u16, String, bool) {
  let PlannedJob { root, run_name, planned, kind, workflow, run_args } =
    match plan_job_request(repo_in, &def_name, &profile_name, &params) {
      Ok(p) => p,
      Err(e) => return e,
    };
  let repo = root.to_string_lossy().into_owned();

  // One job per directory.
  let now = now_unix_secs();
  let port = {
    let store = lock_store(store);
    if store.job_running_in(&repo, now) {
      return (409, err_body("a job is already running in this repository"), false);
    }
    store.port
  };

  let exe = match super::client::scsh_executable() {
    Ok(exe) => exe,
    Err(e) => return (500, err_body(&format!("cannot locate the scsh binary to spawn: {e}")), false),
  };
  let branch = crate::current_branch(&root);
  let session_id = crate::runtime::random_nonce_6();
  let mut cmd = std::process::Command::new(exe);
  cmd.args(&run_args);
  cmd.current_dir(&root);
  for (k, v) in &params {
    cmd.env(k, v);
  }
  if let Some(old) = &resume_from {
    cmd.args(["--resume-from", old]);
  }
  cmd.args(["--session", &session_id]);
  cmd.env(super::paths::PORT_ENV, port.to_string());
  cmd.env("NO_COLOR", "1"); // plain stderr, so a captured error reads cleanly on the session page
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::piped()); // captured, so a failure before registration is not silent
  match cmd.spawn() {
    Ok(mut child) => {
      let run_pid = Some(child.id());
      // Bind the session's fate to the process: drain its stderr, wait, then reconcile — so a job
      // that dies before it ever registers becomes a *failed* session (with the error), never a
      // hidden "running" one.
      let store_reap = Arc::clone(store);
      let sid = session_id.clone();
      let stderr = child.stderr.take();
      std::thread::spawn(move || {
        let mut tail = String::new();
        if let Some(mut e) = stderr {
          let _ = e.read_to_string(&mut tail);
        }
        let code = child.wait().ok().and_then(|s| s.code());
        reconcile_finished_job(&store_reap, &sid, code, &tail);
      });
      write_start_recipe(&session_id, def_name.as_deref(), profile_name.as_deref(), &params);
      let mut store = lock_store(store);
      store.touch(now);
      store.insert_session(
        session_id.clone(),
        Session {
          id: session_id.clone(),
          started_at: now,
          ended_at: None,
          profile: Some(run_name.clone()),
          kind,
          repo: repo.clone(),
          branch,
          skills: planned,
          procs: Vec::new(),
          last_seen_at: now,
          client_connected: false,
          run_pid,
          workflow,
          parent_session: None,
          supervisor: crate::daemon::model::SupervisorState::fresh(retries),
        },
      );
      (200, format!("{{\"ok\":true,\"session\":{}}}", quote(&session_id)), true)
    }
    Err(e) => (500, err_body(&format!("failed to spawn scsh run: {e}")), false),
  }
}

/// When a spawned job's process exits, reconcile its session so no job is ever left hidden: a run
/// that deregistered normally already has an end time and is left alone; one that died without
/// finishing is ended, and — if it never produced a single proc (a refusal/crash before it
/// registered) — the captured error is surfaced as a failed row instead of a silent "running".
fn reconcile_finished_job(store: &Arc<Mutex<Store>>, session_id: &str, code: Option<i32>, stderr_tail: &str) {
  let now = now_unix_secs();
  let mut store = lock_store(store);
  let Some(s) = store.sessions.get_mut(session_id) else { return };
  if s.ended_at.is_some() {
    return; // finished and deregistered normally
  }
  s.ended_at = Some(now);
  s.client_connected = false;
  s.run_pid = None;
  if s.procs.is_empty() {
    let label =
      if s.profile.as_deref() == Some(BUILD_IMAGES_PROFILE) { "build failed to start" } else { "run failed to start" };
    s.procs.push(ProcRecord {
      index: 0,
      previous_attempt: None,
      label: label.into(),
      kind: ProcKind::Skill,
      status: ProcStatus::Fail,
      skill_name: None,
      harness: None,
      model: None,
      started_at: Some(now),
      note: None,
      detail: Some(startup_error_detail(stderr_tail, code)),
      fail_reason: Some("startup_failed".into()),
      elapsed: Some(0.0),
      lines: Vec::new(),
      container_name: None,
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
    });
  }
}

/// The tail of a failed run's stderr as a human detail (with an exit-code fallback if it was silent).
fn startup_error_detail(stderr_tail: &str, code: Option<i32>) -> String {
  let lines: Vec<&str> = stderr_tail.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
  if lines.is_empty() {
    return match code {
      Some(c) => format!("the run exited with status {c} before starting (no output)"),
      None => "the run was killed before starting (no output)".into(),
    };
  }
  let start = lines.len().saturating_sub(6);
  lines[start..].join("\n")
}

/// `GET /api/v1/repos` — the opened repositories and any repos that have jobs, each with its
/// jobs (sessions) grouped underneath. Powers the browser's jobs-per-directory view.
fn repos_json(store: &Store, now: u64) -> String {
  let mut paths: std::collections::BTreeSet<String> = store.open_repos.keys().cloned().collect();
  for s in store.sessions.values() {
    if s.repo != IMAGE_BUILDS_REPO && s.repo != INTERNAL_REPO {
      paths.insert(s.repo.clone());
    }
  }
  let repos: Vec<String> = paths
    .iter()
    .map(|path| {
      let jobs: Vec<String> = store
        .sessions
        .values()
        .filter(|s| &s.repo == path)
        .map(|s| {
          format!(
            "{{\"session\":{},\"profile\":{},\"status\":{},\"started_at\":{}}}",
            quote(&s.id),
            s.profile.as_deref().map(quote).unwrap_or_else(|| "null".into()),
            quote(s.lifecycle_status(now).label()),
            s.started_at
          )
        })
        .collect();
      let clean = store.open_repos.get(path).map(|r| r.clean.to_string()).unwrap_or_else(|| "null".into());
      format!("{{\"path\":{},\"clean\":{},\"jobs\":[{}]}}", quote(path), clean, jobs.join(","))
    })
    .collect();
  format!("{{\"repos\":[{}]}}", repos.join(","))
}

/// `POST /api/v1/jobs/restart` — body `{"session":"…","mode":"resume"|"scratch"}`.
/// Force-restart a job: stop the old run first (containers killed, incomplete procs failed
/// `force_stopped` — exactly Force stop), then start the SAME job fresh in the same
/// repository: from the session's persisted start recipe when it has one (params included),
/// else from its stored def/profile name alone (older CLI runs — their env params were never
/// persisted, so a definition whose required params have no defaults refuses with the missing
/// param). `mode:"resume"` (workflow jobs only) passes `--resume-from <old id>` so the fresh
/// run restores every step the old one completed and runs only the rest; the default
/// (`"scratch"` or absent) runs everything anew. Answers `{ok:true,session:"<new id>"}` — the
/// NEW job's id.
pub(crate) fn jobs_restart_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object"), false),
  };
  let session_id = match field_str(&obj, "session") {
    Some(s) if !s.trim().is_empty() => s.trim().to_string(),
    _ => return (400, err_body("give a session id"), false),
  };
  let resume = match field_str(&obj, "mode").as_deref() {
    Some("resume") => true,
    None | Some("scratch") => false,
    Some(other) => return (400, err_body(&format!("unknown restart mode '{other}' (resume or scratch)")), false),
  };
  let (repo, name, retries) = {
    let store = lock_store(store);
    let Some(s) = store.sessions.get(&session_id) else {
      return (404, err_body("session not found"), false);
    };
    if s.repo == IMAGE_BUILDS_REPO || s.repo == INTERNAL_REPO || s.parent_session.is_some() {
      return (400, err_body("only repository jobs can be restarted"), false);
    }
    let Some(name) = s.profile.clone() else {
      return (400, err_body("this job has no definition or profile to restart"), false);
    };
    if resume && s.kind.as_deref() != Some("workflow") {
      return (
        400,
        err_body("resume applies to workflow jobs — this job's routes are independent, restart it from scratch"),
        false,
      );
    }
    (s.repo.clone(), name, s.supervisor.retries)
  };
  // The recipe: what jobs/start recorded, or — for a CLI-started run — the name alone,
  // classified def-vs-profile with the same precedence a fresh start would use.
  let (def, profile, params) = read_start_recipe(&session_id).unwrap_or_else(|| {
    let is_def = crate::git_root_of(std::path::Path::new(&repo))
      .map(|root| crate::harness_def::discover(&root).find(&name).is_some())
      .unwrap_or(false);
    if is_def {
      (Some(name.clone()), None, Vec::new())
    } else {
      (None, Some(name.clone()), Vec::new())
    }
  });
  // Refuse a doomed restart BEFORE stopping anything: if the respawn cannot start (repo
  // dirty, unmet required param, vanished def), the old run — stuck or not — is left alone.
  if let Err(e) = plan_job_request(&repo, &def, &profile, &params) {
    return e;
  }
  if resume && def.is_none() {
    return (400, err_body("resume applies to workflow definition jobs — restart this job from scratch"), false);
  }
  // Stop the old run before spawning, so the one-job-per-repo guard sees the repo free.
  // Idempotent: restarting an already-ended job just starts it again.
  let stop_body = format!("{{\"session\":{}}}", quote(&session_id));
  let (status, out, _) = session_stop_response(&stop_body, store);
  if status != 200 {
    return (status, out, false);
  }
  let (status, out, mutated) =
    start_job_in_repo(&repo, def, profile, params, resume.then(|| session_id.clone()), retries, store);
  // Link the chain: the old session records its replacement, and the fresh session
  // inherits the supervisor state (attempt-incremented) so restart budgets and the
  // job-level breaker span the whole chain rather than resetting per restart.
  if status == 200 {
    if let Some(new_id) = out.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()) {
      let new_id = new_id.to_string();
      let mut store = lock_store(store);
      let inherited = store.sessions.get(&session_id).map(|old| old.supervisor.inherited());
      if let Some(old) = store.sessions.get_mut(&session_id) {
        old.supervisor.restarted_as = Some(new_id.clone());
        old.supervisor.next_retry_at = None;
        // The stop inside this restart marked the old session "stopped from the browser";
        // being replaced is not giving up — the chain continues in the new session.
        old.supervisor.gave_up = None;
      }
      if let Some(sup) = inherited {
        if let Some(new) = store.sessions.get_mut(&new_id) {
          new.supervisor = sup;
        }
      }
    }
  }
  (status, out, mutated)
}

/// `POST /api/v1/session/stop` — body `{"session":"…"}`. Force-stop a stalled job: stop every
/// still-named container for the session, SIGTERM (then SIGKILL) the `scsh run` process when its
/// PID is known, and mark incomplete procs failed with `force_stopped`. Idempotent on an already
/// ended session (`{ok:true,already_ended:true}`).
fn session_stop_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object"), false),
  };
  let session_id = match field_str(&obj, "session") {
    Some(s) if !s.trim().is_empty() => s.trim().to_string(),
    _ => return (400, err_body("give a session id"), false),
  };
  let now = now_unix_secs();
  let runtime = crate::runtime::detect_runtime().map(|r| r.name);
  let (run_pid, containers, suppressed, already_ended) = {
    let mut store = lock_store(store);
    let Some(s) = store.sessions.get_mut(&session_id) else {
      return (404, err_body("session not found"), false);
    };
    if s.ended_at.is_some() {
      // Even a stop that arrives after the job settled is the human saying stop: the
      // session's pending supervision is cancelled, not just this (absent) run —
      // otherwise the supervisor would resurrect a job the user explicitly killed.
      let mutated =
        if s.supervisor.supervised() && s.supervisor.restarted_as.is_none() && s.supervisor.gave_up.is_none() {
          s.supervisor.next_retry_at = None;
          s.supervisor.gave_up = Some("stopped from the session browser".into());
          true
        } else {
          false
        };
      return (200, "{\"ok\":true,\"already_ended\":true}".into(), mutated);
    }
    let containers: Vec<String> = s.procs.iter().filter_map(|p| p.container_name.clone()).collect();
    let suppressed: Vec<String> = s
      .procs
      .iter()
      .filter(|p| p.status == ProcStatus::Running || p.status == ProcStatus::Waiting)
      .filter_map(|p| match p.kind {
        ProcKind::Skill => p.cast_path.clone(),
        ProcKind::Annotate => p.annotate_target.clone(),
        ProcKind::Build => None,
      })
      .collect();
    let run_pid = s.run_pid;
    s.ended_at = Some(now);
    s.client_connected = false;
    s.run_pid = None;
    s.last_seen_at = now;
    // A human's stop IS supervision — the supervisor must never resurrect a job the
    // user explicitly killed.
    if s.supervisor.supervised() && s.supervisor.restarted_as.is_none() {
      s.supervisor.next_retry_at = None;
      s.supervisor.gave_up = Some("stopped from the session browser".into());
    }
    for p in &mut s.procs {
      if p.status == ProcStatus::Running || p.status == ProcStatus::Waiting {
        p.status = ProcStatus::Fail;
        p.fail_reason = Some(crate::failure::reason::FORCE_STOPPED.into());
        if p.detail.is_none() {
          p.detail = Some("stopped from the session browser".into());
        }
        p.container_name = None;
      }
    }
    (run_pid, containers, suppressed, false)
  };
  let _ = already_ended;
  for path in suppressed {
    crate::annotate::suppress_automatic_annotation(std::path::Path::new(&path));
  }
  // Tear down outside the store lock: container stop sleeps up to ~1s each.
  if let Some(rt) = runtime.as_deref() {
    for name in &containers {
      crate::ui::signals::stop_container(rt, name);
    }
  }
  if let Some(pid) = run_pid {
    signal_run_pid(pid);
  }
  crate::failure::log_session_proc(
    &session_id,
    crate::failure::reason::FORCE_STOPPED,
    "(session)",
    "stopped from the session browser",
  );
  (200, "{\"ok\":true}".into(), true)
}

/// `POST /api/v1/proc/stop` — body `{"session":"…","proc":<index>}`. Stop one live proc:
/// kill its container, or signal the host annotation process while suppressing its sidecar.
/// The session and its other procs keep running. Idempotent on a proc that already finished
/// (`{ok:true,already_ended:true}`).
#[cfg(test)]
fn proc_stop_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  proc_stop_response_notifying(body, store, || {})
}

fn proc_stop_response_notifying<F: Fn()>(body: &str, store: &Arc<Mutex<Store>>, notify: F) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object"), false),
  };
  let session_id = match field_str(&obj, "session") {
    Some(s) if !s.trim().is_empty() => s.trim().to_string(),
    _ => return (400, err_body("give a session id"), false),
  };
  let Some(index) = field_num(&obj, "proc").map(|n| n as usize) else {
    return (400, err_body("give a proc index"), false);
  };
  let runtime = crate::runtime::detect_runtime().map(|r| r.name);
  let (container, label, annotation_pid, suppress) = {
    let mut store = lock_store(store);
    let Some(s) = store.sessions.get_mut(&session_id) else {
      return (404, err_body("session not found"), false);
    };
    let run_pid = s.run_pid;
    let Some(p) = s.procs.iter_mut().find(|p| p.index == index) else {
      return (404, err_body("proc not found"), false);
    };
    if p.status != ProcStatus::Running && p.status != ProcStatus::Waiting {
      return (200, "{\"ok\":true,\"already_ended\":true}".into(), false);
    }
    let container = p.container_name.take();
    let annotation_pid = (p.kind == ProcKind::Annotate).then_some(run_pid).flatten();
    let suppress = match p.kind {
      ProcKind::Skill => p.cast_path.clone(),
      ProcKind::Annotate => p.annotate_target.clone(),
      ProcKind::Build => None,
    };
    p.fail_reason = Some(crate::failure::reason::STOP_REQUESTED.into());
    p.detail = Some(if p.kind == ProcKind::Annotate {
      "stopping annotation; the recording will remain unchanged".into()
    } else {
      "terminating container from the session browser".into()
    });
    let label = p.label.clone();
    s.last_seen_at = now_unix_secs();
    (container, label, annotation_pid, suppress)
  };
  // Publish the accepted request before teardown, which may take a second or more.
  notify();
  if let Some(path) = suppress {
    crate::annotate::suppress_automatic_annotation(std::path::Path::new(&path));
  }
  // Tear down outside the store lock: container stop sleeps up to ~1s.
  if let (Some(rt), Some(name)) = (runtime.as_deref(), container.as_deref()) {
    crate::ui::signals::stop_container(rt, name);
  }
  if let Some(pid) = annotation_pid {
    signal_run_pid(pid);
  }
  let finalized = {
    let mut store = lock_store(store);
    let Some(p) = store.proc_mut(&session_id, index) else {
      return (404, err_body("proc disappeared during stop"), true);
    };
    if p.fail_reason.as_deref() != Some(crate::failure::reason::STOP_REQUESTED) {
      false
    } else {
      p.status = ProcStatus::Fail;
      p.fail_reason = Some(crate::failure::reason::FORCE_STOPPED.into());
      p.detail = Some(if p.kind == ProcKind::Annotate {
        "annotation stopped from the session browser; the recording is unchanged".into()
      } else {
        "container stopped from the session browser".into()
      });
      true
    }
  };
  notify();
  if finalized {
    crate::failure::log_session_proc(
      &session_id,
      crate::failure::reason::FORCE_STOPPED,
      &label,
      "container stopped from the session browser",
    );
  }
  (200, "{\"ok\":true}".into(), true)
}

/// `POST /api/v1/proc/restart` — body `{"session":"…","proc":N}`. Force-restart ONE skill run:
/// record the request as a marker file for the owning `scsh run`, kill this attempt's container,
/// and leave the proc in `restart_requested`; the runner consumes the marker when the attempt
/// comes back failed and registers a fresh proc with an explicit `previous_attempt` edge. Only
/// then does the old attempt settle as `force_restarted`. Builds and annotations have no respawn path and are
/// refused. Requires the run client to still be attached — with no runner alive nothing could
/// act on the marker, so the request is refused rather than degraded into a plain stop.
#[cfg(test)]
fn proc_restart_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  proc_restart_response_notifying(body, store, || {})
}

fn proc_restart_response_notifying<F: Fn()>(body: &str, store: &Arc<Mutex<Store>>, notify: F) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object"), false),
  };
  let session_id = match field_str(&obj, "session") {
    Some(s) if !s.trim().is_empty() => s.trim().to_string(),
    _ => return (400, err_body("give a session id"), false),
  };
  let Some(index) = field_num(&obj, "proc").map(|n| n as usize) else {
    return (400, err_body("give a proc index"), false);
  };
  let runtime = crate::runtime::detect_runtime().map(|r| r.name);
  let (container, label, suppress) = {
    let mut store = lock_store(store);
    let Some(s) = store.sessions.get_mut(&session_id) else {
      return (404, err_body("session not found"), false);
    };
    let client_connected = s.client_connected;
    let Some(p) = s.procs.iter_mut().find(|p| p.index == index) else {
      return (404, err_body("proc not found"), false);
    };
    if p.kind != ProcKind::Skill {
      return (400, err_body("only skill runs can be restarted — builds and annotations have no respawn path"), false);
    }
    if matches!(
      p.fail_reason.as_deref(),
      Some(crate::failure::reason::STOP_REQUESTED) | Some(crate::failure::reason::RESTART_REQUESTED)
    ) {
      return (200, "{\"ok\":true,\"already_requested\":true}".into(), false);
    }
    if p.status != ProcStatus::Running && p.status != ProcStatus::Waiting {
      return (200, "{\"ok\":true,\"already_ended\":true}".into(), false);
    }
    if !client_connected {
      return (409, err_body("the run client is gone — nothing is left to respawn this route"), false);
    }
    // The marker is the whole daemon→runner channel: without it this would be a plain
    // force stop wearing a restart label, so a failed write refuses the request instead.
    if !crate::daemon::request_proc_restart(&session_id, index) {
      return (500, err_body("could not record the restart request"), false);
    }
    let container = p.container_name.take();
    p.fail_reason = Some(crate::failure::reason::RESTART_REQUESTED.into());
    p.detail = Some("terminating container from the session browser; a fresh attempt follows".into());
    let label = p.label.clone();
    s.last_seen_at = now_unix_secs();
    (container, label, p.cast_path.clone())
  };
  // Publish the accepted request before teardown, which may take a second or more.
  notify();
  // This attempt's recording ends mid-scene; the fresh attempt gets its own annotation.
  if let Some(path) = suppress {
    crate::annotate::suppress_automatic_annotation(std::path::Path::new(&path));
  }
  // Tear down outside the store lock: container stop sleeps up to ~1s.
  if let (Some(rt), Some(name)) = (runtime.as_deref(), container.as_deref()) {
    crate::ui::signals::stop_container(rt, name);
  }
  let replacement_pending = {
    let mut store = lock_store(store);
    let Some(p) = store.proc_mut(&session_id, index) else {
      return (404, err_body("proc disappeared during restart"), true);
    };
    if p.fail_reason.as_deref() != Some(crate::failure::reason::RESTART_REQUESTED) {
      false
    } else {
      p.detail = Some("restart requested; waiting for the replacement attempt to register".into());
      true
    }
  };
  notify();
  if replacement_pending {
    crate::failure::log_session_proc(
      &session_id,
      crate::failure::reason::RESTART_REQUESTED,
      &label,
      "restart requested from the session browser",
    );
  }
  (200, "{\"ok\":true,\"replacement\":\"pending\"}".into(), true)
}

/// `POST /api/v1/harness/stop` — body `{"harness":"grok"}`. Stop EVERY still-running skill
/// container of one harness across all live sessions (the "grok is out of quota" button) and
/// expose each proc as `stop_requested` while teardown runs, then settle it as stopped.
/// Returns `{ok:true,stopped:<n>}` (`0` when nothing of that harness was running).
#[cfg(test)]
fn harness_stop_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  harness_stop_response_notifying(body, store, || {})
}

fn harness_stop_response_notifying<F: Fn()>(body: &str, store: &Arc<Mutex<Store>>, notify: F) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => return (400, err_body("expected a JSON object"), false),
  };
  let harness = match field_str(&obj, "harness") {
    Some(h) if !h.trim().is_empty() => h.trim().to_string(),
    _ => return (400, err_body("give a harness name"), false),
  };
  let runtime = crate::runtime::detect_runtime().map(|r| r.name);
  let now = now_unix_secs();
  // (session, proc label, container) for every victim — teardown and logging happen
  // outside the store lock (container stop sleeps up to ~1s each).
  let mut stopped: Vec<(String, usize, String, Option<String>)> = Vec::new();
  {
    let mut store = lock_store(store);
    for (sid, s) in store.sessions.iter_mut() {
      // Lifecycle, not `ended_at`: a dead client's session stays un-ended with "running"
      // procs forever — Terminated zombies have no containers left to stop, and marking
      // their history force_stopped would be a lie.
      if s.lifecycle_status(now) != SessionLifecycle::Running {
        continue;
      }
      let mut touched = false;
      for p in &mut s.procs {
        let live = p.status == ProcStatus::Running || p.status == ProcStatus::Waiting;
        if !live || p.kind != ProcKind::Skill || p.harness.as_deref() != Some(harness.as_str()) {
          continue;
        }
        stopped.push((sid.clone(), p.index, p.label.clone(), p.container_name.take()));
        p.fail_reason = Some(crate::failure::reason::STOP_REQUESTED.into());
        p.detail = Some(format!("terminating all {harness} containers from the session browser"));
        touched = true;
      }
      if touched {
        s.last_seen_at = now;
      }
    }
  }
  if !stopped.is_empty() {
    // Let every open job page render orange Terminating before teardown begins.
    notify();
  }
  for (sid, index, label, container) in &stopped {
    if let (Some(rt), Some(name)) = (runtime.as_deref(), container.as_deref()) {
      crate::ui::signals::stop_container(rt, name);
    }
    let finalized = {
      let mut store = lock_store(store);
      let Some(p) = store.proc_mut(sid, *index) else { continue };
      if p.fail_reason.as_deref() != Some(crate::failure::reason::STOP_REQUESTED) {
        false
      } else {
        p.status = ProcStatus::Fail;
        p.fail_reason = Some(crate::failure::reason::FORCE_STOPPED.into());
        p.detail = Some(format!("all {harness} containers stopped from the session browser"));
        true
      }
    };
    notify();
    if finalized {
      crate::failure::log_session_proc(
        sid,
        crate::failure::reason::FORCE_STOPPED,
        label,
        &format!("all {harness} containers stopped from the session browser"),
      );
    }
  }
  let n = stopped.len();
  (200, format!("{{\"ok\":true,\"stopped\":{n}}}"), n > 0)
}

/// SIGTERM a run PID, wait briefly, then SIGKILL if it is still alive — same cadence as Ctrl-C.
fn signal_run_pid(pid: u32) {
  let _ = std::process::Command::new("kill")
    .arg("-TERM")
    .arg(pid.to_string())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();
  std::thread::sleep(std::time::Duration::from_secs(1));
  if super::paths::pid_alive(pid) {
    let _ = std::process::Command::new("kill")
      .arg("-9")
      .arg(pid.to_string())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status();
  }
}

/// The tasks a definition will run, as `SkillMeta` (name + agent) — a workflow's steps, or a
/// flat definition's expanded `{def}-{route}` invocations. Pre-populated on a job's session so
/// its page immediately lists what is about to run instead of a blank board.
fn planned_skills(def: &crate::harness_def::HarnessDef, def_name: &str) -> Vec<SkillMeta> {
  if def.is_workflow() {
    def.steps.iter().map(|s| SkillMeta { name: s.id.clone(), harness: s.agent.harness.as_str().to_string() }).collect()
  } else {
    def
      .invocations
      .iter()
      .map(|r| SkillMeta { name: format!("{def_name}-{}", r.name), harness: r.harness.as_str().to_string() })
      .collect()
  }
}

/// Check job params against a definition: every required param present, every value well-typed.
fn validate_job_params(def: &crate::harness_def::HarnessDef, params: &[(String, String)]) -> Result<(), String> {
  for p in &def.params {
    match params.iter().find(|(k, _)| k == &p.name) {
      Some((_, v)) => p.validate_value(v)?,
      None => {
        if p.required && p.default.is_none() {
          return Err(format!("param '{}' is required", p.name));
        }
      }
    }
  }
  Ok(())
}

/// Extract the `params` object as `(name, value-as-string)` pairs (strings, numbers, and bools
/// are accepted; other JSON shapes are skipped).
fn read_params(obj: &[(String, Value)]) -> Vec<(String, String)> {
  let Some(Value::Object(entries)) = obj.iter().find(|(k, _)| k == "params").map(|(_, v)| v) else {
    return Vec::new();
  };
  entries.iter().filter_map(|(k, v)| value_to_string(v).map(|s| (k.clone(), s))).collect()
}

fn value_to_string(v: &Value) -> Option<String> {
  match v {
    Value::String(s) => Some(s.clone()),
    Value::Bool(b) => Some(b.to_string()),
    Value::Number(n) => Some(if n.fract() == 0.0 { format!("{}", *n as i64) } else { n.to_string() }),
    _ => None,
  }
}

/// One JSON object per definition (name, description, source, params, agent routes).
fn defs_json(defs: &[crate::harness_def::HarnessDef]) -> Vec<String> {
  defs.iter().map(def_json).collect()
}

/// The NAMED skill profiles the global manifest declares (`$SCSH_HOME/.scsh.yml`, written by
/// `scsh installskills --global`), as JSON cards for the run page — so any opened repo can
/// start them from the browser. "default" is excluded: the global manifest never serves a
/// bare `scsh run` (mirroring `resolve_config_for_run`), so only named profiles are
/// startable anywhere.
fn global_profiles_json() -> Vec<String> {
  let yml = crate::runtime::scsh_home().join(".scsh.yml");
  let Ok(src) = std::fs::read_to_string(&yml) else { return Vec::new() };
  let Ok(cfg) = crate::config::validate(&src) else { return Vec::new() };
  let mut profiles: Vec<(String, Vec<String>)> = Vec::new();
  for inv in crate::config::expand_invocations(&cfg) {
    let profile = inv.profile.as_deref().unwrap_or("default");
    if profile == "default" {
      continue;
    }
    let agent = format!(
      "{{\"route\":{},\"agent\":{},\"model\":{}}}",
      quote(&inv.name),
      quote(inv.harness.as_str()),
      inv.model.as_deref().map(quote).unwrap_or_else(|| "null".into()),
    );
    match profiles.iter_mut().find(|(name, _)| name == profile) {
      Some((_, agents)) => agents.push(agent),
      None => profiles.push((profile.to_string(), vec![agent])),
    }
  }
  profiles
    .into_iter()
    .map(|(name, agents)| format!("{{\"name\":{},\"agents\":[{}]}}", quote(&name), agents.join(",")))
    .collect()
}

/// Where a started job's recipe lives: `$SCSH_HOME/sessions/<id>/start.json` — the def or
/// profile it ran plus the params it was started with. Written by `jobs/start`, read back
/// by `jobs/restart`, and reclaimed with the rest of the session dir. A session without one
/// (a CLI-started run — its env was never the daemon's to see) restarts from its stored
/// name alone.
fn session_start_recipe_path(session_id: &str) -> std::path::PathBuf {
  crate::runtime::host_sessions_dir().join(session_id).join("start.json")
}

/// Best-effort: a missing recipe only means a later restart falls back to the name-only path
/// (and loses any env params). The daemon writes it when it spawns a job; a CLI-started
/// workflow run writes its own (def name + env-resolved params), so every job restarts with
/// the params it actually ran with.
pub(crate) fn write_start_recipe(
  session_id: &str, def: Option<&str>, profile: Option<&str>, params: &[(String, String)],
) {
  let path = session_start_recipe_path(session_id);
  let Some(dir) = path.parent() else { return };
  if std::fs::create_dir_all(dir).is_err() {
    return;
  }
  let what = match (def, profile) {
    (Some(d), _) => format!("\"def\":{}", quote(d)),
    (_, Some(p)) => format!("\"profile\":{}", quote(p)),
    _ => return,
  };
  let params: Vec<String> = params.iter().map(|(k, v)| format!("{}:{}", quote(k), quote(v))).collect();
  let _ = std::fs::write(&path, format!("{{{what},\"params\":{{{}}}}}", params.join(",")));
}

/// A persisted start recipe: `(def, profile, params)`.
type StartRecipe = (Option<String>, Option<String>, Vec<(String, String)>);

/// The recipe from a session's persisted `start.json`, or `None` when the session has no
/// (readable) recipe.
fn read_start_recipe(session_id: &str) -> Option<StartRecipe> {
  let src = std::fs::read_to_string(session_start_recipe_path(session_id)).ok()?;
  let Ok(Value::Object(obj)) = parse(&src) else { return None };
  let def = field_str(&obj, "def").filter(|s| !s.is_empty());
  let profile = field_str(&obj, "profile").filter(|s| !s.is_empty());
  if def.is_none() && profile.is_none() {
    return None;
  }
  let params = read_params(&obj);
  Some((def, profile, params))
}

/// The routes a named skill profile would run in `root`, mirroring `scsh run <profile>`'s
/// config precedence: the repo's own `.scsh.yml` wins when it declares the profile;
/// otherwise the global manifest serves it. Empty when neither declares the profile (or a
/// manifest does not validate).
fn profile_routes_for(root: &std::path::Path, profile: &str) -> Vec<crate::config::ResolvedInvocation> {
  for yml in [root.join(".scsh.yml"), crate::runtime::scsh_home().join(".scsh.yml")] {
    let Ok(src) = std::fs::read_to_string(&yml) else { continue };
    let Ok(cfg) = crate::config::validate(&src) else { continue };
    let routes: Vec<_> = crate::config::expand_invocations(&cfg)
      .into_iter()
      .filter(|inv| inv.profile.as_deref().unwrap_or("default") == profile)
      .collect();
    if !routes.is_empty() {
      return routes;
    }
  }
  Vec::new()
}

fn def_json(def: &crate::harness_def::HarnessDef) -> String {
  let params: Vec<String> = def.params.iter().map(param_json).collect();
  // Agents come from the flat matrix, or (for a workflow) from each step's agent.
  let agent_obj = |route: &str, harness: &str, model: Option<&str>| {
    format!(
      "{{\"route\":{},\"agent\":{},\"model\":{}}}",
      quote(route),
      quote(harness),
      model.map(quote).unwrap_or_else(|| "null".into())
    )
  };
  let agents: Vec<String> = if def.is_workflow() {
    def.steps.iter().map(|s| agent_obj(&s.id, s.agent.harness.as_str(), s.agent.model.as_deref())).collect()
  } else {
    def.invocations.iter().map(|r| agent_obj(&r.name, r.harness.as_str(), r.model.as_deref())).collect()
  };
  format!(
    "{{\"name\":{},\"description\":{},\"source\":{},\"workflow\":{},\"steps\":{},\"params\":[{}],\"agents\":[{}]}}",
    quote(&def.name),
    quote(&def.description),
    quote(def.source.as_str()),
    def.is_workflow(),
    def.steps.len(),
    params.join(","),
    agents.join(",")
  )
}

fn param_json(p: &crate::harness_def::Param) -> String {
  let default = p.default.as_deref().map(quote).unwrap_or_else(|| "null".into());
  let description = p.description.as_deref().map(quote).unwrap_or_else(|| "null".into());
  let choices: Vec<String> = p.choices.iter().map(|c| quote(c)).collect();
  format!(
    "{{\"name\":{},\"type\":{},\"default\":{},\"required\":{},\"description\":{},\"choices\":[{}]}}",
    quote(&p.name),
    quote(p.ty.as_str()),
    default,
    p.required,
    description,
    choices.join(",")
  )
}

/// A JSON string array field (e.g. `"harnesses": ["claude", "codex"]`); non-strings are skipped.
fn parse_string_array(obj: &[(String, Value)], key: &str) -> Vec<String> {
  let Some(Value::Array(arr)) = obj.iter().find(|(k, _)| k == key).map(|(_, v)| v) else {
    return Vec::new();
  };
  arr
    .iter()
    .filter_map(|item| match item {
      Value::String(s) => Some(s.clone()),
      _ => None,
    })
    .collect()
}

fn deregister_incomplete_detail(p: &ProcRecord) -> String {
  let last = p.lines.last().map(|l| l.text.as_str()).unwrap_or("");
  let tail = if last.chars().count() > 200 {
    format!("{}…", last.chars().take(200).collect::<String>())
  } else {
    last.to_string()
  };
  if tail.is_empty() {
    "session ended before this proc reported finish (lost proc/finish event or crash)".into()
  } else {
    format!("session ended before this proc reported finish; last output: {tail}")
  }
}

fn touch_session_liveness(store: &mut Store, session_id: &str, now: u64) {
  if session_id.is_empty() {
    return;
  }
  if let Some(s) = store.session_mut(session_id) {
    s.last_seen_at = now;
  }
}

fn push_proc_lines(p: &mut ProcRecord, lines: &[(f64, String)]) {
  for (at, text) in lines {
    if p.lines.len() >= MAX_PROC_LINES {
      let drop_n = p.lines.len() - MAX_PROC_LINES + 1;
      p.lines.drain(0..drop_n);
    }
    p.lines.push(OutputLine { at: *at, text: text.clone() });
  }
}

fn parse_lines_field(obj: &[(String, Value)]) -> Vec<(f64, String)> {
  let Some(Value::Array(arr)) = obj.iter().find(|(k, _)| k == "lines").map(|(_, v)| v) else {
    return Vec::new();
  };
  arr
    .iter()
    .filter_map(|item| {
      let Value::Object(fields) = item else { return None };
      Some((field_num(fields, "at").unwrap_or(0.0), field_str(fields, "line").unwrap_or_default()))
    })
    .collect()
}

fn parse_skills_array(obj: &[(String, Value)]) -> Vec<SkillMeta> {
  let Some(Value::Array(arr)) = obj.iter().find(|(k, _)| k == "skills").map(|(_, v)| v) else {
    return Vec::new();
  };
  arr
    .iter()
    .filter_map(|item| {
      let Value::Object(fields) = item else { return None };
      Some(SkillMeta {
        name: field_str(fields, "name").unwrap_or_default(),
        harness: field_str(fields, "harness").unwrap_or_default(),
      })
    })
    .collect()
}

fn write_response(
  stream: &mut TcpStream, status: u16, body: &str, content_type: &str, close: bool,
) -> std::io::Result<()> {
  let status_text = match status {
    200 => "OK",
    400 => "Bad Request",
    403 => "Forbidden",
    404 => "Not Found",
    405 => "Method Not Allowed",
    _ => "Error",
  };
  let connection = if close { "close" } else { "keep-alive" };
  let resp = format!(
    "HTTP/1.1 {status} {status_text}\r\n\
Content-Type: {content_type}\r\n\
Content-Length: {}\r\n\
Connection: {connection}\r\n\r\n\
{body}",
    body.len()
  );
  stream.write_all(resp.as_bytes())?;
  Ok(())
}

/// Like [`write_response`] but for downloadable payloads (`.cast` bytes, `/export.html`
/// pages): the given content type on 200 plus an optional Content-Disposition (the
/// attachment variants). 404 bodies are text.
fn write_download_response(
  stream: &mut TcpStream, status: u16, body: &str, ok_content_type: &str, disposition: Option<&str>, close: bool,
) -> std::io::Result<()> {
  let status_text = if status == 200 { "OK" } else { "Not Found" };
  let content_type = if status == 200 { ok_content_type } else { "text/plain" };
  let disposition_header = match disposition {
    Some(d) => format!("Content-Disposition: {d}\r\n"),
    None => String::new(),
  };
  let connection = if close { "close" } else { "keep-alive" };
  let resp = format!(
    "HTTP/1.1 {status} {status_text}\r\n\
Content-Type: {content_type}\r\n\
{disposition_header}Content-Length: {}\r\n\
Connection: {connection}\r\n\r\n\
{body}",
    body.len()
  );
  stream.write_all(resp.as_bytes())?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::model::SessionLifecycle;
  use std::io::Write;
  use std::net::TcpListener;
  use std::thread;

  #[test]
  fn peer_is_local_serves_loopback_only() {
    // Loopback over either family is the local machine and is served.
    assert!(peer_is_local("127.0.0.1:7274".parse().unwrap()));
    assert!(peer_is_local("127.0.0.5:7274".parse().unwrap()));
    assert!(peer_is_local("[::1]:7274".parse().unwrap()));
    // Anything arriving over a routable interface is not local and is denied.
    assert!(!peer_is_local("192.168.1.10:7274".parse().unwrap()));
    assert!(!peer_is_local("10.0.0.4:7274".parse().unwrap()));
    assert!(!peer_is_local("203.0.113.7:7274".parse().unwrap()));
    assert!(!peer_is_local("[2001:db8::1]:7274".parse().unwrap()));
  }

  #[test]
  fn connection_close_reads_the_header_as_a_token_list() {
    let h = |v: &str| vec![("Connection".to_string(), v.to_string())];
    assert!(connection_close(&h("close")));
    assert!(connection_close(&h("Close")), "token match is case-insensitive");
    assert!(connection_close(&h("close, upgrade")), "close inside a token list still closes");
    assert!(connection_close(&h("keep-alive, close")));
    assert!(!connection_close(&h("keep-alive")));
    assert!(!connection_close(&h("closed")), "only the exact token, not a prefix");
    assert!(!connection_close(&[]), "HTTP/1.1 defaults to keep-alive");
  }

  #[test]
  fn write_response_labels_forbidden() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      write_response(&mut server, 403, "denied\n", "text/plain; charset=utf-8", true).unwrap();
    });
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    let mut buf = String::new();
    client.read_to_string(&mut buf).unwrap();
    handle.join().unwrap();
    assert!(buf.starts_with("HTTP/1.1 403 Forbidden\r\n"), "got: {buf}");
    assert!(buf.contains("denied\n"));
  }

  #[test]
  fn read_request_reads_body_after_split_header() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let body = r#"{"session":"abcxyz","repo":"/tmp"}"#;
    let header =
      format!("POST /api/v1/session/start HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n", body.len());
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      read_request(&mut server).unwrap()
    });
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client.write_all(header.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(20));
    client.write_all(body.as_bytes()).unwrap();
    drop(client);
    let req = handle.join().unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/api/v1/session/start");
    assert_eq!(req.body, body);
  }

  #[test]
  fn parse_content_length_finds_header() {
    let headers = b"POST / HTTP/1.1\r\nContent-Length: 12\r\n\r\n";
    assert_eq!(parse_content_length(headers), 12);
  }

  #[test]
  fn read_request_handles_http11_get_without_extra_read() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let host = format!("127.0.0.1:{}", addr.port());
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      read_request(&mut server).unwrap()
    });
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client.write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\n\r\n").as_bytes()).unwrap();
    // Do not shutdown write — browsers and curl keep the write half open on HTTP/1.1.
    let req = handle.join().unwrap();
    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/");
  }

  #[test]
  fn parse_content_length_is_case_insensitive() {
    let headers = b"POST / HTTP/1.1\r\ncontent-length: 9\r\n\r\n";
    assert_eq!(parse_content_length(headers), 9);
  }

  #[test]
  fn version_endpoint_reports_the_running_daemons_version() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    let ws_dirty = AtomicBool::new(false);
    let req = HttpRequest {
      method: "GET".into(),
      path: "/api/v1/version".into(),
      body: String::new(),
      headers: Vec::new(),
      http1_0: false,
    };
    let (status, body, content_type, mutated) = route(&req, &store, &prune, &ws_dirty);
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/json");
    assert!(!mutated);
    assert!(body.contains(crate::version::pkg_version()), "endpoint omits the version: {body}");
    assert!(body.contains("\"version\""), "{body}");
    // The bare stamp rides alongside: a hash string in any stamped build, null otherwise.
    assert!(body.contains("\"commit\""), "{body}");
    let stamp = crate::version::git_stamp();
    if stamp.is_empty() {
      assert!(body.contains("\"commit\": null"), "{body}");
    } else {
      assert!(body.contains(&format!("\"commit\": \"{stamp}\"")), "{body}");
    }
    assert!(parse(&body).is_ok(), "version payload parses as JSON: {body}");
  }

  #[test]
  fn fleet_endpoint_serves_rollups_and_job_verdict() {
    use crate::daemon::model::{ProcKind, ProcRecord, ProcStatus, Session};
    let route_proc = |index: usize, source: &str, route: &str| ProcRecord {
      index,
      previous_attempt: None,
      kind: ProcKind::Skill,
      label: format!("{source}-{route}"),
      status: ProcStatus::Ok,
      note: None,
      detail: Some("done".into()),
      fail_reason: None,
      container_name: None,
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: Some(source.into()),
      route: Some(route.into()),
      result_path: None,
      annotate_target: None,
      harness: Some("claude".into()),
      skill_name: Some(format!("{source}-{route}")),
      model: None,
      started_at: Some(1),
      elapsed: Some(1.0),
      lines: vec![],
    };
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    lock_store(&store).sessions.insert(
      "flapi1".into(),
      Session {
        id: "flapi1".into(),
        started_at: 1,
        ended_at: Some(10),
        profile: Some("default".into()),
        kind: Some("profile".into()),
        repo: "/tmp/repo".into(),
        branch: "main".into(),
        last_seen_at: 10,
        client_connected: false,
        run_pid: None,
        skills: vec![],
        procs: vec![
          route_proc(0, "conventions-reviewer", "opus"),
          route_proc(1, "conventions-reviewer", "codex"),
          route_proc(2, "testing-reviewer", "opus"),
          route_proc(3, "testing-reviewer", "codex"),
        ],
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    let ws_dirty = AtomicBool::new(false);
    let req = HttpRequest {
      method: "GET".into(),
      path: "/api/v1/session/flapi1/fleet".into(),
      body: String::new(),
      headers: Vec::new(),
      http1_0: false,
    };
    let (status, body, content_type, mutated) = route(&req, &store, &prune, &ws_dirty);
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/json");
    assert!(!mutated);
    // The payload is real JSON with the verdict and both per-skill rollups.
    assert!(parse(&body).is_ok(), "fleet payload parses as JSON: {body}");
    assert!(body.contains("\"session\": \"flapi1\""), "{body}");
    assert!(body.contains("\"routes\": 4") && body.contains("\"ok\": 4"), "job verdict counts routes: {body}");
    assert!(body.contains("\"skill_source\": \"conventions-reviewer\""), "{body}");
    assert!(body.contains("\"skill_source\": \"testing-reviewer\""), "{body}");
    // No result files here → the verdict reports no mean rather than inventing one.
    assert!(body.contains("\"mean_score\": null"), "{body}");

    let missing = HttpRequest {
      method: "GET".into(),
      path: "/api/v1/session/zzzzzz/fleet".into(),
      body: String::new(),
      headers: Vec::new(),
      http1_0: false,
    };
    let (status, _, _, _) = route(&missing, &store, &prune, &ws_dirty);
    assert_eq!(status, 404);
  }

  #[test]
  fn stats_endpoint_serves_flakiness_json() {
    let _env = crate::runtime::test_env_lock();
    let file = std::env::temp_dir().join(format!("scsh-stats-api-{}.jsonl", crate::runtime::random_nonce_6()));
    let prev = std::env::var_os(crate::stats::STATS_FILE_ENV);
    std::env::set_var(crate::stats::STATS_FILE_ENV, &file);
    crate::stats::record(&crate::stats::StatRecord {
      ts: 1000,
      kind: "skill".into(),
      session: "abc".into(),
      repo: "/r".into(),
      branch: "b".into(),
      profile: None,
      skill: Some("add-claude".into()),
      skill_source: Some("add".into()),
      harness: Some("claude".into()),
      model: None,
      effort: None,
      outcome: Some("ok".into()),
      fail_reason: None,
      attempts: 1,
      duration_secs: 4.0,
      commits: 0,
      loc_added: 0,
      loc_deleted: 0,
      skills_total: None,
      skills_failed: None,
    });

    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    let ws_dirty = AtomicBool::new(false);
    let req = HttpRequest {
      method: "GET".into(),
      path: "/api/v1/stats".into(),
      body: String::new(),
      headers: Vec::new(),
      http1_0: false,
    };
    let (status, body, content_type, mutated) = route(&req, &store, &prune, &ws_dirty);
    assert_eq!((status, content_type, mutated), (200, "application/json", false));
    assert!(parse(&body).is_ok(), "stats payload parses as JSON: {body}");
    assert!(body.contains("\"routes\"") && body.contains("\"skills\""), "{body}");
    assert!(body.contains("\"p95_secs\": 4.000"), "percentiles ride along: {body}");
    assert!(body.contains("\"fail_pct\": 0.00"), "{body}");

    let _ = std::fs::remove_file(&file);
    match prev {
      Some(v) => std::env::set_var(crate::stats::STATS_FILE_ENV, v),
      None => std::env::remove_var(crate::stats::STATS_FILE_ENV),
    }
  }

  #[test]
  fn setup_and_images_routes_honor_runtime_query_param() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    let ws_dirty = AtomicBool::new(false);
    for api in ["/api/v1/setup", "/api/v1/images"] {
      let req = HttpRequest {
        method: "GET".into(),
        path: format!("{api}?runtime=not-a-runtime-xyz"),
        body: String::new(),
        headers: Vec::new(),
        http1_0: false,
      };
      let (status, body, _, _) = route(&req, &store, &prune, &ws_dirty);
      assert_eq!(status, 200, "{api}");
      assert!(body.contains("\"error\""), "{api} ignored its runtime query param: {body}");
      assert!(body.contains("not-a-runtime-xyz"), "{api}: {body}");
    }
  }

  #[test]
  fn proc_events_update_liveness_and_record_container_runtime() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "xyzabc".into(),
        Session {
          id: "xyzabc".into(),
          started_at: 50,
          ended_at: None,
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
            started_at: Some(50),
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
          client_connected: true,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    let body = r#"{"session":"xyzabc","proc":0,"at":1.0,"line":"step"}"#;
    assert!(handle_api_post("/api/v1/proc/line", body, &store, &prune));
    let last = store.lock().unwrap().sessions.get("xyzabc").unwrap().last_seen_at;
    assert!(last > 50);

    let start = r#"{"session":"xyzabc","proc":0,"action":"start","name":"scsh-run","runtime":"container"}"#;
    assert!(handle_api_post("/api/v1/container", start, &store, &prune));
    {
      let guard = store.lock().unwrap();
      let proc = &guard.sessions.get("xyzabc").unwrap().procs[0];
      assert_eq!(proc.container_name.as_deref(), Some("scsh-run"));
      assert_eq!(proc.container_runtime.as_deref(), Some("container"));
    }
    let stop = r#"{"session":"xyzabc","proc":0,"action":"stop","name":"scsh-run","runtime":"container"}"#;
    assert!(handle_api_post("/api/v1/container", stop, &store, &prune));
    let guard = store.lock().unwrap();
    let proc = &guard.sessions.get("xyzabc").unwrap().procs[0];
    assert_eq!(proc.container_name, None);
    assert_eq!(proc.container_runtime.as_deref(), Some("container"));
  }

  #[test]
  fn proc_line_caps_at_max_proc_lines() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 1)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "captest".into(),
        Session {
          id: "captest".into(),
          started_at: 1,
          ended_at: None,
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
          last_seen_at: 1,
          client_connected: false,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    for i in 0..=MAX_PROC_LINES {
      let body = format!(r#"{{"session":"captest","proc":0,"at":{i}.0,"line":"L{i}"}}"#);
      assert!(handle_api_post("/api/v1/proc/line", &body, &store, &prune));
    }
    let len = store.lock().unwrap().sessions.get("captest").unwrap().procs[0].lines.len();
    assert_eq!(len, MAX_PROC_LINES);
  }

  #[test]
  fn proc_lines_bulk_appends_all() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 10)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "bulk".into(),
        Session {
          id: "bulk".into(),
          started_at: 10,
          ended_at: None,
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
            started_at: Some(10),
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
          last_seen_at: 10,
          client_connected: true,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    let body = r#"{"session":"bulk","proc":0,"lines":[{"at":1.0,"line":"a"},{"at":2.0,"line":"b"}]}"#;
    assert!(handle_api_post("/api/v1/proc/lines", body, &store, &prune));
    let guard = store.lock().unwrap();
    let lines = &guard.sessions.get("bulk").unwrap().procs[0].lines;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "a");
    assert_eq!(lines[1].text, "b");
  }

  #[test]
  fn prune_tick_endpoint_runs_janitor_pass() {
    let name = "scsh-tickab-run-add";
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 59999, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    {
      let mut q = prune.lock().unwrap();
      // Eligible immediately: scheduled far enough in the past that the grace period elapsed.
      q.schedule(&dir.to_string_lossy(), name, "docker", true, 0);
    }
    assert!(handle_api_post("/api/v1/prune/tick", "{}", &store, &prune));
    assert!(!dir.exists(), "eligible run dir should be deleted by the forced pass");
    assert!(prune.lock().unwrap().jobs.is_empty());
    let _ = std::fs::remove_file(super::super::paths::prune_file(59999));
  }

  #[test]
  fn deregister_marks_ended_and_fails_incomplete_procs() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "dereg01".into(),
        Session {
          id: "dereg01".into(),
          started_at: 50,
          ended_at: None,
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
            started_at: Some(50),
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
          client_connected: true,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    let body = r#"{"session":"dereg01"}"#;
    assert!(handle_api_post("/api/v1/deregister", body, &store, &prune));
    let guard = store.lock().unwrap();
    let session = guard.sessions.get("dereg01").unwrap();
    assert!(session.ended_at.is_some());
    assert_eq!(session.procs[0].status, ProcStatus::Fail);
    assert_eq!(session.procs[0].fail_reason.as_deref(), Some(crate::failure::reason::SESSION_END_INCOMPLETE));
    assert!(session.procs[0]
      .detail
      .as_deref()
      .unwrap_or("")
      .contains("session ended before this proc reported finish"));
    assert_eq!(session.lifecycle_status(session.ended_at.unwrap()), SessionLifecycle::Cancelled);
  }

  #[test]
  fn session_stop_marks_running_procs_force_stopped() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "stop01".into(),
        Session {
          id: "stop01".into(),
          started_at: 50,
          ended_at: None,
          profile: Some("doctor".into()),
          kind: None,
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: vec![ProcRecord {
            index: 0,
            previous_attempt: None,
            label: "opencode: doctor".into(),
            kind: ProcKind::Skill,
            status: ProcStatus::Running,
            skill_name: Some("doctor".into()),
            harness: Some("opencode".into()),
            model: None,
            started_at: Some(50),
            note: None,
            detail: None,
            fail_reason: None,
            elapsed: None,
            lines: Vec::new(),
            container_name: None, // no live container — avoid a 2s stop_container sleep in the unit test
            container_runtime: None,
            cast_path: None,
            diff_path: None,
            skill_source: None,
            route: None,
            result_path: None,
            annotate_target: None,
          }],
          last_seen_at: 50,
          client_connected: true,
          // No live PID — we assert store state, not kill success.
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    let (status, body, mutated) = session_stop_response(r#"{"session":"stop01"}"#, &store);
    assert_eq!(status, 200);
    assert!(mutated);
    assert!(body.contains(r#""ok":true"#));
    let guard = store.lock().unwrap();
    let session = guard.sessions.get("stop01").unwrap();
    assert!(session.ended_at.is_some());
    assert!(session.run_pid.is_none());
    assert_eq!(session.procs[0].status, ProcStatus::Fail);
    assert_eq!(session.procs[0].fail_reason.as_deref(), Some(crate::failure::reason::FORCE_STOPPED));
    drop(guard);
    // Idempotent on an already-ended session.
    let (status2, body2, mutated2) = session_stop_response(r#"{"session":"stop01"}"#, &store);
    assert_eq!(status2, 200);
    assert!(!mutated2);
    assert!(body2.contains("already_ended"));
  }

  #[test]
  fn proc_stop_kills_one_container_and_leaves_the_session_running() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let proc = |index: usize, name: &str| ProcRecord {
      index,
      previous_attempt: None,
      label: format!("grok: {name}"),
      kind: ProcKind::Skill,
      status: ProcStatus::Running,
      skill_name: Some(name.into()),
      harness: Some("grok".into()),
      model: None,
      started_at: Some(50),
      note: None,
      detail: None,
      fail_reason: None,
      elapsed: None,
      lines: Vec::new(),
      container_name: None, // no live container — avoid a 2s stop_container sleep in the unit test
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
    };
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "kill01".into(),
        Session {
          id: "kill01".into(),
          started_at: 50,
          ended_at: None,
          profile: None,
          kind: None,
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: vec![proc(0, "review-a"), proc(1, "review-b")],
          last_seen_at: 50,
          client_connected: true,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    let (status, body, mutated) = proc_stop_response(r#"{"session":"kill01","proc":1}"#, &store);
    assert_eq!(status, 200, "got: {body}");
    assert!(mutated);
    assert!(body.contains(r#""ok":true"#));
    {
      let guard = store.lock().unwrap();
      let session = guard.sessions.get("kill01").unwrap();
      // Only proc 1 was killed; the session and its sibling proc keep running.
      assert!(session.ended_at.is_none());
      assert_eq!(session.procs[0].status, ProcStatus::Running);
      assert_eq!(session.procs[1].status, ProcStatus::Fail);
      assert_eq!(session.procs[1].fail_reason.as_deref(), Some(crate::failure::reason::FORCE_STOPPED));
    }
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    assert!(handle_api_post(
      "/api/v1/proc/finish",
      r#"{"session":"kill01","proc":1,"status":"ok","fail_reason":null,"detail":"late success","elapsed":2}"#,
      &store,
      &prune,
    ));
    {
      let guard = store.lock().unwrap();
      let stopped = &guard.sessions.get("kill01").unwrap().procs[1];
      assert_eq!(stopped.status, ProcStatus::Fail, "a late harness finish cannot resurrect a stopped task");
      assert_eq!(stopped.fail_reason.as_deref(), Some(crate::failure::reason::FORCE_STOPPED));
    }
    // Idempotent on a proc that already finished.
    let (status2, body2, mutated2) = proc_stop_response(r#"{"session":"kill01","proc":1}"#, &store);
    assert_eq!(status2, 200);
    assert!(!mutated2);
    assert!(body2.contains("already_ended"));
    // Unknown proc index → 404.
    let (status3, _, _) = proc_stop_response(r#"{"session":"kill01","proc":9}"#, &store);
    assert_eq!(status3, 404);
  }

  #[test]
  fn proc_stop_cancels_annotation_without_touching_its_recording() {
    let dir = std::env::temp_dir().join(format!("scsh-stop-annotation-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("source.cast");
    std::fs::write(&cast, "recording").unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    {
      let mut guard = store.lock().unwrap();
      guard.insert_session(
        "annstp".into(),
        Session {
          id: "annstp".into(),
          started_at: 50,
          ended_at: None,
          profile: Some("annotate".into()),
          kind: Some("annotate".into()),
          repo: INTERNAL_REPO.into(),
          branch: String::new(),
          skills: Vec::new(),
          procs: vec![ProcRecord {
            index: 0,
            previous_attempt: None,
            label: "annotate · source".into(),
            kind: ProcKind::Annotate,
            status: ProcStatus::Running,
            skill_name: None,
            harness: Some("codex".into()),
            model: None,
            started_at: Some(50),
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
            annotate_target: Some(cast.to_string_lossy().into_owned()),
          }],
          last_seen_at: 50,
          client_connected: true,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    let (status, body, mutated) = proc_stop_response(r#"{"session":"annstp","proc":0}"#, &store);
    assert_eq!(status, 200, "body: {body}");
    assert!(mutated);
    let guard = store.lock().unwrap();
    let proc = &guard.sessions["annstp"].procs[0];
    assert_eq!(proc.fail_reason.as_deref(), Some(crate::failure::reason::FORCE_STOPPED));
    assert!(proc.detail.as_deref().is_some_and(|d| d.contains("recording is unchanged")));
    drop(guard);
    assert_eq!(std::fs::read_to_string(&cast).unwrap(), "recording");
    assert!(crate::annotate::automatic_annotation_suppressed(&cast));
    assert!(!dir.join("source.chapters.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn proc_restart_records_pending_attempt_and_links_the_replacement() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let proc = |index: usize, kind: ProcKind| ProcRecord {
      index,
      previous_attempt: None,
      label: format!("claude: review-{index}"),
      kind,
      status: ProcStatus::Running,
      skill_name: Some(format!("review-{index}")),
      harness: Some("claude".into()),
      model: None,
      started_at: Some(50),
      note: None,
      detail: None,
      fail_reason: None,
      elapsed: None,
      lines: Vec::new(),
      container_name: None, // no live container — avoid a 2s stop_container sleep in the unit test
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
    };
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "rst01".into(),
        Session {
          id: "rst01".into(),
          started_at: 50,
          ended_at: None,
          profile: None,
          kind: None,
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: vec![proc(0, ProcKind::Skill), proc(1, ProcKind::Skill), proc(2, ProcKind::Build)],
          last_seen_at: 50,
          client_connected: true,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    let (status, body, mutated) = proc_restart_response(r#"{"session":"rst01","proc":1}"#, &store);
    assert_eq!(status, 200, "got: {body}");
    assert!(mutated);
    assert!(body.contains(r#""ok":true"#));
    {
      let guard = store.lock().unwrap();
      let session = guard.sessions.get("rst01").unwrap();
      // Only proc 1 was interrupted; the session and its sibling proc keep running. The
      // attempt stays explicitly pending until the replacement registers.
      assert!(session.ended_at.is_none());
      assert_eq!(session.procs[0].status, ProcStatus::Running);
      assert_eq!(session.procs[1].status, ProcStatus::Running);
      assert_eq!(session.procs[1].fail_reason.as_deref(), Some(crate::failure::reason::RESTART_REQUESTED));
    }
    // The marker the owning `scsh run` consumes to respawn the route was written.
    assert!(crate::daemon::paths::proc_restart_marker("rst01", 1).is_file());
    assert!(crate::daemon::consume_proc_restart("rst01", 1));
    assert!(!crate::daemon::consume_proc_restart("rst01", 1), "consume is once");
    // The teardown failure cannot turn a pending restart into an ordinary failed attempt.
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    assert!(handle_api_post(
      "/api/v1/proc/finish",
      r#"{"session":"rst01","proc":1,"status":"fail","fail_reason":"container_run_failed","detail":"killed","elapsed":2}"#,
      &store,
      &prune,
    ));
    {
      let guard = store.lock().unwrap();
      let restarted = &guard.sessions.get("rst01").unwrap().procs[1];
      assert_eq!(restarted.status, ProcStatus::Running);
      assert_eq!(restarted.fail_reason.as_deref(), Some(crate::failure::reason::RESTART_REQUESTED));
    }
    // Idempotent while the replacement is pending.
    let (status2, body2, mutated2) = proc_restart_response(r#"{"session":"rst01","proc":1}"#, &store);
    assert_eq!(status2, 200);
    assert!(!mutated2);
    assert!(body2.contains("already_requested"));

    // The runner registers the replacement with an explicit lineage edge. That one event
    // finalizes the old attempt and makes both forward/backward navigation deterministic.
    assert!(handle_api_post(
      "/api/v1/proc/add",
      r#"{"session":"rst01","proc":3,"label":"claude: review-1 (retry)","kind":"skill","skill_name":"review-1","harness":"claude","previous_attempt":1}"#,
      &store,
      &prune,
    ));
    {
      let guard = store.lock().unwrap();
      let session = guard.sessions.get("rst01").unwrap();
      let old = session.procs.iter().find(|p| p.index == 1).unwrap();
      let replacement = session.procs.iter().find(|p| p.index == 3).unwrap();
      assert_eq!(old.fail_reason.as_deref(), Some(crate::failure::reason::FORCE_RESTARTED));
      assert_eq!(replacement.previous_attempt, Some(1));
      assert_eq!(session.proc_next_attempt(old).map(|p| p.index), Some(3));
      assert_eq!(session.proc_first_attempt(replacement).index, 1);
    }
    assert!(!handle_api_post(
      "/api/v1/proc/add",
      r#"{"session":"rst01","proc":4,"label":"claude: duplicate replacement","kind":"skill","skill_name":"review-1","harness":"claude","previous_attempt":1}"#,
      &store,
      &prune,
    ));

    // Idempotent once linked; builds have no respawn path; unknown → 404.
    let (status_linked, body_linked, mutated_linked) = proc_restart_response(r#"{"session":"rst01","proc":1}"#, &store);
    assert_eq!(status_linked, 200);
    assert!(!mutated_linked);
    assert!(body_linked.contains("already_ended"));
    let (status3, body3, _) = proc_restart_response(r#"{"session":"rst01","proc":2}"#, &store);
    assert_eq!(status3, 400, "got: {body3}");
    let (status4, _, _) = proc_restart_response(r#"{"session":"rst01","proc":9}"#, &store);
    assert_eq!(status4, 404);

    // If a valid result wins the teardown race, the runner consumes the marker instead of
    // spawning a replacement; the original attempt is therefore allowed to finish cleanly.
    let (race_status, race_body, _) = proc_restart_response(r#"{"session":"rst01","proc":0}"#, &store);
    assert_eq!(race_status, 200, "got: {race_body}");
    assert!(handle_api_post(
      "/api/v1/proc/finish",
      r#"{"session":"rst01","proc":0,"status":"ok","detail":"valid result won","elapsed":3}"#,
      &store,
      &prune,
    ));
    {
      let guard = store.lock().unwrap();
      let won = &guard.sessions.get("rst01").unwrap().procs[0];
      assert_eq!(won.status, ProcStatus::Ok);
      assert!(won.fail_reason.is_none());
    }
    assert!(crate::daemon::consume_proc_restart("rst01", 0));

    // A gone run client means nothing is left to act on the marker → refused.
    {
      let mut guard = store.lock().unwrap();
      guard.sessions.get_mut("rst01").unwrap().client_connected = false;
    }
    let (status5, body5, _) = proc_restart_response(r#"{"session":"rst01","proc":3}"#, &store);
    assert_eq!(status5, 409, "got: {body5}");
    assert!(!crate::daemon::paths::proc_restart_marker("rst01", 3).exists(), "no marker without a runner");
  }

  #[test]
  fn projects_create_scaffolds_a_runnable_repo_and_bare_names_open_it() {
    let _env = crate::runtime::test_env_lock();
    let home = std::env::temp_dir().join(format!("scsh-projects-test-{}", crate::runtime::random_nonce_6()));
    let prev = std::env::var_os("SCSH_HOME");
    std::env::set_var("SCSH_HOME", &home);
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));

    // Create: the project is born runnable — one commit, /tmp gitignored, clean.
    let (status, body, mutated) = projects_create_response(r#"{"name":"demo-1"}"#, &store);
    assert_eq!(status, 200, "got: {body}");
    assert!(mutated);
    assert!(body.contains(r#""created":true"#) && body.contains(r#""runnable":true"#), "got: {body}");
    let path = crate::daemon::paths::projects_dir().join("demo-1");
    assert!(path.join(".git").is_dir() && path.join("tmp").is_dir());
    assert!(std::fs::read_to_string(path.join(".gitignore")).unwrap().contains("/tmp"));
    let log = crate::git_command().args(["-C", &path.to_string_lossy(), "log", "--format=%s|%an"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), format!("Init.|{}", crate::SCSH_COMMIT_NAME));

    // A bare (slash-free) name in the open box resolves under $SCSH_HOME/projects/.
    let (status2, body2, _) = repos_open_response(r#"{"path":"demo-1"}"#, &store);
    assert_eq!(status2, 200, "got: {body2}");
    assert!(body2.contains(r#""runnable":true"#) && body2.contains("demo-1"), "got: {body2}");

    // Same name again → open the existing project (idempotent; HTTP 200, created:false).
    let (status3, body3, _) = projects_create_response(r#"{"name":"demo-1"}"#, &store);
    assert_eq!(status3, 200, "got: {body3}");
    assert!(body3.contains(r#""ok":true"#) && body3.contains(r#""created":false"#), "got: {body3}");
    assert!(body3.contains("demo-1") && body3.contains(r#""runnable":true"#), "got: {body3}");

    // Hostile names are rejected before any filesystem work (no dots, no slashes).
    for bad in [
      r#"{"name":"../escape"}"#,
      r#"{"name":"a/b"}"#,
      r#"{"name":".hidden"}"#,
      r#"{"name":"foo.bar"}"#,
      r#"{"name":""}"#,
    ] {
      let (st, b, _) = projects_create_response(bad, &store);
      assert_eq!(st, 400, "{bad} got: {b}");
    }

    match prev {
      Some(v) => std::env::set_var("SCSH_HOME", v),
      None => std::env::remove_var("SCSH_HOME"),
    }
    let _ = std::fs::remove_dir_all(&home);
  }

  #[test]
  fn harness_stop_kills_only_that_harness_across_live_sessions() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let proc = |index: usize, harness: &str| ProcRecord {
      index,
      previous_attempt: None,
      label: format!("{harness}: review"),
      kind: ProcKind::Skill,
      status: ProcStatus::Running,
      skill_name: Some("review".into()),
      harness: Some(harness.into()),
      model: None,
      started_at: Some(50),
      note: None,
      detail: None,
      fail_reason: None,
      elapsed: None,
      lines: Vec::new(),
      container_name: None, // no live container — avoid a 2s stop_container sleep in the unit test
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
    };
    let now = now_unix_secs();
    let session = |id: &str, procs: Vec<ProcRecord>, last_seen_at: u64| Session {
      id: id.into(),
      started_at: last_seen_at.saturating_sub(10),
      ended_at: None,
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs,
      last_seen_at,
      client_connected: true,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    };
    {
      let mut s = store.lock().unwrap();
      s.insert_session("hs01".into(), session("hs01", vec![proc(0, "grok"), proc(1, "opencode")], now));
      s.insert_session("hs02".into(), session("hs02", vec![proc(0, "grok")], now));
      // A zombie: its client died without deregistering — un-ended, procs "running", but
      // last seen ages ago. It must be invisible to a harness-wide stop.
      s.insert_session("hs99".into(), session("hs99", vec![proc(0, "grok")], 50));
    }
    let snapshots = Mutex::new(Vec::new());
    let (status, body, mutated) = harness_stop_response_notifying(r#"{"harness":"grok"}"#, &store, || {
      let guard = store.lock().unwrap();
      let states = ["hs01", "hs02"]
        .iter()
        .map(|id| {
          let p = &guard.sessions.get(*id).unwrap().procs[0];
          (p.status, p.fail_reason.clone())
        })
        .collect::<Vec<_>>();
      snapshots.lock().unwrap().push(states);
    });
    assert_eq!(status, 200, "got: {body}");
    assert!(mutated);
    assert!(body.contains(r#""stopped":2"#), "got: {body}");
    let snapshots = snapshots.into_inner().unwrap();
    assert!(
      snapshots[0].iter().all(|(status, reason)| *status == ProcStatus::Running
        && reason.as_deref() == Some(crate::failure::reason::STOP_REQUESTED)),
      "the first published state must be terminating: {:?}",
      snapshots[0]
    );
    assert!(
      snapshots.last().unwrap().iter().all(|(status, reason)| {
        *status == ProcStatus::Fail && reason.as_deref() == Some(crate::failure::reason::FORCE_STOPPED)
      }),
      "the final published state must be stopped: {:?}",
      snapshots.last()
    );
    {
      let guard = store.lock().unwrap();
      let s1 = guard.sessions.get("hs01").unwrap();
      // grok died, opencode keeps running, the session itself stays live.
      assert!(s1.ended_at.is_none());
      assert_eq!(s1.procs[0].status, ProcStatus::Fail);
      assert_eq!(s1.procs[0].fail_reason.as_deref(), Some(crate::failure::reason::FORCE_STOPPED));
      assert_eq!(s1.procs[1].status, ProcStatus::Running);
      assert_eq!(guard.sessions.get("hs02").unwrap().procs[0].status, ProcStatus::Fail);
      // The zombie is untouched — no container to stop, no history rewritten.
      assert_eq!(guard.sessions.get("hs99").unwrap().procs[0].status, ProcStatus::Running);
    }
    // Nothing of that harness left → ok, zero stopped, no mutation.
    let (status2, body2, mutated2) = harness_stop_response(r#"{"harness":"grok"}"#, &store);
    assert_eq!(status2, 200);
    assert!(!mutated2);
    assert!(body2.contains(r#""stopped":0"#), "got: {body2}");
  }

  #[test]
  fn session_stop_unknown_session_is_404() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let (status, body, mutated) = session_stop_response(r#"{"session":"nosuch"}"#, &store);
    assert_eq!(status, 404);
    assert!(!mutated);
    assert!(body.contains("not found"));
  }

  #[test]
  fn session_stop_cancels_supervision_even_on_an_already_ended_job() {
    // A stub-spawned supervised job can die (and settle) before the user's stop arrives.
    // The already_ended early return must STILL cancel supervision — otherwise the
    // supervisor resurrects a job the human explicitly killed. Found live by
    // RESILIENCE-DEMO.md step 5.
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    store.lock().unwrap().insert_session(
      "endedd".into(),
      Session {
        id: "endedd".into(),
        started_at: 50,
        ended_at: Some(60),
        profile: Some("greet".into()),
        kind: Some("workflow".into()),
        repo: "/r".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: 60,
        client_connected: false,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: crate::daemon::model::SupervisorState {
          next_retry_at: Some(1000),
          ..crate::daemon::model::SupervisorState::fresh(crate::daemon::model::DEFAULT_JOB_RETRIES)
        },
      },
    );
    let (status, body, mutated) = session_stop_response(r#"{"session":"endedd"}"#, &store);
    assert_eq!(status, 200);
    assert!(body.contains("already_ended"), "got: {body}");
    assert!(mutated, "the supervision cancel must persist");
    let guard = store.lock().unwrap();
    let sup = &guard.sessions["endedd"].supervisor;
    assert_eq!(sup.gave_up.as_deref(), Some("stopped from the session browser"));
    assert!(sup.next_retry_at.is_none(), "no restart stays scheduled");
  }

  #[test]
  fn session_start_records_run_pid() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    let body = r#"{"session":"pidabc","repo":"/r","branch":"main","profile":"default","skills":[],"run_pid":4242}"#;
    assert!(handle_api_post("/api/v1/session/start", body, &store, &prune));
    assert_eq!(store.lock().unwrap().sessions.get("pidabc").unwrap().run_pid, Some(4242));
  }

  #[test]
  fn session_start_stamps_the_retries_budget_and_explicit_beats_daemon_default() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    // A CLI-started run with no --retries gets the default budget: every job is
    // presumed worth finishing.
    let body = r#"{"session":"cliabc","repo":"/r","branch":"main","profile":"default","skills":[]}"#;
    assert!(handle_api_post("/api/v1/session/start", body, &store, &prune));
    {
      let guard = store.lock().unwrap();
      let sup = &guard.sessions["cliabc"].supervisor;
      assert_eq!(sup.retries, crate::daemon::model::DEFAULT_JOB_RETRIES);
      assert_eq!(sup.attempt(), 1);
    }
    // Re-registering WITHOUT the field (a daemon-spawned child) keeps the daemon-set
    // budget; an explicit `scsh run --retries 0` overrides it — including down to zero.
    store.lock().unwrap().sessions.get_mut("cliabc").unwrap().supervisor.retries = 3;
    let body = r#"{"session":"cliabc","repo":"/r","branch":"main","profile":"default","skills":[]}"#;
    assert!(handle_api_post("/api/v1/session/start", body, &store, &prune));
    assert_eq!(store.lock().unwrap().sessions["cliabc"].supervisor.retries, 3, "absent field never demotes");
    let body = r#"{"session":"cliabc","repo":"/r","branch":"main","profile":"default","skills":[],"retries":0}"#;
    assert!(handle_api_post("/api/v1/session/start", body, &store, &prune));
    assert_eq!(store.lock().unwrap().sessions["cliabc"].supervisor.retries, 0, "explicit 0 opts out");
  }

  #[test]
  fn display_or_absolute_repo_keeps_labels_and_absolutizes_paths() {
    assert_eq!(display_or_absolute_repo(""), "");
    assert_eq!(display_or_absolute_repo("(image builds)"), "(image builds)");
    assert_eq!(display_or_absolute_repo("(internal)"), "(internal)");
    // An absolute path survives (canonicalization is best-effort; /tmp may resolve to a symlink
    // target, so assert it is still absolute rather than byte-equal).
    assert!(display_or_absolute_repo("/tmp").starts_with('/'));
  }

  #[test]
  fn parse_string_array_reads_strings_and_skips_junk() {
    let obj = match parse(r#"{"harnesses":["claude","codex",7,null,"grok"]}"#).unwrap() {
      Value::Object(o) => o,
      _ => panic!("object"),
    };
    assert_eq!(parse_string_array(&obj, "harnesses"), vec!["claude", "codex", "grok"]);
    assert!(parse_string_array(&obj, "missing").is_empty());
  }

  #[test]
  fn images_build_rejects_unknown_harness() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let (status, body, mutated) = images_build_response(r#"{"harnesses":["fancyharness"]}"#, &store);
    assert_eq!(status, 400);
    assert!(body.contains("unknown image 'fancyharness'"), "body: {body}");
    assert!(!mutated);
    assert!(store.lock().unwrap().sessions.is_empty(), "no session pre-created on rejection");
  }

  #[test]
  fn images_build_rejects_concurrent_build_session() {
    let now = now_unix_secs();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, now)));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "bldabc".into(),
        Session {
          id: "bldabc".into(),
          started_at: now,
          ended_at: None,
          profile: Some(BUILD_IMAGES_PROFILE.into()),
          kind: None,
          repo: "(image builds)".into(),
          branch: String::new(),
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
    }
    let (status, body, mutated) = images_build_response("{}", &store);
    assert_eq!(status, 409);
    assert!(body.contains("already running"), "body: {body}");
    assert!(!mutated);
  }

  #[test]
  fn images_build_spawns_and_precreates_session() {
    // A sleeping stub stands in for scsh so the "build" stays alive while we assert the
    // pre-created session (an instant-exit stub would be reconciled to ended before we look).
    let stub = std::env::temp_dir().join(format!("scsh-build-sleeper-{}.sh", crate::runtime::random_nonce_6()));
    std::fs::write(&stub, "#!/bin/sh\nsleep 5\n").unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    std::env::set_var("SCSH_BIN", &stub);
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let (status, body, mutated) =
      images_build_response(r#"{"harnesses":["claude"],"rebuild_base":true,"force":true}"#, &store);
    std::env::remove_var("SCSH_BIN");
    assert_eq!(status, 200, "body: {body}");
    assert!(mutated);
    let session_id = body.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();
    let guard = store.lock().unwrap();
    let session = guard.sessions.get(&session_id).expect("session pre-created");
    assert_eq!(session.profile.as_deref(), Some(BUILD_IMAGES_PROFILE));
    assert_eq!(session.repo, "(image builds)");
    assert!(session.ended_at.is_none());
    assert!(session.run_pid.is_some(), "build PID recorded for Force stop");
    drop(guard);
    std::fs::remove_file(&stub).ok();
  }

  #[test]
  fn images_build_reconciles_a_silent_startup_failure() {
    // Instant-exit stub with no registration → session must end as failed, not stay "running".
    std::env::set_var("SCSH_BIN", "/usr/bin/false");
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let (status, body, _) = images_build_response(r#"{"harnesses":["claude"]}"#, &store);
    std::env::remove_var("SCSH_BIN");
    assert_eq!(status, 200, "body: {body}");
    let session_id = body.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();
    // Reap thread may need a beat to wait() + reconcile.
    for _ in 0..50 {
      let ended = store.lock().unwrap().sessions.get(&session_id).and_then(|s| s.ended_at).is_some();
      if ended {
        break;
      }
      std::thread::sleep(Duration::from_millis(20));
    }
    let guard = store.lock().unwrap();
    let session = guard.sessions.get(&session_id).expect("session");
    assert!(session.ended_at.is_some(), "startup failure must end the session");
    assert_eq!(session.procs.len(), 1);
    assert_eq!(session.procs[0].label, "build failed to start");
    assert_eq!(session.procs[0].status, ProcStatus::Fail);
  }

  #[test]
  fn chapters_sidecar_path_derives_from_cast() {
    let p = chapters_sidecar_path("/a/b/foo-123-utc-xyz.cast").unwrap();
    assert_eq!(p.to_string_lossy(), "/a/b/foo-123-utc-xyz.chapters.json");
    assert!(chapters_sidecar_path("/a/b/not-a-cast.txt").is_none());
  }

  #[test]
  fn proc_cast_registers_and_cast_endpoint_serves_partial_file() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let prune = Arc::new(Mutex::new(PruneQueue::default()));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "castab".into(),
        Session {
          id: "castab".into(),
          started_at: 50,
          ended_at: None,
          profile: None,
          kind: None,
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: vec![ProcRecord {
            index: 0,
            previous_attempt: None,
            label: "claude: add".into(),
            kind: ProcKind::Skill,
            status: ProcStatus::Running,
            skill_name: None,
            harness: None,
            model: None,
            started_at: Some(50),
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
          client_connected: true,
          run_pid: None,
          workflow: None,
          parent_session: None,
          supervisor: Default::default(),
        },
      );
    }
    // Before registration: 404.
    let (status, _, _) = cast_response("/cast/castab/0", &store);
    assert_eq!(status, 404);

    // Register a cast path; write a partially-flushed asciicast (last line incomplete).
    let path = std::env::temp_dir().join(format!("scsh-test-cast-{}.cast", std::process::id()));
    let header = r#"{"version": 3, "term": {"cols": 200, "rows": 50}}"#;
    std::fs::write(&path, format!("{header}\n[0.1, \"o\", \"hello\"]\n[0.2, \"o\", \"trunc")).unwrap();
    let body = format!(r#"{{"session":"castab","proc":0,"path":{}}}"#, crate::json::quote(&path.to_string_lossy()));
    assert!(handle_api_post("/api/v1/proc/cast", &body, &store, &prune));
    assert_eq!(
      store.lock().unwrap().sessions.get("castab").unwrap().procs[0].cast_path.as_deref(),
      Some(path.to_string_lossy().as_ref())
    );

    // Inline fetch: 200, truncated to the last complete line, no disposition.
    let (status, served, disposition) = cast_response("/cast/castab/0", &store);
    assert_eq!(status, 200);
    assert_eq!(served, format!("{header}\n[0.1, \"o\", \"hello\"]\n"));
    assert!(disposition.is_none());

    // Download variant carries an attachment disposition with a stable filename.
    let (status, _, disposition) = cast_response("/cast/castab/0?dl=1", &store);
    assert_eq!(status, 200);
    assert_eq!(disposition.as_deref(), Some("attachment; filename=\"scsh-castab-p0.cast\""));

    // Unknown session/proc and vanished files are 404s, not errors.
    assert_eq!(cast_response("/cast/nosuch/0", &store).0, 404);
    assert_eq!(cast_response("/cast/castab/9", &store).0, 404);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(cast_response("/cast/castab/0", &store).0, 404);

    // The commits-diff endpoint mirrors the cast one: 404 until the run posts a packed
    // page, then the self-contained HTML inline (renders in a tab) or as a download.
    assert_eq!(diff_response("/diff/castab/0", &store).0, 404);
    let diff = std::env::temp_dir().join(format!("scsh-test-diff-{}.html", std::process::id()));
    std::fs::write(&diff, "<html>packed diff</html>").unwrap();
    let body = format!(r#"{{"session":"castab","proc":0,"path":{}}}"#, crate::json::quote(&diff.to_string_lossy()));
    assert!(handle_api_post("/api/v1/proc/diff", &body, &store, &prune));
    assert_eq!(
      store.lock().unwrap().sessions.get("castab").unwrap().procs[0].diff_path.as_deref(),
      Some(diff.to_string_lossy().as_ref())
    );
    let (status, served, disposition) = diff_response("/diff/castab/0", &store);
    assert_eq!(status, 200);
    assert_eq!(served, "<html>packed diff</html>");
    assert!(disposition.is_none(), "inline by default — the page renders in a new tab");
    let (status, _, disposition) = diff_response("/diff/castab/0?dl=1", &store);
    assert_eq!(status, 200);
    assert_eq!(disposition.as_deref(), Some("attachment; filename=\"scsh-job-castab-p0-diff.html\""));
    assert_eq!(diff_response("/diff/nosuch/0", &store).0, 404);
    assert_eq!(diff_response("/diff/castab/9", &store).0, 404);
    std::fs::remove_file(&diff).unwrap();
    assert_eq!(diff_response("/diff/castab/0", &store).0, 404);
  }

  #[test]
  fn export_endpoint_serves_the_offline_page_as_an_attachment() {
    let dir = std::env::temp_dir().join(format!("scsh-export-endpoint-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast_path = dir.join("rec.cast");
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    {
      let mut s = store.lock().unwrap();
      s.insert_session(
        "expabc".into(),
        Session {
          id: "expabc".into(),
          started_at: 50,
          ended_at: None,
          profile: None,
          kind: None,
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: vec![ProcRecord {
            index: 0,
            previous_attempt: None,
            label: "claude: add".into(),
            kind: ProcKind::Skill,
            status: ProcStatus::Ok,
            skill_name: None,
            harness: None,
            model: None,
            started_at: Some(50),
            note: None,
            detail: None,
            fail_reason: None,
            elapsed: Some(2.0),
            lines: Vec::new(),
            container_name: None,
            container_runtime: None,
            cast_path: Some(cast_path.to_string_lossy().into_owned()),
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
        },
      );
    }
    // Unknown session/proc → the existing 404 style.
    assert_eq!(export_response("/cast/nosuch/0/export.html", &store).0, 404);
    assert_eq!(export_response("/cast/expabc/9/export.html", &store).0, 404);
    // A registered cast whose file is not on disk yet → 404.
    assert_eq!(export_response("/cast/expabc/0/export.html", &store).0, 404);
    // A header with no complete frames yet → 404 with an actionable body.
    std::fs::write(&cast_path, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n").unwrap();
    let (status, body, disposition) = export_response("/cast/expabc/0/export.html", &store);
    assert_eq!(status, 404);
    assert!(body.contains("no recorded frames yet"), "body: {body}");
    assert!(disposition.is_none());
    // Frames + a sidecar → the self-contained page, served as `<stem>.html`.
    std::fs::write(&cast_path, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.5,\"o\",\"hello\\r\\n\"]\n")
      .unwrap();
    std::fs::write(
      dir.join("rec.chapters.json"),
      r#"{"summary":"Ran the demo.","chapters":[{"t":0,"title":"Start"}]}"#,
    )
    .unwrap();
    let (status, page, disposition) = export_response("/cast/expabc/0/export.html", &store);
    assert_eq!(status, 200);
    assert_eq!(disposition.as_deref(), Some("attachment; filename=\"rec.html\""));
    assert!(page.contains("<title>rec</title>"), "cast stem is the title");
    assert!(
      page.contains("BeeCastPlayer") && !page.contains("@license"),
      "first-party player, no third-party attribution"
    );
    assert!(page.contains("\"title\":\"Start\""), "sidecar chapter folded in");
    // A malformed sidecar exports without chapters — a warning path, never an error.
    std::fs::write(dir.join("rec.chapters.json"), "{ not json").unwrap();
    let (status, page, _) = export_response("/cast/expabc/0/export.html", &store);
    assert_eq!(status, 200);
    assert!(!page.contains("\"chapters\":["), "malformed sidecar → chapterless export");
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// A skill proc for the session-export tests, recorded (`cast_path`) or not.
  fn export_test_proc(index: usize, label: &str, cast_path: Option<String>) -> ProcRecord {
    ProcRecord {
      index,
      previous_attempt: None,
      label: label.into(),
      kind: ProcKind::Skill,
      status: ProcStatus::Ok,
      skill_name: None,
      harness: None,
      model: None,
      started_at: Some(50),
      note: None,
      detail: None,
      fail_reason: None,
      elapsed: Some(2.0),
      lines: Vec::new(),
      container_name: None,
      container_runtime: None,
      cast_path,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
    }
  }

  fn store_with_export_session(id: &str, procs: Vec<ProcRecord>) -> Arc<Mutex<Store>> {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    store.lock().unwrap().insert_session(
      id.into(),
      Session {
        id: id.into(),
        started_at: 50,
        ended_at: Some(60),
        profile: Some("default".into()),
        kind: None,
        repo: "/r".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs,
        last_seen_at: 60,
        client_connected: false,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    store
  }

  #[test]
  fn live_reconcile_ends_a_job_when_its_owner_pid_disappears() {
    let mut restarted = export_test_proc(0, "cursor: prepare", None);
    restarted.status = ProcStatus::Fail;
    restarted.fail_reason = Some(crate::failure::reason::RESTART_REQUESTED.into());
    let mut downstream = export_test_proc(1, "claude: review", None);
    downstream.status = ProcStatus::Waiting;
    downstream.started_at = None;
    downstream.elapsed = None;
    let store = store_with_export_session("deadlive", vec![restarted, downstream]);
    let mut guard = store.lock().unwrap();
    let session = guard.sessions.get_mut("deadlive").unwrap();
    session.ended_at = None;
    session.client_connected = true;
    session.run_pid = Some(u32::MAX / 2);

    assert_eq!(settle_dead_run_pids(&mut guard, 100), vec!["deadlive"]);
    let session = &guard.sessions["deadlive"];
    assert_eq!(session.ended_at, Some(100));
    assert!(!session.client_connected);
    assert!(session.run_pid.is_none());
    assert_eq!(session.procs[0].fail_reason.as_deref(), Some(crate::failure::reason::FORCE_RESTARTED));
    assert_eq!(session.procs[1].fail_reason.as_deref(), Some(crate::failure::reason::SESSION_END_INCOMPLETE));
    assert_eq!(session.lifecycle_status(100), crate::daemon::model::SessionLifecycle::Cancelled);
  }

  #[test]
  fn chapters_response_names_the_live_summarizing_job_while_the_sidecar_is_pending() {
    let now = now_unix_secs();
    let dir = std::env::temp_dir().join(format!("scsh-chap-job-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("add-20260711-114749-utc-ufakca.cast");
    std::fs::write(&cast, "{\"version\":3}\n[0.5,\"o\",\"hi\"]\n").unwrap();
    let cast_path = cast.to_string_lossy().into_owned();
    let store = store_with_export_session("srcjob", vec![export_test_proc(0, "claude: add", Some(cast_path.clone()))]);
    // No annotator proc means no invented annotation state.
    let (status, body) = chapters_response("/cast/srcjob/0/chapters", &store);
    assert_eq!(status, 200);
    assert_eq!(body, "{}");
    // A LIVE annotate proc covering this cast names its job — the id the pending
    // "summarizing…" note on the job page links to.
    let mut annotate = export_test_proc(0, "annotate · add-20260711-114749-utc-ufakca", None);
    annotate.kind = ProcKind::Annotate;
    annotate.status = ProcStatus::Running;
    annotate.annotate_target = Some(cast_path.clone());
    store.lock().unwrap().insert_session(
      "annjob".into(),
      Session {
        id: "annjob".into(),
        started_at: now,
        ended_at: None,
        profile: Some("annotate".into()),
        kind: Some("annotate".into()),
        repo: INTERNAL_REPO.into(),
        branch: String::new(),
        skills: Vec::new(),
        procs: vec![annotate],
        last_seen_at: now,
        client_connected: true,
        run_pid: None,
        workflow: None,
        parent_session: Some("srcjob".into()),
        supervisor: Default::default(),
      },
    );
    let (status, body) = chapters_response("/cast/srcjob/0/chapters", &store);
    assert_eq!(status, 200);
    assert_eq!(body, r#"{ "annotation_job": "annjob", "annotation_proc": 0, "annotation_status": "running" }"#);
    // Regression: the old model treated 30 seconds without a session event as a terminal
    // failure even after the annotation proc had started. Running work gets the 30-minute
    // idle allowance, so a quiet annotator remains running here.
    store.lock().unwrap().sessions.get_mut("annjob").unwrap().last_seen_at = now - 31;
    let (_, body) = chapters_response("/cast/srcjob/0/chapters", &store);
    assert_eq!(body, r#"{ "annotation_job": "annjob", "annotation_proc": 0, "annotation_status": "running" }"#);
    // The match also holds across path spellings: a standalone `scsh annotate-cast` may
    // register a relative argument while the run registered the absolute path — the
    // nonce-stamped file name is the shared key.
    store.lock().unwrap().sessions.get_mut("annjob").unwrap().procs[0].annotate_target =
      Some("casts/add-20260711-114749-utc-ufakca.cast".into());
    let (_, body) = chapters_response("/cast/srcjob/0/chapters", &store);
    assert_eq!(body, r#"{ "annotation_job": "annjob", "annotation_proc": 0, "annotation_status": "running" }"#);
    // A job that ended with a non-terminal proc is cancelled. The stale proc must not keep
    // animated "annotating..." UI alive forever in the source player or workflow graph.
    store.lock().unwrap().sessions.get_mut("annjob").unwrap().ended_at = Some(now + 1);
    let (_, body) = chapters_response("/cast/srcjob/0/chapters", &store);
    assert_eq!(body, r#"{ "annotation_job": "annjob", "annotation_proc": 0, "annotation_status": "fail" }"#);
    store.lock().unwrap().sessions.get_mut("annjob").unwrap().ended_at = None;
    // Finished state remains linked instead of disappearing.
    store.lock().unwrap().sessions.get_mut("annjob").unwrap().procs[0].status = ProcStatus::Ok;
    let (_, body) = chapters_response("/cast/srcjob/0/chapters", &store);
    assert_eq!(body, r#"{ "annotation_job": "annjob", "annotation_proc": 0, "annotation_status": "ok" }"#);
    // Once the sidecar lands, chapters and the durable relationship coexist.
    std::fs::write(
      dir.join("add-20260711-114749-utc-ufakca.chapters.json"),
      r#"{"summary":"ok","chapters":[{"t":0,"title":"Start"}]}"#,
    )
    .unwrap();
    let (_, body) = chapters_response("/cast/srcjob/0/chapters", &store);
    assert!(body.contains("\"chapters\""), "sidecar content served: {body}");
    assert!(body.contains("\"annotation_status\": \"ok\""), "completed annotation stays linked: {body}");
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// Every `srcdoc="…"` attribute value in the page. `esc` turns every embedded `"` into
  /// `&quot;`, so the first literal quote after `srcdoc="` is the attribute terminator.

  #[test]
  fn session_export_assembles_every_recording_into_one_page() {
    let dir = std::env::temp_dir().join(format!("scsh-session-export-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    // Proc 0: a recording WITH a chapters sidecar. Proc 1: a bare recording. Proc 2: no cast.
    let cast0 = dir.join("rec0.cast");
    std::fs::write(&cast0, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.5,\"o\",\"hello\\r\\n\"]\n")
      .unwrap();
    std::fs::write(
      dir.join("rec0.chapters.json"),
      r#"{"summary":"Ran the demo.","chapters":[{"t":0,"title":"Start"}]}"#,
    )
    .unwrap();
    let cast1 = dir.join("rec1.cast");
    std::fs::write(&cast1, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[1.0,\"o\",\"done\\r\\n\"]\n").unwrap();
    let store = store_with_export_session(
      "sexabc",
      vec![
        export_test_proc(0, "claude: add", Some(cast0.to_string_lossy().into_owned())),
        export_test_proc(1, "codex: multiply", Some(cast1.to_string_lossy().into_owned())),
        export_test_proc(2, "cursor: skipped", None),
      ],
    );
    let (status, page, disposition) = session_export_response("/job/sexabc/export.html", &store);
    assert_eq!(status, 200);
    assert_eq!(disposition.as_deref(), Some("attachment; filename=\"scsh-job-sexabc.html\""));
    // The header: the page says JOB, wears the live page's purple island, and carries the
    // job's metadata.
    assert!(page.contains("<title>scsh job sexabc</title>"), "job title");
    assert!(!page.contains("scsh session"), "the word is job, not session");
    assert!(page.contains("card--accent-left-purple"), "live-page island");
    assert!(!page.contains(r#"class="session-kind""#), "export island has no session-kind");
    assert!(page.contains(r#"<dl class="session-meta">"#), "export keeps session-meta");
    assert!(page.contains(r#"<main class="page-shell">"#), "offline export keeps the live page's centered width");
    assert!(page.contains(r#"<code class="repo-path">/r</code>"#), "repo label");
    for label in ["claude: add", "codex: multiply", "cursor: skipped"] {
      assert!(page.contains(label), "a section for {label}");
    }
    // ONE shared player bundle, one mounted player per recording — no iframes, no
    // per-cast page copies; recordings ride inline in the boot script's JSON.
    assert!(!page.contains("<iframe"), "no iframes — the players mount from inline data");
    assert_eq!(page.matches("Player.prototype.append").count(), 1, "exactly one player bundle");
    assert_eq!(page.matches("\"cast\":").count(), 2, "one inline recording per exportable cast");
    assert!(page.contains("BeeCastPlayer.create"), "the boot script mounts the first-party player");
    assert!(!page.contains("fullscreenEl: box"), "fullscreen contains only the player, like live");
    assert!(!page.contains("@license"), "no third-party attribution anywhere in the assembled page");
    // Every proc section is the live page's details.proc row, open by default.
    assert_eq!(page.matches("<details open class=\"chamfer proc").count(), 3, "one collapsible row per proc");
    assert_eq!(page.matches("<span class=\"triangle\"").count(), 3, "live-page row chrome");
    assert!(page.contains("data:image/svg+xml"), "inline favicon");
    // The annotated cast contributes its chapter chip and its one-sentence summary.
    assert!(page.contains("Start"), "chapter title folded in");
    assert!(page.contains(r#"<div class="cast-summary">Ran the demo.</div>"#), "sidecar summary shown");
    // The cast-less proc degrades to a styled note row, never an error.
    assert!(page.contains(r#"<div class="detail dim">no recording — skipped/failed before output</div>"#));
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn session_export_escapes_hostile_cast_payloads() {
    let dir = std::env::temp_dir().join(format!("scsh-session-export-esc-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    // A recording whose output tries `"` `&` `<` and a literal `</iframe>` breakout.
    let cast = dir.join("evil.cast");
    std::fs::write(
      &cast,
      "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.1,\"o\",\"x\\\" & <b> </iframe><script>alert(1)</script>\"]\n",
    )
    .unwrap();
    let store = store_with_export_session(
      "hostil",
      vec![export_test_proc(0, "claude: evil", Some(cast.to_string_lossy().into_owned()))],
    );
    let (status, page, _) = session_export_response("/job/hostil/export.html", &store);
    assert_eq!(status, 200);
    // The recording rides inside a JSON string in the boot script, with every `</`
    // escaped as `<\/` — a literal `</script>` in the cast can neither terminate the
    // script block nor become live markup.
    assert!(!page.contains("<script>alert(1)</script>"), "script payload must not go live");
    assert!(page.contains("<\\/iframe>"), "the payload's closing tags are JSON-escaped");
    assert!(page.contains("<\\/script>"), "the payload's </script> is JSON-escaped");
    assert_eq!(page.matches("</script>").count(), 2, "only the page's own two script blocks close");
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn session_export_without_recordings_keeps_the_job_snapshot_useful() {
    // A frameless cast (header only) and a cast-less proc still export as explanatory rows.
    let dir = std::env::temp_dir().join(format!("scsh-session-export-404-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("frameless.cast");
    std::fs::write(&cast, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n").unwrap();
    let store = store_with_export_session(
      "nocast",
      vec![
        export_test_proc(0, "claude: add", Some(cast.to_string_lossy().into_owned())),
        export_test_proc(1, "codex: multiply", None),
      ],
    );
    let (status, body, disposition) = session_export_response("/job/nocast/export.html", &store);
    assert_eq!(status, 200);
    assert!(body.contains("no recording"), "body: {body}");
    assert_eq!(disposition.as_deref(), Some("attachment; filename=\"scsh-job-nocast.html\""));
    // Unknown job: the existing 404 style. Legacy /session/… alias still works.
    assert_eq!(session_export_response("/job/nosuch/export.html", &store).0, 404);
    assert_eq!(session_export_response("/session/nosuch/export.html", &store).0, 404);
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn server_reload_keeps_sessions_but_resets_runtime_state() {
    // An explicit temp-file DB (via `with_db`) — no `SCSH_HOME`, no real ~/.scsh, no global env
    // race with other tests.
    let db_path = std::env::temp_dir().join(format!("scsh-reload-{}.redb", crate::runtime::random_nonce_6()));
    let port = 7274;

    // First instance: register a session, persist it to redb, then drop (releases the DB lock).
    {
      let db = crate::daemon::db::StoreDb::open_path(&db_path).unwrap();
      let server = Server::with_db(DaemonMode::Persistent, port, Some(db));
      {
        let mut store = lock_store(&server.store);
        store.insert_session(
          "sessaa".into(),
          Session {
            id: "sessaa".into(),
            started_at: 1,
            ended_at: None,
            profile: None,
            kind: None,
            repo: "/r".into(),
            branch: "main".into(),
            skills: Vec::new(),
            procs: Vec::new(),
            last_seen_at: 1,
            client_connected: true,
            run_pid: None,
            workflow: None,
            parent_session: None,
            supervisor: Default::default(),
          },
        );
      }
      server.dirty_sessions.lock().unwrap().insert("sessaa".into());
      server.persist_now();
    }

    // Second instance on the same DB: the session reloads, but the daemon's own runtime state
    // is fresh (started_at ~ now, no client connected).
    let before = now_unix_secs();
    let db = crate::daemon::db::StoreDb::open_path(&db_path).unwrap();
    let server2 = Server::with_db(DaemonMode::Persistent, port, Some(db));
    {
      let store = lock_store(&server2.store);
      assert!(store.sessions.contains_key("sessaa"), "session reloaded from redb");
      assert!(store.started_at >= before, "started_at refreshed on reload");
      assert!(!store.sessions["sessaa"].client_connected, "reload marks clients disconnected");
    }
    drop(server2);
    let _ = std::fs::remove_file(&db_path);
  }

  #[test]
  fn server_reload_ends_sessions_whose_run_pid_is_already_dead() {
    let db_path = std::env::temp_dir().join(format!("scsh-reload-dead-{}.redb", crate::runtime::random_nonce_6()));
    let port = 7274;
    {
      let db = crate::daemon::db::StoreDb::open_path(&db_path).unwrap();
      let server = Server::with_db(DaemonMode::Persistent, port, Some(db));
      {
        let mut store = lock_store(&server.store);
        store.insert_session(
          "deadpid".into(),
          Session {
            id: "deadpid".into(),
            started_at: 10,
            ended_at: None,
            profile: Some("smoke".into()),
            kind: Some("definition".into()),
            repo: "/r".into(),
            branch: "main".into(),
            skills: Vec::new(),
            procs: vec![ProcRecord {
              index: 0,
              previous_attempt: None,
              label: "skill".into(),
              kind: ProcKind::Skill,
              status: ProcStatus::Waiting,
              skill_name: Some("s".into()),
              harness: Some("grok".into()),
              model: None,
              started_at: None,
              note: Some("waiting for image build…".into()),
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
            last_seen_at: 20,
            client_connected: false,
            // PIDs wrap; 2^31-1 is almost never a live process on the test host.
            run_pid: Some(u32::MAX / 2),
            workflow: None,
            parent_session: None,
            supervisor: Default::default(),
          },
        );
      }
      server.dirty_sessions.lock().unwrap().insert("deadpid".into());
      server.persist_now();
    }
    let db = crate::daemon::db::StoreDb::open_path(&db_path).unwrap();
    let server2 = Server::with_db(DaemonMode::Persistent, port, Some(db));
    {
      let store = lock_store(&server2.store);
      let s = store.sessions.get("deadpid").expect("reloaded");
      assert!(s.ended_at.is_some(), "dead run_pid must end the session on reload");
      assert!(s.run_pid.is_none(), "run_pid cleared after orphan end");
      assert_eq!(s.lifecycle_status(now_unix_secs()), SessionLifecycle::Cancelled);
      assert_eq!(s.procs[0].status, ProcStatus::Fail, "a dead owner cannot leave a running/waiting proc behind");
      let mut annotation = s.clone();
      annotation.procs[0].kind = ProcKind::Annotate;
      annotation.procs[0].status = ProcStatus::Running;
      annotation.procs[0].fail_reason = None;
      settle_loaded_incomplete_procs(&mut annotation);
      assert_eq!(annotation.procs[0].status, ProcStatus::Fail);
      assert_eq!(
        annotation.procs[0].fail_reason.as_deref(),
        Some(crate::failure::reason::ANNOTATION_INTERRUPTED),
        "orphaned annotations persist a terminal interruption, never stale running"
      );
    }
    drop(server2);
    let _ = std::fs::remove_file(&db_path);
  }

  #[test]
  fn read_request_rejects_oversized_content_length() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      read_request(&mut server)
    });
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    let huge = MAX_HTTP_BODY + 1;
    client
      .write_all(format!("POST /api/v1/ping HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {huge}\r\n\r\n").as_bytes())
      .unwrap();
    drop(client);
    assert!(handle.join().unwrap().is_err());
  }

  // ---- harness-definition endpoints ---------------------------------------------------

  /// A fresh temp dir that is a *runnable* git repo: committed, clean, and with `tmp/` gitignored.
  fn clean_repo(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("scsh-repos-{tag}-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
      assert!(crate::git_command().args(args).current_dir(&dir).status().unwrap().success());
    };
    git(&["init", "-q", "."]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(dir.join(".gitignore"), "/tmp\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    dir
  }

  #[test]
  fn repos_open_rejects_a_non_repo() {
    let dir = std::env::temp_dir().join(format!("scsh-nonrepo-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let body = format!(r#"{{"path":{}}}"#, quote(&dir.to_string_lossy()));
    let (status, out, _) = repos_open_response(&body, &store);
    assert_eq!(status, 200);
    assert!(out.contains("\"ok\":false") && out.contains("not a git repository"), "got: {out}");
    std::fs::remove_dir_all(&dir).ok();
  }

  /// Install (and commit) a stub big-beautiful-build skill into a test repo: its built-in
  /// def references a deliberately UNBUNDLED skill and is only discoverable once the skill
  /// is installed — in the repo here, so the test never depends on the developer machine's
  /// machine-wide install.
  fn commit_bbb_skill(dir: &std::path::Path) {
    let skill = dir.join(".skills/big-beautiful-build");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# big-beautiful-build\n\nDeliver the FEATURE completely.\n").unwrap();
    let git = |args: &[&str]| {
      assert!(crate::git_command().args(args).current_dir(dir).status().unwrap().success());
    };
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "install big-beautiful-build"]);
  }

  #[test]
  fn repos_open_lists_builtin_defs_for_a_clean_repo() {
    let dir = clean_repo("open");
    commit_bbb_skill(&dir);
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let body = format!(r#"{{"path":{}}}"#, quote(&dir.to_string_lossy()));
    let (status, out, mutated) = repos_open_response(&body, &store);
    assert_eq!(status, 200, "got: {out}");
    assert!(mutated);
    assert!(out.contains("\"ok\":true") && out.contains("\"clean\":true"), "got: {out}");
    assert!(
      out.contains("doctor") && out.contains("add") && out.contains("research") && out.contains("big-beautiful-build"),
      "built-ins listed; got: {out}"
    );
    assert!(out.contains(r#""name":"FEATURE","type":"text""#), "multiline feature intake is typed; got: {out}");
    // The repo is remembered as open.
    assert_eq!(store.lock().unwrap().open_repos.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_start_spawns_and_guards_one_per_repo() {
    // A sleeping stub stands in for the scsh binary: it stays alive during the test so the
    // spawned "job" is still running when the guard is checked (a real run takes seconds; an
    // instant-exit stub would be reconciled to ended before the second call, defeating the test).
    let dir = clean_repo("start");
    // The stub lives OUTSIDE the repo, or it would dirty the tree and fail the readiness check.
    let stub = std::env::temp_dir().join(format!("scsh-sleeper-{}.sh", crate::runtime::random_nonce_6()));
    std::fs::write(&stub, "#!/bin/sh\nsleep 5\n").unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    // Isolated $SCSH_HOME: starting a job persists its restart recipe under sessions/.
    let home = std::env::temp_dir().join(format!("scsh-shome-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&home).unwrap();
    let repo = dir.to_string_lossy().into_owned();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let body = format!(r#"{{"repo":{},"def":"add","params":{{"A":"2","B":"3"}}}}"#, quote(&repo));

    with_scsh_home(&home, || {
      std::env::set_var("SCSH_BIN", &stub);
      let (status, out, mutated) = jobs_start_response(&body, &store);
      assert_eq!(status, 200, "got: {out}");
      assert!(mutated && out.contains("\"ok\":true"), "got: {out}");
      let session_id = out.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();
      {
        let guard = store.lock().unwrap();
        let session = guard.sessions.get(&session_id).expect("session pre-created");
        assert_eq!(session.profile.as_deref(), Some("add"), "session labeled with the definition name");
        assert!(!session.repo.is_empty(), "session carries the repo path");
        // No limbo: the planned tasks are on the session immediately (add's four agents).
        assert_eq!(session.skills.len(), 4, "planned skills shown at once, before the run registers");
        assert!(session.skills.iter().any(|s| s.harness == "claude"), "agents listed by harness");
        assert!(session.skills.iter().all(|s| s.name.starts_with("add-")), "flat routes named {{def}}-{{route}}");
      }

      // A second start in the same repo is refused while the first job is still running.
      let (status2, out2, _) = jobs_start_response(&body, &store);
      assert_eq!(status2, 409, "got: {out2}");
      assert!(out2.contains("already running"), "got: {out2}");
      std::env::remove_var("SCSH_BIN");
    });

    std::fs::remove_file(&stub).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  /// An isolated `$SCSH_HOME` whose global manifest declares ONE named profile
  /// (`hello-fleet`, two routes) plus a profile-less skill (i.e. "default" — which must
  /// never surface as a globally startable profile).
  fn global_home_with_profile(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("scsh-ghome-{tag}-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&home).unwrap();
    let yml = "skills:\n\
      \x20 greeter:\n\
      \x20   profile: hello-fleet\n\
      \x20   result: tmp/greeter-{name}.json\n\
      \x20   invocations:\n\
      \x20     claude-sonnet:\n\
      \x20       harness: claude\n\
      \x20       model: sonnet\n\
      \x20     codex-luna:\n\
      \x20       harness: codex\n\
      \x20       model: gpt-5.6-luna\n\
      \x20 chore:\n\
      \x20   harness: claude\n\
      \x20   result: tmp/chore.json\n";
    std::fs::write(home.join(".scsh.yml"), yml).unwrap();
    home
  }

  /// Run `f` with `$SCSH_HOME` pointing at `home`, restoring the previous value after —
  /// under the env lock, since Rust tests share one process.
  fn with_scsh_home<T>(home: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let _guard = crate::runtime::test_env_lock();
    let prev = std::env::var_os("SCSH_HOME");
    std::env::set_var("SCSH_HOME", home);
    let out = f();
    match prev {
      Some(v) => std::env::set_var("SCSH_HOME", v),
      None => std::env::remove_var("SCSH_HOME"),
    }
    out
  }

  #[test]
  fn global_profiles_ride_the_defs_responses() {
    let home = global_home_with_profile("list");
    let dir = clean_repo("glist");
    let body = format!(r#"{{"repo":{}}}"#, quote(&dir.to_string_lossy()));
    let (status, out) = with_scsh_home(&home, || harness_defs_response(&body));
    assert_eq!(status, 200, "got: {out}");
    assert!(out.contains(r#""global":[{"name":"hello-fleet""#), "named global profiles are listed: {out}");
    assert!(
      out.contains(r#""route":"greeter-claude-sonnet","agent":"claude","model":"sonnet""#),
      "each route rides with its agent and model: {out}"
    );
    assert!(out.contains(r#""agent":"codex","model":"gpt-5.6-luna""#), "all routes listed: {out}");
    assert!(
      !out.contains(r#"{"name":"default""#),
      "the default profile is never a global card — it is always the repo's own: {out}"
    );

    // The open-repo reply carries the same list, so the run page needs no second fetch.
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let open_body = format!(r#"{{"path":{}}}"#, quote(&dir.to_string_lossy()));
    let (status, out, _) = with_scsh_home(&home, || repos_open_response(&open_body, &store));
    assert_eq!(status, 200, "got: {out}");
    assert!(out.contains(r#""global":[{"name":"hello-fleet""#), "repos/open lists global profiles: {out}");

    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_start_runs_a_global_profile() {
    let home = global_home_with_profile("start");
    let dir = clean_repo("gstart");
    let repo = dir.to_string_lossy().into_owned();
    let stub = std::env::temp_dir().join(format!("scsh-sleeper-{}.sh", crate::runtime::random_nonce_6()));
    std::fs::write(&stub, "#!/bin/sh\nsleep 5\n").unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let body = format!(r#"{{"repo":{},"profile":"hello-fleet"}}"#, quote(&repo));
    let (status, out, mutated) = with_scsh_home(&home, || {
      std::env::set_var("SCSH_BIN", &stub);
      let out = jobs_start_response(&body, &store);
      std::env::remove_var("SCSH_BIN");
      out
    });
    assert_eq!(status, 200, "got: {out}");
    assert!(mutated && out.contains("\"ok\":true"), "got: {out}");
    let session_id = out.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();
    {
      let guard = store.lock().unwrap();
      let session = guard.sessions.get(&session_id).expect("session pre-created");
      assert_eq!(session.profile.as_deref(), Some("hello-fleet"), "session labeled with the profile name");
      assert!(session.kind.is_none(), "a profile run is kind 'profile' (the default)");
      // No limbo: the profile's routes are the planned tasks, named {skill}-{route}.
      let names: Vec<&str> = session.skills.iter().map(|s| s.name.as_str()).collect();
      assert_eq!(names, ["greeter-claude-sonnet", "greeter-codex-luna"], "planned tasks are the profile's routes");
    }
    std::fs::remove_file(&stub).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_start_rejects_bad_profile_requests() {
    let home = std::env::temp_dir().join(format!("scsh-ghome-empty-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&home).unwrap(); // no global manifest at all
    let dir = clean_repo("gbad");
    let repo = dir.to_string_lossy().into_owned();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));

    // Unknown profile: nothing declares it, and the error teaches the install command.
    let body = format!(r#"{{"repo":{},"profile":"nope"}}"#, quote(&repo));
    let (status, out, mutated) = with_scsh_home(&home, || jobs_start_response(&body, &store));
    assert_eq!(status, 400, "got: {out}");
    assert!(!mutated && out.contains("installskills --global"), "the fix rides in the error: {out}");

    // "default" is always the repo's own — never startable as a global profile.
    let body = format!(r#"{{"repo":{},"profile":"default"}}"#, quote(&repo));
    let (status, out, _) = with_scsh_home(&home, || jobs_start_response(&body, &store));
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("default profile"), "got: {out}");

    // A definition AND a profile (or neither) is ambiguous.
    let body = format!(r#"{{"repo":{},"def":"add","profile":"hello-fleet"}}"#, quote(&repo));
    let (status, out, _) = jobs_start_response(&body, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("exactly one"), "got: {out}");
    let body = format!(r#"{{"repo":{}}}"#, quote(&repo));
    let (status, out, _) = jobs_start_response(&body, &store);
    assert_eq!(status, 400, "got: {out}");

    assert!(store.lock().unwrap().sessions.is_empty(), "nothing spawned");
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_restart_respawns_the_same_job() {
    let home = std::env::temp_dir().join(format!("scsh-rhome-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&home).unwrap();
    let dir = clean_repo("restart");
    let repo = dir.to_string_lossy().into_owned();
    let stub = std::env::temp_dir().join(format!("scsh-sleeper-{}.sh", crate::runtime::random_nonce_6()));
    std::fs::write(&stub, "#!/bin/sh\nsleep 5\n").unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));

    let (old_id, new_id) = with_scsh_home(&home, || {
      std::env::set_var("SCSH_BIN", &stub);
      // Start `add` from the web, params included; the recipe lands in the session dir.
      let body = format!(r#"{{"repo":{},"def":"add","params":{{"A":"7","B":"9"}}}}"#, quote(&repo));
      let (status, out, _) = jobs_start_response(&body, &store);
      assert_eq!(status, 200, "got: {out}");
      let old_id = out.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();
      let recipe = std::fs::read_to_string(session_start_recipe_path(&old_id)).expect("recipe persisted");
      assert!(recipe.contains(r#""def":"add""#) && recipe.contains(r#""A":"7""#), "recipe: {recipe}");

      // Restart: the old job is force-stopped and the SAME def respawns with the SAME params.
      let body = format!(r#"{{"session":{}}}"#, quote(&old_id));
      let (status, out, mutated) = jobs_restart_response(&body, &store);
      assert_eq!(status, 200, "got: {out}");
      assert!(mutated && out.contains("\"ok\":true"), "got: {out}");
      let new_id = out.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();
      std::env::remove_var("SCSH_BIN");
      (old_id, new_id)
    });
    assert_ne!(old_id, new_id, "the restart answers with the NEW job's id");
    {
      let guard = store.lock().unwrap();
      let old = guard.sessions.get(&old_id).unwrap();
      assert!(old.ended_at.is_some(), "the old job is stopped, never left running");
      let new = guard.sessions.get(&new_id).expect("the new session is pre-created");
      assert_eq!(new.profile.as_deref(), Some("add"), "same definition");
      assert_eq!(new.skills.len(), 4, "same planned tasks");
      assert!(new.ended_at.is_none(), "the new job is live");
    }
    std::fs::remove_file(&stub).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_restart_resume_passes_the_old_session_to_the_respawn() {
    let nonce = crate::runtime::random_nonce_6();
    let home = std::env::temp_dir().join(format!("scsh-rhome-resume-{nonce}"));
    std::fs::create_dir_all(&home).unwrap();
    let dir = clean_repo("restart-resume");
    let repo = dir.to_string_lossy().into_owned();
    // The stub captures its argv so the test can see exactly what a resume respawn runs.
    let argfile = std::env::temp_dir().join(format!("scsh-resume-args-{nonce}.txt"));
    let stub = std::env::temp_dir().join(format!("scsh-sleeper-{nonce}.sh"));
    std::fs::write(&stub, format!("#!/bin/sh\necho \"$@\" > {}\nsleep 5\n", argfile.display())).unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));

    let (old_id, new_id) = with_scsh_home(&home, || {
      std::env::set_var("SCSH_BIN", &stub);
      // `greet` is a built-in WORKFLOW definition (defaulted params), so resume applies.
      // Started with an explicit retries budget, which must survive the restart.
      let body = format!(r#"{{"repo":{},"def":"greet","retries":3}}"#, quote(&repo));
      let (status, out, _) = jobs_start_response(&body, &store);
      assert_eq!(status, 200, "got: {out}");
      let old_id = out.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();

      let body = format!(r#"{{"session":{},"mode":"resume"}}"#, quote(&old_id));
      let (status, out, mutated) = jobs_restart_response(&body, &store);
      assert_eq!(status, 200, "got: {out}");
      assert!(mutated && out.contains("\"ok\":true"), "got: {out}");
      let new_id = out.split("\"session\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap().to_string();
      std::env::remove_var("SCSH_BIN");
      (old_id, new_id)
    });
    assert_ne!(old_id, new_id, "resume answers with the NEW job's id");
    {
      let guard = store.lock().unwrap();
      let old = guard.sessions.get(&old_id).unwrap();
      assert_eq!(old.supervisor.retries, 3, "jobs/start recorded the retries budget");
      assert_eq!(old.supervisor.restarted_as.as_deref(), Some(new_id.as_str()), "the chain is linked");
      assert!(old.supervisor.gave_up.is_none(), "being replaced is not giving up");
      let new = guard.sessions.get(&new_id).unwrap();
      assert_eq!(new.supervisor.retries, 3, "the fresh session inherits the budget");
      assert_eq!(new.supervisor.attempt(), 2, "…one attempt later");
      assert!(new.supervisor.next_retry_at.is_none(), "…with the schedule cleared");
    }
    // The respawned stub writes its argv asynchronously — poll briefly for it.
    let mut args = String::new();
    for _ in 0..50 {
      args = std::fs::read_to_string(&argfile).unwrap_or_default();
      if args.contains("--resume-from") {
        break;
      }
      std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(args.contains("run --def greet"), "the same workflow def respawns: {args}");
    assert!(args.contains(&format!("--resume-from {old_id}")), "the fresh run resumes the OLD session: {args}");
    assert!(args.contains(&format!("--session {new_id}")), "…under the NEW session id: {args}");

    // Resume is workflow-only: a flat job's routes are independent, so the mode is refused.
    store.lock().unwrap().insert_session(
      "flatjb".into(),
      Session {
        id: "flatjb".into(),
        started_at: 50,
        ended_at: Some(60),
        profile: Some("add".into()),
        kind: None,
        repo: repo.clone(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: 60,
        client_connected: false,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    let (status, out, _) = jobs_restart_response(r#"{"session":"flatjb","mode":"resume"}"#, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("workflow"), "the refusal explains the workflow-only rule: {out}");

    // An unknown mode is a caller bug, not a silent scratch restart.
    let (status, out, _) = jobs_restart_response(r#"{"session":"flatjb","mode":"sideways"}"#, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("unknown restart mode"), "got: {out}");

    std::fs::remove_file(&stub).ok();
    std::fs::remove_file(&argfile).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_restart_falls_back_to_the_name_for_cli_runs() {
    // A CLI-started session has no start.json — the daemon never saw its env. Its stored
    // def/profile name alone restarts it (params-free; `add`'s params all have defaults).
    let home = std::env::temp_dir().join(format!("scsh-rhome-cli-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&home).unwrap();
    let dir = clean_repo("restart-cli");
    let repo = dir.to_string_lossy().into_owned();
    let stub = std::env::temp_dir().join(format!("scsh-sleeper-{}.sh", crate::runtime::random_nonce_6()));
    std::fs::write(&stub, "#!/bin/sh\nsleep 5\n").unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    store.lock().unwrap().insert_session(
      "cliadd".into(),
      Session {
        id: "cliadd".into(),
        started_at: 50,
        ended_at: None,
        profile: Some("add".into()),
        kind: None,
        repo: repo.clone(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: now_unix_secs(),
        client_connected: true,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    let (status, out, _) = with_scsh_home(&home, || {
      std::env::set_var("SCSH_BIN", &stub);
      let out = jobs_restart_response(r#"{"session":"cliadd"}"#, &store);
      std::env::remove_var("SCSH_BIN");
      out
    });
    assert_eq!(status, 200, "got: {out}");
    let guard = store.lock().unwrap();
    assert!(guard.sessions.get("cliadd").unwrap().ended_at.is_some(), "the stuck CLI job is stopped");
    assert_eq!(guard.sessions.len(), 2, "a fresh job took its place");
    std::fs::remove_file(&stub).ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_restart_refuses_what_it_cannot_replay() {
    let home = std::env::temp_dir().join(format!("scsh-rhome-no-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&home).unwrap();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));

    // Unknown session.
    let (status, out, _) = jobs_restart_response(r#"{"session":"nosuch"}"#, &store);
    assert_eq!(status, 404, "got: {out}");

    // An image build is not a repository job — nothing to replay.
    store.lock().unwrap().insert_session(
      "buildx".into(),
      Session {
        id: "buildx".into(),
        started_at: 50,
        ended_at: None,
        profile: Some("build-images".into()),
        kind: Some("build".into()),
        repo: IMAGE_BUILDS_REPO.into(),
        branch: String::new(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: now_unix_secs(),
        client_connected: true,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    let (status, out, _) = jobs_restart_response(r#"{"session":"buildx"}"#, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("only repository jobs"), "got: {out}");

    // A CLI-started run of a definition with an unmet required param cannot be replayed
    // faithfully — the daemon never saw its env — so it refuses with the missing param.
    let dir = clean_repo("restart-park");
    store.lock().unwrap().insert_session(
      "clires".into(),
      Session {
        id: "clires".into(),
        started_at: 50,
        ended_at: None,
        profile: Some("research".into()),
        kind: None,
        repo: dir.to_string_lossy().into_owned(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: now_unix_secs(),
        client_connected: true,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    let (status, out, _) = with_scsh_home(&home, || jobs_restart_response(r#"{"session":"clires"}"#, &store));
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("CITY"), "the missing param is named: {out}");
    let ended = store.lock().unwrap().sessions.get("clires").unwrap().ended_at;
    assert!(ended.is_none(), "a refused restart leaves the old run alone — stop only after the respawn validates");
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_start_rejects_a_missing_required_param() {
    let dir = clean_repo("reqparam");
    let repo = dir.to_string_lossy().into_owned();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    // `research` requires CITY; omitting it is a 400 before anything spawns.
    let body = format!(r#"{{"repo":{},"def":"research","params":{{}}}}"#, quote(&repo));
    let (status, out, mutated) = jobs_start_response(&body, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(!mutated && out.contains("CITY"), "got: {out}");
    assert!(store.lock().unwrap().sessions.is_empty(), "nothing spawned");
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_start_rejects_an_empty_feature_brief() {
    let dir = clean_repo("empty-feature");
    commit_bbb_skill(&dir);
    let repo = dir.to_string_lossy().into_owned();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let body = format!(r#"{{"repo":{},"def":"big-beautiful-build","params":{{"FEATURE":"  \n "}}}}"#, quote(&repo));
    let (status, out, mutated) = jobs_start_response(&body, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(!mutated && out.contains("FEATURE") && out.contains("must not be empty"), "got: {out}");
    assert!(store.lock().unwrap().sessions.is_empty(), "nothing spawned");
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_start_rejects_a_dirty_repo() {
    let dir = clean_repo("dirty");
    std::fs::write(dir.join("scratch.txt"), "uncommitted").unwrap();
    let repo = dir.to_string_lossy().into_owned();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let body = format!(r#"{{"repo":{},"def":"add","params":{{}}}}"#, quote(&repo));
    let (status, out, _) = jobs_start_response(&body, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("not ready") && out.contains("uncommitted"), "got: {out}");
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn jobs_start_rejects_a_repo_without_a_gitignored_scratch() {
    // A committed, clean repo that does NOT gitignore tmp/ or .harness/tmp — the exact case that
    // used to spawn a doomed run and leave a hidden "running" session.
    let dir = std::env::temp_dir().join(format!("scsh-noscratch-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| assert!(crate::git_command().args(args).current_dir(&dir).status().unwrap().success());
    git(&["init", "-q", "."]);
    git(&["config", "user.email", "t@e.com"]);
    git(&["config", "user.name", "t"]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    let repo = dir.to_string_lossy().into_owned();
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    let body = format!(r#"{{"repo":{},"def":"add","params":{{}}}}"#, quote(&repo));
    let (status, out, _) = jobs_start_response(&body, &store);
    assert_eq!(status, 400, "got: {out}");
    assert!(out.contains("gitignored scratch"), "got: {out}");
    assert!(store.lock().unwrap().sessions.is_empty(), "no session created for a doomed job");
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn reconcile_marks_an_orphaned_job_failed_with_its_error() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    // A pre-created session whose spawned run died before it registered any proc.
    store.lock().unwrap().insert_session(
      "orphan".into(),
      Session {
        id: "orphan".into(),
        started_at: 50,
        ended_at: None,
        profile: Some("add".into()),
        kind: None,
        repo: "/r".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: 50,
        client_connected: false,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    reconcile_finished_job(&store, "orphan", Some(1), "✗ /tmp is not gitignored in this repository");
    let guard = store.lock().unwrap();
    let s = guard.sessions.get("orphan").unwrap();
    assert!(s.ended_at.is_some(), "the orphaned job is ended, not left running");
    assert_eq!(s.procs.len(), 1, "a failure row is surfaced");
    assert_eq!(s.procs[0].status, ProcStatus::Fail);
    assert!(s.procs[0].detail.as_deref().unwrap().contains("gitignored"), "the real error is shown");
    assert_eq!(s.lifecycle_status(60), SessionLifecycle::Failed, "shows as failed, never hidden");
  }

  #[test]
  fn reconcile_leaves_a_cleanly_finished_job_alone() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
    store.lock().unwrap().insert_session(
      "done".into(),
      Session {
        id: "done".into(),
        started_at: 50,
        ended_at: Some(55), // already deregistered
        profile: Some("add".into()),
        kind: None,
        repo: "/r".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: 55,
        client_connected: false,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    reconcile_finished_job(&store, "done", Some(0), "");
    let guard = store.lock().unwrap();
    assert!(guard.sessions.get("done").unwrap().procs.is_empty(), "no synthetic row added to a finished job");
  }

  #[test]
  fn repos_json_groups_sessions_by_directory() {
    let mut store = Store::new(DaemonMode::Persistent, 7274, 50);
    store.open_repo(OpenRepo { path: "/work/a".into(), opened_at: 50, clean: true });
    store.insert_session(
      "aaaaaa".into(),
      Session {
        id: "aaaaaa".into(),
        started_at: 50,
        ended_at: None,
        profile: Some("add".into()),
        kind: None,
        repo: "/work/a".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs: Vec::new(),
        last_seen_at: 50,
        client_connected: false,
        run_pid: None,
        workflow: None,
        parent_session: None,
        supervisor: Default::default(),
      },
    );
    let out = repos_json(&store, 51);
    assert!(out.contains("/work/a") && out.contains("aaaaaa") && out.contains("\"running\""), "got: {out}");
  }
}
