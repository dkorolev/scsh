//! HTTP server for the session browser daemon.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::castprobe::{cast_probe_snapshot, probe_growth_messages, CastProbe};
use super::db::StoreDb;
use super::html;
use super::jsonio::{field_bool, field_num, field_str, tick_json, tick_json_light};
use super::model::{
  DaemonMode, OutputLine, ProcKind, ProcRecord, ProcStatus, Session, SessionLifecycle, SkillMeta, Store,
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

    let addr = format!("127.0.0.1:{}", self.port);
    let listener = TcpListener::bind(&addr)?;
    listener.set_nonblocking(true)?;
    let pid_path = pid_file(self.port);
    std::fs::write(&pid_path, std::process::id().to_string())?;

    let mut last_ws_tick = Instant::now();
    // Per-proc incremental cast probes (parse offsets cached across ticks) — see `castprobe`.
    let mut cast_probes: std::collections::HashMap<(String, usize), CastProbe> = std::collections::HashMap::new();

    loop {
      match listener.accept() {
        Ok((stream, _)) => {
          let store = Arc::clone(&self.store);
          let prune = Arc::clone(&self.prune);
          let dirty = Arc::clone(&self.dirty);
          let ws_dirty = Arc::clone(&self.ws_dirty);
          let dirty_sessions = Arc::clone(&self.dirty_sessions);
          let ws_hub = Arc::clone(&self.ws_hub);
          std::thread::spawn(move || {
            let (mutated, session_id) = catch_unwind(AssertUnwindSafe(|| {
              handle_connection(stream, &store, &prune, &ws_hub).unwrap_or((false, None))
            }))
            .unwrap_or((false, None));
            if mutated {
              dirty.store(true, Ordering::Relaxed);
              ws_dirty.store(true, Ordering::Relaxed);
              if let Some(id) = session_id {
                dirty_sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
              }
            }
          });
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(e),
      }

      self.persist_if_due();
      self.prune_if_due();

      if last_ws_tick.elapsed() >= WS_TICK {
        let now = now_unix_secs();
        let include_sessions = self.ws_dirty.load(Ordering::Relaxed);
        // The snapshot of casts to probe is taken under the store lock; the file stats and
        // tail-parses below run with the lock released, and only when someone is listening.
        let probe_casts = self.ws_hub.client_count() > 0;
        let (json, casts) = {
          let mut store = lock_store(&self.store);
          store.reconcile(now);
          let json = if include_sessions { tick_json(&*store, now) } else { tick_json_light(&*store, now) };
          (json, if probe_casts { cast_probe_snapshot(&store) } else { Vec::new() })
        };
        self.ws_hub.broadcast(json);
        if probe_casts {
          for msg in probe_growth_messages(&casts, &mut cast_probes) {
            self.ws_hub.broadcast(msg);
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
        .filter_map(|id| store.sessions.get(&id).map(|s| (id, crate::daemon::jsonio::session_json_api(s))))
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

/// Handle one request. Returns `(mutated, session_id)`: `mutated` drives the persist + WS
/// refresh, and `session_id` (extracted from a mutating POST body) is the one session to
/// write through to the store DB — so a mutation persists just that session, not the store.
fn handle_connection(
  mut stream: TcpStream, store: &Arc<Mutex<Store>>, prune: &Arc<Mutex<PruneQueue>>, ws_hub: &Arc<Hub>,
) -> std::io::Result<(bool, Option<String>)> {
  // Accepted sockets inherit the listener's non-blocking mode on macOS; block for reads.
  stream.set_nonblocking(false)?;
  stream.set_read_timeout(Some(Duration::from_secs(5)))?;
  let req = read_request(&mut stream)?;
  if websocket::wants_upgrade(&req.method, &req.path, &req.headers) {
    websocket::accept_handshake(&mut stream, &req.headers)?;
    let rx = ws_hub.subscribe();
    websocket::serve(stream, rx);
    return Ok((false, None));
  }
  let bare_path = req.path.split('?').next().unwrap_or("");
  if req.method == "GET" && req.path.starts_with("/cast/") && bare_path.ends_with("/chapters") {
    let (status, body) = chapters_response(bare_path, store);
    write_response(&mut stream, status, &body, "application/json")?;
    return Ok((false, None));
  }
  if req.method == "GET" && req.path.starts_with("/cast/") && bare_path.ends_with("/export.html") {
    let (status, body, disposition) = export_response(bare_path, store);
    write_download_response(&mut stream, status, &body, "text/html; charset=utf-8", disposition.as_deref())?;
    return Ok((false, None));
  }
  if req.method == "GET" && req.path.starts_with("/session/") && bare_path.ends_with("/export.html") {
    let (status, body, disposition) = session_export_response(bare_path, store);
    write_download_response(&mut stream, status, &body, "text/html; charset=utf-8", disposition.as_deref())?;
    return Ok((false, None));
  }
  if req.method == "GET" && req.path.starts_with("/cast/") && !bare_path.ends_with("/play") {
    let (status, body, disposition) = cast_response(&req.path, store);
    write_download_response(
      &mut stream,
      status,
      &body,
      "application/x-asciicast; charset=utf-8",
      disposition.as_deref(),
    )?;
    return Ok((false, None));
  }
  let (status, body, content_type, mutated) = route(&req, store, prune);
  write_response(&mut stream, status, &body, content_type)?;
  let session_id = if mutated { mutated_session_id(&req) } else { None };
  Ok((mutated, session_id))
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

/// `GET /session/<id>/export.html` — EVERY recording of the session assembled into ONE
/// self-contained offline HTML page, served as a download attachment named
/// `scsh-session-<id>.html`. Each recording embeds as the exact per-cast export page
/// ([`crate::export::render_page_from_texts`]) in an attribute-escaped `<iframe srcdoc>`
/// — see [`html::session_export_page`] for the composition rationale. Procs with no cast
/// or no frames become note rows, never errors; a session with ZERO exportable casts is a
/// 404 with an actionable body (only a hand-typed URL sees it — the session-page button
/// renders only when a proc has a registered cast).
fn session_export_response(bare_path: &str, store: &Arc<Mutex<Store>>) -> (u16, String, Option<String>) {
  let id = bare_path.strip_prefix("/session/").unwrap_or("").strip_suffix("/export.html").unwrap_or("");
  // Clone the session under the lock, then do all file I/O (casts + sidecars) unlocked.
  let Some(session) = lock_store(store).sessions.get(id).cloned() else {
    return (404, "session not found".into(), None);
  };
  let exports: Vec<html::CastExport> = session.procs.iter().map(gather_proc_export).collect();
  if !exports.iter().any(|e| matches!(e, html::CastExport::Page { .. })) {
    return (404, "no exportable recordings in this session — retry once a skill's recording has output".into(), None);
  }
  let page = html::session_export_page(&session, &exports);
  (200, page, Some(format!("attachment; filename=\"scsh-session-{id}.html\"")))
}

/// One proc's contribution to the session export: the rendered per-cast page (frames on
/// disk → the same rendering `/cast/…/export.html` serves, sidecar summary alongside), or
/// the note explaining why there is nothing to embed. Never an error — a vanished file, a
/// frameless cast, and a proc that was never recorded all degrade to notes.
fn gather_proc_export(proc: &ProcRecord) -> html::CastExport {
  const NO_RECORDING: &str = "no recording — skipped/failed before output";
  let Some(cast_path) = proc.cast_path.as_deref() else {
    let note = match proc.kind {
      ProcKind::Build => "no recording — image-build step (text log only)",
      ProcKind::Skill => NO_RECORDING,
    };
    return html::CastExport::Note(note.into());
  };
  let Ok(ndjson) = read_complete_cast_lines(cast_path) else {
    return html::CastExport::Note(NO_RECORDING.into());
  };
  if !ndjson.lines().any(|l| l.trim_start().starts_with('[')) {
    return html::CastExport::Note(NO_RECORDING.into());
  }
  let sidecar = chapters_sidecar_path(cast_path).and_then(|p| std::fs::read_to_string(p).ok());
  let stem = crate::export::cast_stem(std::path::Path::new(cast_path));
  match crate::export::render_page_from_texts(&ndjson, sidecar.as_deref(), &stem) {
    Ok(page) => {
      let summary = sidecar.as_deref().and_then(crate::annotate::parse_annotation).map(|a| a.summary);
      html::CastExport::Page { page, summary }
    }
    Err(_) => html::CastExport::Note(NO_RECORDING.into()),
  }
}

/// `GET /cast/<session>/<proc>/chapters` — the cast's analysis sidecar
/// (`{ "summary": …, "chapters": [{ "t", "title" }] }`), written next to the cast file by
/// the cursor/Composer analysis pass as `<cast-basename>.chapters.json`. Returns `{}` when
/// no sidecar exists yet, so the player can ask unconditionally.
fn chapters_response(bare_path: &str, store: &Arc<Mutex<Store>>) -> (u16, String) {
  let rest = bare_path.strip_prefix("/cast/").unwrap_or("").strip_suffix("/chapters").unwrap_or("");
  let Some((session_id, proc_index)) = parse_cast_route(rest) else {
    return (404, "{}".into());
  };
  let sidecar = proc_cast_path(store, session_id, proc_index).and_then(|c| chapters_sidecar_path(&c));
  match sidecar.and_then(|p| std::fs::read_to_string(p).ok()) {
    Some(json) => (200, json),
    None => (200, "{}".into()),
  }
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
}

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

  let header_end = header_end.unwrap_or(buf.len());
  let text = String::from_utf8_lossy(&buf[..header_end]);
  let mut lines = text.split("\r\n");
  let first = lines.next().unwrap_or("");
  let parts: Vec<&str> = first.split_whitespace().collect();
  let method = parts.first().unwrap_or(&"GET").to_string();
  let path = parts.get(1).unwrap_or(&"/").to_string();

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
  Ok(HttpRequest { method, path, body, headers })
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
  req: &HttpRequest, store: &Arc<Mutex<Store>>, prune: &Arc<Mutex<PruneQueue>>,
) -> (u16, String, &'static str, bool) {
  // The images-build endpoint returns a custom body (the spawned session id), so it does not
  // go through the generic `{"ok":…}` POST handler.
  if req.method == "POST" && req.path == "/api/v1/images/build" {
    let (status, body, mutated) = images_build_response(&req.body, store);
    return (status, body, "application/json", mutated);
  }
  if req.method == "POST" && req.path.starts_with("/api/v1/") {
    let ok = handle_api_post(&req.path, &req.body, store, prune);
    let body = if ok { "{\"ok\":true}" } else { "{\"ok\":false}" };
    return (if ok { 200 } else { 400 }, body.to_string(), "application/json", ok);
  }
  if req.method != "GET" {
    return (405, "method not allowed".into(), "text/plain", false);
  }
  match req.path.as_str() {
    "/" => {
      let html = html::index_page(&*lock_store(store));
      (200, html, "text/html; charset=utf-8", false)
    }
    path if path.starts_with("/session/") => {
      let id = path.strip_prefix("/session/").unwrap_or("");
      let store = lock_store(store);
      if let Some(page) = html::session_page(&*store, id) {
        (200, page, "text/html; charset=utf-8", false)
      } else {
        (404, "session not found".into(), "text/plain", false)
      }
    }
    "/assets/asciinema-player.js" => (200, html::PLAYER_JS.to_string(), "application/javascript; charset=utf-8", false),
    "/assets/asciinema-player.css" => (200, html::PLAYER_CSS.to_string(), "text/css; charset=utf-8", false),
    path if path.starts_with("/cast/") && path.ends_with("/play") => {
      let rest = path.strip_prefix("/cast/").unwrap_or("").strip_suffix("/play").unwrap_or("");
      let page = rest.split_once('/').and_then(|(sid, proc)| {
        let proc_index = proc.parse::<usize>().ok()?;
        html::cast_player_page(&*lock_store(store), sid, proc_index)
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
    "/api/v1/images" => (200, images_json(), "application/json", false),
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
      let skills = parse_skills_array(&obj);
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
        if !skills.is_empty() {
          s.skills = skills;
        }
        return true;
      }
      let session = Session {
        id: id.clone(),
        started_at: now,
        ended_at: None,
        profile,
        repo,
        branch,
        skills,
        procs: Vec::new(),
        last_seen_at: now,
        client_connected: false,
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
          orphan_containers =
            s.procs.iter().filter_map(|p| p.container_name.as_ref().map(|n| (n.clone(), String::new()))).collect();
          s.client_connected = false;
          s.last_seen_at = now;
          if s.ended_at.is_none() {
            s.ended_at = Some(now);
            for p in &mut s.procs {
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
      if let Some(p) = s.procs.iter_mut().find(|p| p.index == proc_index) {
        p.label = label;
        p.kind = kind;
        p.skill_name = skill_name;
        p.harness = harness;
        p.model = model;
      } else {
        s.procs.push(ProcRecord {
          index: proc_index,
          label,
          kind,
          status: ProcStatus::Waiting,
          skill_name,
          harness,
          model,
          started_at: None,
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          container_name: None,
          cast_path: None,
          lines: Vec::new(),
        });
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
        p.cast_path = if path.is_empty() { None } else { Some(path) };
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
      if let Some(p) = store.proc_mut(&session, proc_index) {
        if action == "start" {
          p.container_name = Some(name);
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
/// `build-images`' "(image builds)") through untouched. Clients already absolutize real paths;
/// the server-side pass is only the defensive second canonicalization for those.
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
fn images_json() -> String {
  let Some(rt) = crate::runtime::detect_runtime() else {
    return r#"{ "error": "no container runtime found (docker, podman, or Apple container)" }"#.to_string();
  };
  let rows: Vec<String> = crate::runtime::image_statuses(&rt.name)
    .iter()
    .map(|s| {
      format!(
        "{{ \"name\": {}, \"tag\": {}, \"exists\": {}, \"up_to_date\": {}, \"created\": {}, \"size\": {} }}",
        quote(&s.name),
        quote(&s.tag),
        s.exists,
        s.up_to_date,
        s.created.as_deref().map(|v| quote(v)).unwrap_or_else(|| "null".into()),
        s.size.as_deref().map(|v| quote(v)).unwrap_or_else(|| "null".into()),
      )
    })
    .collect();
  format!("{{ \"runtime\": {}, \"images\": [{}] }}", quote(&rt.name), rows.join(", "))
}

/// `POST /api/v1/images/build` — body `{"harnesses": [name…], "rebuild_base": bool, "force":
/// bool}` (all fields optional; no harnesses = all). Spawns a detached `scsh build-images
/// --session <id>` and pre-creates that session in the store, so the returned
/// `{"ok":true,"session":id}` deep-links to a live page before the child registers. One image
/// build at a time; a concurrent request gets 409.
fn images_build_response(body: &str, store: &Arc<Mutex<Store>>) -> (u16, String, bool) {
  let obj = match parse(body) {
    Ok(Value::Object(o)) => o,
    _ => Vec::new(), // an empty/absent body means "build everything with defaults"
  };
  let harnesses = parse_string_array(&obj, "harnesses");
  for h in &harnesses {
    if crate::config::Harness::parse(h).is_none() {
      let msg = format!("unknown harness '{h}' (known: {})", crate::config::Harness::known().join(", "));
      return (400, format!("{{\"ok\":false,\"error\":{}}}", quote(&msg)), false);
    }
  }
  let rebuild_base = field_bool(&obj, "rebuild_base").unwrap_or(false);
  let force = field_bool(&obj, "force").unwrap_or(false);
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
  cmd.env(super::paths::PORT_ENV, port.to_string());
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());
  match cmd.spawn() {
    Ok(mut child) => {
      // Reap the child when it exits so it never lingers as a zombie under the daemon.
      std::thread::spawn(move || {
        let _ = child.wait();
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
          repo: "(image builds)".to_string(),
          branch: String::new(),
          skills: Vec::new(),
          procs: Vec::new(),
          last_seen_at: now,
          client_connected: false,
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

fn write_response(stream: &mut TcpStream, status: u16, body: &str, content_type: &str) -> std::io::Result<()> {
  let status_text = match status {
    200 => "OK",
    400 => "Bad Request",
    404 => "Not Found",
    405 => "Method Not Allowed",
    _ => "Error",
  };
  let resp = format!(
    "HTTP/1.1 {status} {status_text}\r\n\
Content-Type: {content_type}\r\n\
Content-Length: {}\r\n\
Connection: close\r\n\r\n\
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
  stream: &mut TcpStream, status: u16, body: &str, ok_content_type: &str, disposition: Option<&str>,
) -> std::io::Result<()> {
  let status_text = if status == 200 { "OK" } else { "Not Found" };
  let content_type = if status == 200 { ok_content_type } else { "text/plain" };
  let disposition_header = match disposition {
    Some(d) => format!("Content-Disposition: {d}\r\n"),
    None => String::new(),
  };
  let resp = format!(
    "HTTP/1.1 {status} {status_text}\r\n\
Content-Type: {content_type}\r\n\
{disposition_header}Content-Length: {}\r\n\
Connection: close\r\n\r\n\
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
  fn proc_line_updates_session_last_seen_at() {
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
            started_at: Some(50),
            note: None,
            detail: None,
            fail_reason: None,
            elapsed: None,
            lines: Vec::new(),
            container_name: None,
            cast_path: None,
          }],
          last_seen_at: 50,
          client_connected: true,
        },
      );
    }
    let body = r#"{"session":"xyzabc","proc":0,"at":1.0,"line":"step"}"#;
    assert!(handle_api_post("/api/v1/proc/line", body, &store, &prune));
    let last = store.lock().unwrap().sessions.get("xyzabc").unwrap().last_seen_at;
    assert!(last > 50);
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
          last_seen_at: 1,
          client_connected: false,
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
            started_at: Some(10),
            note: None,
            detail: None,
            fail_reason: None,
            elapsed: None,
            lines: Vec::new(),
            container_name: None,
            cast_path: None,
          }],
          last_seen_at: 10,
          client_connected: true,
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
            started_at: Some(50),
            note: None,
            detail: None,
            fail_reason: None,
            elapsed: None,
            lines: Vec::new(),
            container_name: None,
            cast_path: None,
          }],
          last_seen_at: 50,
          client_connected: true,
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
    assert_eq!(session.lifecycle_status(session.ended_at.unwrap()), SessionLifecycle::Failed);
  }

  #[test]
  fn display_or_absolute_repo_keeps_labels_and_absolutizes_paths() {
    assert_eq!(display_or_absolute_repo(""), "");
    assert_eq!(display_or_absolute_repo("(image builds)"), "(image builds)");
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
    assert!(body.contains("unknown harness 'fancyharness'"), "body: {body}");
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
          repo: "(image builds)".into(),
          branch: String::new(),
          skills: Vec::new(),
          procs: Vec::new(),
          last_seen_at: now,
          client_connected: true,
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
    // SCSH_BIN points the spawn at /usr/bin/true, so the "child" exits instantly and the
    // test never builds anything. The env var is process-global; no other test sets it.
    std::env::set_var("SCSH_BIN", "/usr/bin/true");
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
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: vec![ProcRecord {
            index: 0,
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
            cast_path: None,
          }],
          last_seen_at: 50,
          client_connected: true,
        },
      );
    }
    // Before registration: 404.
    let (status, _, _) = cast_response("/cast/castab/0", &store);
    assert_eq!(status, 404);

    // Register a cast path; write a partially-flushed asciicast (last line incomplete).
    let path = std::env::temp_dir().join(format!("scsh-test-cast-{}.cast", std::process::id()));
    let header = r#"{"version": 2, "width": 200, "height": 50}"#;
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
          repo: "/r".into(),
          branch: "main".into(),
          skills: Vec::new(),
          procs: vec![ProcRecord {
            index: 0,
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
            cast_path: Some(cast_path.to_string_lossy().into_owned()),
          }],
          last_seen_at: 50,
          client_connected: false,
        },
      );
    }
    // Unknown session/proc → the existing 404 style.
    assert_eq!(export_response("/cast/nosuch/0/export.html", &store).0, 404);
    assert_eq!(export_response("/cast/expabc/9/export.html", &store).0, 404);
    // A registered cast whose file is not on disk yet → 404.
    assert_eq!(export_response("/cast/expabc/0/export.html", &store).0, 404);
    // A header with no complete frames yet → 404 with an actionable body.
    std::fs::write(&cast_path, "{\"version\":2,\"width\":80,\"height\":24}\n").unwrap();
    let (status, body, disposition) = export_response("/cast/expabc/0/export.html", &store);
    assert_eq!(status, 404);
    assert!(body.contains("no recorded frames yet"), "body: {body}");
    assert!(disposition.is_none());
    // Frames + a sidecar → the self-contained page, served as `<stem>.html`.
    std::fs::write(&cast_path, "{\"version\":2,\"width\":80,\"height\":24}\n[0.5,\"o\",\"hello\\r\\n\"]\n").unwrap();
    std::fs::write(
      dir.join("rec.chapters.json"),
      r#"{"summary":"Ran the demo.","chapters":[{"t":0,"title":"Start"}]}"#,
    )
    .unwrap();
    let (status, page, disposition) = export_response("/cast/expabc/0/export.html", &store);
    assert_eq!(status, 200);
    assert_eq!(disposition.as_deref(), Some("attachment; filename=\"rec.html\""));
    assert!(page.contains("<title>rec</title>"), "cast stem is the title");
    assert!(page.contains("@license"), "vendored player attribution survives the export");
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
      cast_path,
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
        repo: "/r".into(),
        branch: "main".into(),
        skills: Vec::new(),
        procs,
        last_seen_at: 60,
        client_connected: false,
      },
    );
    store
  }

  /// Every `srcdoc="…"` attribute value in the page. `esc` turns every embedded `"` into
  /// `&quot;`, so the first literal quote after `srcdoc="` is the attribute terminator.
  fn srcdoc_values(page: &str) -> Vec<&str> {
    page.split("srcdoc=\"").skip(1).map(|tail| tail.split('"').next().unwrap()).collect()
  }

  #[test]
  fn session_export_assembles_every_recording_into_one_page() {
    let dir = std::env::temp_dir().join(format!("scsh-session-export-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    // Proc 0: a recording WITH a chapters sidecar. Proc 1: a bare recording. Proc 2: no cast.
    let cast0 = dir.join("rec0.cast");
    std::fs::write(&cast0, "{\"version\":2,\"width\":80,\"height\":24}\n[0.5,\"o\",\"hello\\r\\n\"]\n").unwrap();
    std::fs::write(
      dir.join("rec0.chapters.json"),
      r#"{"summary":"Ran the demo.","chapters":[{"t":0,"title":"Start"}]}"#,
    )
    .unwrap();
    let cast1 = dir.join("rec1.cast");
    std::fs::write(&cast1, "{\"version\":2,\"width\":80,\"height\":24}\n[1.0,\"o\",\"done\\r\\n\"]\n").unwrap();
    let store = store_with_export_session(
      "sexabc",
      vec![
        export_test_proc(0, "claude: add", Some(cast0.to_string_lossy().into_owned())),
        export_test_proc(1, "codex: multiply", Some(cast1.to_string_lossy().into_owned())),
        export_test_proc(2, "cursor: skipped", None),
      ],
    );
    let (status, page, disposition) = session_export_response("/session/sexabc/export.html", &store);
    assert_eq!(status, 200);
    assert_eq!(disposition.as_deref(), Some("attachment; filename=\"scsh-session-sexabc.html\""));
    // The header: session id, repo, profile, and the per-proc summary table in board order.
    assert!(page.contains("scsh session <code>sexabc</code>"), "header session id");
    assert!(page.contains("<code>/r</code>"), "repo label");
    assert!(page.contains("<strong>profile</strong> default"), "profile");
    for label in ["claude: add", "codex: multiply", "cursor: skipped"] {
      assert!(page.contains(label), "summary table + section for {label}");
    }
    // Both recordings embed as iframes, each carrying its own full player copy (the
    // deliberate srcdoc-composition tradeoff), and the vendored player's license marker
    // survives the assembly at least once.
    assert_eq!(page.matches("<iframe").count(), 2, "one iframe per exportable cast");
    assert!(page.matches("loading=\"lazy\"").count() >= 2, "iframes load lazily");
    assert!(page.matches("@license").count() >= 2, "each embedded page keeps the player license");
    // Every proc section is a native <details> block — collapsible offline, no JS — open
    // by default with the informative head as its <summary>; and the page has the favicon.
    assert_eq!(page.matches("<details open class=\"proc").count(), 3, "one collapsible section per proc");
    assert_eq!(page.matches("<summary class=\"proc-head\"").count(), 3, "each section head is the summary");
    assert!(page.contains("data:image/svg+xml"), "inline favicon");
    // The annotated cast contributes its chapter title and its one-sentence summary (the
    // latter both in the section head and inside the embedded page).
    assert!(page.contains("Start"), "chapter title folded in");
    assert!(page.contains(r#"<div class="proc-summary">Ran the demo.</div>"#), "sidecar summary shown");
    // The cast-less proc degrades to a styled note row, never an error.
    assert!(page.contains(r#"<div class="proc-note">no recording — skipped/failed before output</div>"#));
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
      "{\"version\":2,\"width\":80,\"height\":24}\n[0.1,\"o\",\"x\\\" & <b> </iframe><script>alert(1)</script>\"]\n",
    )
    .unwrap();
    let store = store_with_export_session(
      "hostil",
      vec![export_test_proc(0, "claude: evil", Some(cast.to_string_lossy().into_owned()))],
    );
    let (status, page, _) = session_export_response("/session/hostil/export.html", &store);
    assert_eq!(status, 200);
    // The attribute-escaped srcdoc can neither terminate early nor open a tag: no raw `<`
    // (or `"` — by construction of the extraction) survives inside the attribute value.
    let srcdocs = srcdoc_values(&page);
    assert_eq!(srcdocs.len(), 1);
    assert!(!srcdocs[0].contains('<'), "no raw '<' inside srcdoc");
    assert!(srcdocs[0].contains("&lt;"), "embedded page markup is entity-escaped");
    // The payload never becomes live markup in the outer page.
    assert!(!page.contains("<script>alert(1)</script>"), "script payload must not go live");
    assert_eq!(page.matches("<iframe").count(), page.matches("</iframe>").count(), "iframes stay balanced");
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn session_export_without_exportable_casts_is_an_actionable_404() {
    // A frameless cast (header only) and a cast-less proc: nothing to export yet.
    let dir = std::env::temp_dir().join(format!("scsh-session-export-404-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("frameless.cast");
    std::fs::write(&cast, "{\"version\":2,\"width\":80,\"height\":24}\n").unwrap();
    let store = store_with_export_session(
      "nocast",
      vec![
        export_test_proc(0, "claude: add", Some(cast.to_string_lossy().into_owned())),
        export_test_proc(1, "codex: multiply", None),
      ],
    );
    let (status, body, disposition) = session_export_response("/session/nocast/export.html", &store);
    assert_eq!(status, 404);
    assert!(body.contains("no exportable recordings"), "body: {body}");
    assert!(disposition.is_none());
    // Unknown session: the existing 404 style.
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
            repo: "/r".into(),
            branch: "main".into(),
            skills: Vec::new(),
            procs: Vec::new(),
            last_seen_at: 1,
            client_connected: true,
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
}
