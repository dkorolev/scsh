//! HTTP server for the session browser daemon.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::html;
use super::jsonio::{field_num, field_str, load_store, save_store, tick_json, tick_json_light};
use super::model::{DaemonMode, OutputLine, ProcKind, ProcRecord, ProcStatus, Session, SkillMeta, Store};
use super::paths::{now_unix_secs, pid_file, state_file};
use super::websocket::{self, Hub};
use crate::json::{parse, quote, Value};

const PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);
const WS_TICK: Duration = Duration::from_millis(500);
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
  port: u16,
  dirty: Arc<AtomicBool>,
  ws_dirty: Arc<AtomicBool>,
  last_persist: Mutex<Option<Instant>>,
  ws_hub: Arc<Hub>,
}

impl Server {
  pub fn new(mode: DaemonMode, port: u16) -> Server {
    let now = now_unix_secs();
    let mut store = if let Ok(text) = std::fs::read_to_string(state_file(port)) {
      load_store(&text).unwrap_or_else(|_| Store::new(mode, port, now))
    } else {
      Store::new(mode, port, now)
    };
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
      port,
      dirty: Arc::new(AtomicBool::new(false)),
      ws_dirty: Arc::new(AtomicBool::new(false)),
      last_persist: Mutex::new(None),
      ws_hub: Hub::new(),
    }
  }

  pub fn run(&self) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::daemon::paths::daemon_dir())?;
    self.persist_now();

    let addr = format!("127.0.0.1:{}", self.port);
    let listener = TcpListener::bind(&addr)?;
    listener.set_nonblocking(true)?;
    let pid_path = pid_file(self.port);
    std::fs::write(&pid_path, std::process::id().to_string())?;

    let mut last_ws_tick = Instant::now();

    loop {
      match listener.accept() {
        Ok((stream, _)) => {
          let store = Arc::clone(&self.store);
          let dirty = Arc::clone(&self.dirty);
          let ws_dirty = Arc::clone(&self.ws_dirty);
          let ws_hub = Arc::clone(&self.ws_hub);
          std::thread::spawn(move || {
            let mutated =
              catch_unwind(AssertUnwindSafe(|| handle_connection(stream, &store, &ws_hub).unwrap_or(false)))
                .unwrap_or(false);
            if mutated {
              dirty.store(true, Ordering::Relaxed);
              ws_dirty.store(true, Ordering::Relaxed);
            }
          });
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(e),
      }

      self.persist_if_due();

      if last_ws_tick.elapsed() >= WS_TICK {
        let now = now_unix_secs();
        let include_sessions = self.ws_dirty.load(Ordering::Relaxed);
        let json = {
          let mut store = lock_store(&self.store);
          store.reconcile(now);
          if include_sessions {
            tick_json(&*store, now)
          } else {
            tick_json_light(&*store, now)
          }
        };
        self.ws_hub.broadcast(json);
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

  fn persist_now(&self) {
    let store = lock_store(&self.store);
    let text = save_store(&store);
    let _ = std::fs::write(state_file(self.port), text);
    self.dirty.store(false, Ordering::Relaxed);
    *lock_last_persist(&self.last_persist) = Some(Instant::now());
  }
}

fn handle_connection(mut stream: TcpStream, store: &Arc<Mutex<Store>>, ws_hub: &Arc<Hub>) -> std::io::Result<bool> {
  // Accepted sockets inherit the listener's non-blocking mode on macOS; block for reads.
  stream.set_nonblocking(false)?;
  stream.set_read_timeout(Some(Duration::from_secs(5)))?;
  let req = read_request(&mut stream)?;
  if websocket::wants_upgrade(&req.method, &req.path, &req.headers) {
    websocket::accept_handshake(&mut stream, &req.headers)?;
    let rx = ws_hub.subscribe();
    websocket::serve(stream, rx);
    return Ok(false);
  }
  let (status, body, content_type, mutated) = route(&req, store);
  write_response(&mut stream, status, &body, content_type)?;
  Ok(mutated)
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

fn route(req: &HttpRequest, store: &Arc<Mutex<Store>>) -> (u16, String, &'static str, bool) {
  if req.method == "POST" && req.path.starts_with("/api/v1/") {
    let ok = handle_api_post(&req.path, &req.body, store);
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
    "/api/v1/sessions" => {
      let store = lock_store(store);
      let ids: Vec<String> = store.sessions.keys().cloned().collect();
      let parts: Vec<String> = ids.iter().map(|id| quote(id)).collect();
      (200, format!("{{ \"sessions\": [{}] }}", parts.join(", ")), "application/json", false)
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

fn handle_api_post(path: &str, body: &str, store: &Arc<Mutex<Store>>) -> bool {
  let obj = match parse(body).ok() {
    Some(Value::Object(o)) => o,
    _ => return false,
  };
  let now = now_unix_secs();
  let mut store = lock_store(store);
  store.touch(now);

  match path {
    "/api/v1/session/start" => {
      let id = field_str(&obj, "session").unwrap_or_default();
      let repo = super::paths::absolutize_repo_path(std::path::Path::new(&field_str(&obj, "repo").unwrap_or_default()));
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
  }
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
          }],
          last_seen_at: 50,
          client_connected: true,
        },
      );
    }
    let body = r#"{"session":"xyzabc","proc":0,"at":1.0,"line":"step"}"#;
    assert!(handle_api_post("/api/v1/proc/line", body, &store));
    let last = store.lock().unwrap().sessions.get("xyzabc").unwrap().last_seen_at;
    assert!(last > 50);
  }

  #[test]
  fn proc_line_caps_at_max_proc_lines() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 1)));
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
          }],
          last_seen_at: 1,
          client_connected: false,
        },
      );
    }
    for i in 0..=MAX_PROC_LINES {
      let body = format!(r#"{{"session":"captest","proc":0,"at":{i}.0,"line":"L{i}"}}"#);
      assert!(handle_api_post("/api/v1/proc/line", &body, &store));
    }
    let len = store.lock().unwrap().sessions.get("captest").unwrap().procs[0].lines.len();
    assert_eq!(len, MAX_PROC_LINES);
  }

  #[test]
  fn proc_lines_bulk_appends_all() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 10)));
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
          }],
          last_seen_at: 10,
          client_connected: true,
        },
      );
    }
    let body = r#"{"session":"bulk","proc":0,"lines":[{"at":1.0,"line":"a"},{"at":2.0,"line":"b"}]}"#;
    assert!(handle_api_post("/api/v1/proc/lines", body, &store));
    let guard = store.lock().unwrap();
    let lines = &guard.sessions.get("bulk").unwrap().procs[0].lines;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "a");
    assert_eq!(lines[1].text, "b");
  }

  #[test]
  fn deregister_marks_ended_and_fails_incomplete_procs() {
    let store = Arc::new(Mutex::new(Store::new(DaemonMode::Persistent, 7274, 50)));
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
          }],
          last_seen_at: 50,
          client_connected: true,
        },
      );
    }
    let body = r#"{"session":"dereg01"}"#;
    assert!(handle_api_post("/api/v1/deregister", body, &store));
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
  fn server_new_resets_started_at_on_reload() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    std::fs::create_dir_all(crate::daemon::paths::daemon_dir()).unwrap();
    let path = state_file(port);
    let _guard = StateFileGuard(path.clone());
    let stale = Store::new(DaemonMode::Persistent, port, 1);
    std::fs::write(&path, save_store(&stale)).unwrap();
    let before = now_unix_secs();
    let server = Server::new(DaemonMode::Persistent, port);
    server.persist_now();
    let loaded = load_store(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(loaded.started_at >= before, "reload should refresh started_at");
    assert_ne!(loaded.started_at, 1);
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

  struct StateFileGuard(std::path::PathBuf);

  impl Drop for StateFileGuard {
    fn drop(&mut self) {
      let _ = std::fs::remove_file(&self.0);
    }
  }
}
