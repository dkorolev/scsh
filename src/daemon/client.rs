//! HTTP client for posting run events to the session browser daemon.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::model::{ProcKind, ProcStatus};
use super::paths::daemon_port;
use crate::json::quote;

/// Batch proc output before POSTing — ~2 flushes/sec, up to half a megabyte per payload.
const LINE_BATCH_INTERVAL: Duration = Duration::from_millis(500);
const LINE_BATCH_MAX_BYTES: usize = 512 * 1024;
/// Large parallel runs (e.g. code-review) can queue thousands of lines; wait long enough to drain.
const FLUSH_DRAIN_TIMEOUT: Duration = Duration::from_secs(120);
const POSTER_JOIN_TIMEOUT: Duration = Duration::from_secs(30);

enum PostJob {
  ProcLine { proc: usize, at: f64, line: String },
  Send { path: String, body: String },
  Flush { done: mpsc::Sender<()> },
}

struct ClientInner {
  port: u16,
  session_id: String,
  post_tx: Mutex<Option<Sender<PostJob>>>,
  poster: Mutex<Option<thread::JoinHandle<()>>>,
}

/// Posts events to the local daemon. Best-effort — failures are silent so runs never break.
pub struct Client {
  inner: Arc<ClientInner>,
}

impl Client {
  pub fn new(session_id: String) -> Client {
    let port = daemon_port();
    let (post_tx, post_rx) = mpsc::channel::<PostJob>();
    let session_for_poster = session_id.clone();
    let poster = thread::spawn(move || poster_loop(port, session_for_poster, post_rx));
    Client {
      inner: Arc::new(ClientInner {
        port,
        session_id,
        post_tx: Mutex::new(Some(post_tx)),
        poster: Mutex::new(Some(poster)),
      }),
    }
  }

  pub fn session_url(&self) -> String {
    super::paths::session_url(self.inner.port, &self.inner.session_id)
  }

  pub fn register_session(
    &self, repo: &str, branch: &str, profile: Option<&str>, kind: &str, skills: &[(&str, &str)],
  ) -> bool {
    let skill_parts: Vec<String> = skills
      .iter()
      .map(|(name, harness)| format!("{{ \"name\": {}, \"harness\": {} }}", quote(name), quote(harness)))
      .collect();
    let profile_json = match profile {
      Some(p) => quote(p),
      None => "null".to_string(),
    };
    let body = format!(
      "{{ \"session\": {}, \"repo\": {}, \"branch\": {}, \"profile\": {}, \"kind\": {}, \"skills\": [{}], \"run_pid\": {} }}",
      quote(&self.inner.session_id),
      quote(repo),
      quote(branch),
      profile_json,
      quote(kind),
      skill_parts.join(", "),
      std::process::id(),
    );
    let start_ok = self.post_sync_after_flush("/api/v1/session/start", &body);
    let register_ok =
      self.post_sync_after_flush("/api/v1/register", &format!("{{ \"session\": {} }}", quote(&self.inner.session_id)));
    start_ok && register_ok
  }

  pub fn deregister(&self) {
    let body = format!("{{ \"session\": {} }}", quote(&self.inner.session_id));
    if !self.post_sync("/api/v1/deregister", &body) {
      log_post_failure("/api/v1/deregister", None);
    }
  }

  /// Drain the poster, deregister synchronously, then stop the poster thread.
  pub fn finish_session(&self) {
    let drained = self.flush_poster();
    self.deregister();
    if !drained {
      crate::failure::log_session_proc(
        &self.inner.session_id,
        crate::failure::reason::DAEMON_DRAIN_TIMEOUT,
        "(session)",
        "event poster did not drain before session end — some proc output or finish events may be missing from the browser UI",
      );
    }
    self.close_poster();
  }

  /// Close the async poster after queued work is drained (see `finish_session`).
  pub fn close_poster(&self) {
    let mut post_tx = self.inner.post_tx.lock().unwrap_or_else(|e| e.into_inner());
    *post_tx = None;
    let mut poster = self.inner.poster.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = poster.take() {
      let (done_tx, done_rx) = mpsc::channel();
      thread::spawn(move || {
        let _ = handle.join();
        let _ = done_tx.send(());
      });
      let _ = done_rx.recv_timeout(POSTER_JOIN_TIMEOUT);
    }
  }

  /// Drain buffered proc lines and close the async poster.
  pub fn flush(&self) {
    self.flush_poster();
    self.close_poster();
  }

  pub fn proc_add(
    &self, proc_index: usize, label: &str, kind: ProcKind, skill_name: Option<&str>, harness: Option<&str>,
    model: Option<&str>,
  ) {
    let mut extras = Vec::new();
    if let Some(s) = skill_name {
      extras.push(format!("\"skill_name\": {}", quote(s)));
    }
    if let Some(h) = harness {
      extras.push(format!("\"harness\": {}", quote(h)));
    }
    if let Some(m) = model {
      extras.push(format!("\"model\": {}", quote(m)));
    }
    let tail = if extras.is_empty() { String::new() } else { format!(", {}", extras.join(", ")) };
    let body = format!(
      "{{ \"session\": {}, \"proc\": {}, \"label\": {}, \"kind\": {}{tail} }}",
      quote(&self.inner.session_id),
      proc_index,
      quote(label),
      quote(kind.as_str()),
    );
    let _ = self.post("/api/v1/proc/add", &body);
  }

  pub fn proc_start(&self, proc_index: usize) {
    let body = format!("{{ \"session\": {}, \"proc\": {} }}", quote(&self.inner.session_id), proc_index);
    let _ = self.post("/api/v1/proc/start", &body);
  }

  pub fn proc_note(&self, proc_index: usize, note: &str) {
    let body = format!(
      "{{ \"session\": {}, \"proc\": {}, \"note\": {} }}",
      quote(&self.inner.session_id),
      proc_index,
      quote(note)
    );
    let _ = self.post("/api/v1/proc/note", &body);
  }

  /// Tell the daemon where this proc's asciinema `.cast` lives on the host: the live
  /// run-dir file while the container runs, then the durable copy after the skill ends.
  pub fn proc_cast(&self, proc_index: usize, path: &str) {
    let body = format!(
      "{{ \"session\": {}, \"proc\": {}, \"path\": {} }}",
      quote(&self.inner.session_id),
      proc_index,
      quote(path)
    );
    let _ = self.post("/api/v1/proc/cast", &body);
  }

  pub fn proc_line(&self, proc_index: usize, at: f64, line: &str) {
    if let Some(tx) = lock_post_tx(&self.inner) {
      let _ = tx.send(PostJob::ProcLine { proc: proc_index, at, line: line.to_string() });
    }
  }

  pub fn proc_finish(
    &self, proc_index: usize, status: ProcStatus, fail_reason: Option<&str>, detail: Option<&str>, elapsed: f64,
  ) {
    let detail_json = match detail {
      Some(d) => quote(d),
      None => "null".to_string(),
    };
    let fail_reason_json = match fail_reason {
      Some(r) => quote(r),
      None => "null".to_string(),
    };
    let body = format!(
      "{{ \"session\": {}, \"proc\": {}, \"status\": {}, \"fail_reason\": {}, \"detail\": {}, \"elapsed\": {} }}",
      quote(&self.inner.session_id),
      proc_index,
      quote(status.as_str()),
      fail_reason_json,
      detail_json,
      elapsed
    );
    if !self.post_sync_after_flush("/api/v1/proc/finish", &body) {
      log_post_failure("/api/v1/proc/finish", Some(proc_index));
      if status == ProcStatus::Fail {
        crate::failure::log_session_proc(
          &self.inner.session_id,
          crate::failure::reason::DAEMON_POST_FAILED,
          &format!("proc {proc_index}"),
          detail.unwrap_or("(proc/finish not accepted by daemon)"),
        );
      }
    }
  }

  pub fn container_event(&self, proc_index: usize, action: &str, name: &str) {
    let body = format!(
      "{{ \"session\": {}, \"proc\": {}, \"action\": {}, \"name\": {} }}",
      quote(&self.inner.session_id),
      proc_index,
      quote(action),
      quote(name)
    );
    if action == "stop" {
      if !self.post_sync_after_flush("/api/v1/container", &body) {
        log_post_failure("/api/v1/container", Some(proc_index));
      }
    } else {
      let _ = self.post("/api/v1/container", &body);
    }
  }

  /// Backup schedule for a run dir — the client deletes first; the daemon retries if it still exists.
  pub fn schedule_run_dir_prune(&self, run_dir: &str, container_name: &str, runtime: &str, outcome_ok: bool) {
    let outcome = if outcome_ok { "ok" } else { "fail" };
    let body = format!(
      "{{ \"run_dir\": {}, \"container_name\": {}, \"runtime\": {}, \"outcome\": {} }}",
      quote(run_dir),
      quote(container_name),
      quote(runtime),
      quote(outcome),
    );
    let _ = self.post("/api/v1/prune/schedule", &body);
  }

  pub fn ping(&self) {
    let _ = self.post("/api/v1/ping", &format!("{{ \"session\": {} }}", quote(&self.inner.session_id)));
  }

  /// True when the session browser daemon serves its HTTP API on localhost.
  pub fn daemon_alive() -> bool {
    super::paths::daemon_api_responds(daemon_port())
  }

  fn post(&self, path: &str, body: &str) {
    if let Some(tx) = lock_post_tx(&self.inner) {
      let _ = tx.send(PostJob::Send { path: path.to_string(), body: body.to_string() });
    }
  }

  fn post_sync(&self, path: &str, body: &str) -> bool {
    send_post(self.inner.port, path, body)
  }

  /// Drain buffered proc lines in the poster before a synchronous lifecycle POST.
  fn post_sync_after_flush(&self, path: &str, body: &str) -> bool {
    self.flush_poster();
    self.post_sync(path, body)
  }

  fn flush_poster(&self) -> bool {
    if let Some(tx) = lock_post_tx(&self.inner) {
      let (done_tx, done_rx) = mpsc::channel();
      if tx.send(PostJob::Flush { done: done_tx }).is_ok() {
        return done_rx.recv_timeout(FLUSH_DRAIN_TIMEOUT).is_ok();
      }
    }
    true
  }
}

/// One synchronous POST to a daemon on `port`, outside any session (e.g. `scsh prune --now`).
pub fn post_once(port: u16, path: &str, body: &str) -> bool {
  send_post(port, path, body)
}

fn log_daemon_warn(msg: &str) {
  eprintln!("session browser: {msg}");
}

fn log_post_failure(path: &str, proc: Option<usize>) {
  match proc {
    Some(i) => log_daemon_warn(&format!("POST {path} for proc {i} failed (daemon unreachable or rejected request)")),
    None => log_daemon_warn(&format!("POST {path} failed (daemon unreachable or rejected request)")),
  }
}

fn lock_post_tx(inner: &ClientInner) -> Option<Sender<PostJob>> {
  inner.post_tx.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn poster_loop(port: u16, session_id: String, rx: mpsc::Receiver<PostJob>) {
  let mut line_buf: Vec<(usize, f64, String)> = Vec::new();
  let mut line_bytes = 0usize;
  loop {
    match rx.recv_timeout(LINE_BATCH_INTERVAL) {
      Ok(PostJob::ProcLine { proc, at, line }) => {
        line_bytes += line.len();
        line_buf.push((proc, at, line));
        if line_bytes >= LINE_BATCH_MAX_BYTES {
          flush_lines(port, &session_id, &mut line_buf, &mut line_bytes);
        }
      }
      Ok(PostJob::Send { path, body }) => {
        flush_lines(port, &session_id, &mut line_buf, &mut line_bytes);
        if !send_post(port, &path, &body) {
          log_post_failure(&path, None);
        }
      }
      Ok(PostJob::Flush { done }) => {
        drain_poster_burst(&rx, port, &session_id, &mut line_buf, &mut line_bytes);
        let _ = done.send(());
      }
      Err(RecvTimeoutError::Timeout) => {
        flush_lines(port, &session_id, &mut line_buf, &mut line_bytes);
      }
      Err(RecvTimeoutError::Disconnected) => {
        flush_lines(port, &session_id, &mut line_buf, &mut line_bytes);
        break;
      }
    }
  }
}

/// After the explicit flush marker, drain any jobs already queued behind it before acking.
fn drain_poster_burst(
  rx: &mpsc::Receiver<PostJob>, port: u16, session_id: &str, line_buf: &mut Vec<(usize, f64, String)>,
  line_bytes: &mut usize,
) {
  flush_lines(port, session_id, line_buf, line_bytes);
  loop {
    match rx.try_recv() {
      Ok(PostJob::ProcLine { proc, at, line }) => {
        *line_bytes += line.len();
        line_buf.push((proc, at, line));
        if *line_bytes >= LINE_BATCH_MAX_BYTES {
          flush_lines(port, session_id, line_buf, line_bytes);
        }
      }
      Ok(PostJob::Send { path, body }) => {
        flush_lines(port, session_id, line_buf, line_bytes);
        if !send_post(port, &path, &body) {
          log_post_failure(&path, None);
        }
      }
      Ok(PostJob::Flush { done }) => {
        flush_lines(port, session_id, line_buf, line_bytes);
        let _ = done.send(());
      }
      Err(TryRecvError::Empty) => break,
      Err(TryRecvError::Disconnected) => break,
    }
  }
  flush_lines(port, session_id, line_buf, line_bytes);
}

fn group_line_buf(buf: &[(usize, f64, String)]) -> Vec<(usize, Vec<(f64, String)>)> {
  let mut groups = Vec::new();
  let mut i = 0;
  while i < buf.len() {
    let proc = buf[i].0;
    let mut j = i + 1;
    while j < buf.len() && buf[j].0 == proc {
      j += 1;
    }
    let chunk: Vec<(f64, String)> = buf[i..j].iter().map(|(_, at, line)| (*at, line.clone())).collect();
    groups.push((proc, chunk));
    i = j;
  }
  groups
}

fn flush_lines(port: u16, session_id: &str, buf: &mut Vec<(usize, f64, String)>, bytes: &mut usize) {
  if buf.is_empty() {
    return;
  }
  for (proc, chunk) in group_line_buf(buf) {
    if !send_lines_bulk(port, session_id, proc, &chunk) {
      log_post_failure("/api/v1/proc/lines", Some(proc));
    }
  }
  buf.clear();
  *bytes = 0;
}

fn send_lines_bulk(port: u16, session_id: &str, proc_index: usize, lines: &[(f64, String)]) -> bool {
  if lines.is_empty() {
    return true;
  }
  let entries: Vec<String> =
    lines.iter().map(|(at, text)| format!("{{ \"at\": {at}, \"line\": {} }}", quote(text))).collect();
  let body =
    format!("{{ \"session\": {}, \"proc\": {}, \"lines\": [{}] }}", quote(session_id), proc_index, entries.join(", "));
  send_post(port, "/api/v1/proc/lines", &body)
}

fn send_post(port: u16, path: &str, body: &str) -> bool {
  let Ok(mut stream) =
    TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_millis(500))
  else {
    return false;
  };
  stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
  stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
  let req = format!(
    "POST {path} HTTP/1.1\r\n\
Host: 127.0.0.1\r\n\
Content-Type: application/json\r\n\
Content-Length: {len}\r\n\
Connection: close\r\n\r\n\
{body}",
    len = body.len()
  );
  if stream.write_all(req.as_bytes()).is_err() {
    return false;
  }
  let mut resp = String::new();
  if stream.read_to_string(&mut resp).is_err() {
    return false;
  }
  resp.starts_with("HTTP/1.1 200") || resp.starts_with("HTTP/1.0 200")
}

/// The real scsh binary to re-exec (`__daemon-serve`, and the daemon's `build-images` spawn).
pub(crate) fn scsh_executable() -> std::io::Result<std::path::PathBuf> {
  if let Ok(path) = std::env::var("SCSH_BIN") {
    return Ok(std::path::PathBuf::from(path));
  }
  let exe = std::env::current_exe()?;
  let lossy = exe.to_string_lossy();
  // `cargo test` runs a harness at `target/{debug,release}/deps/scsh-<hash>`; spawn the real binary.
  if lossy.contains("/deps/scsh-") {
    let mut candidate = exe.clone();
    if candidate.pop() && candidate.pop() {
      candidate.push("scsh");
      if candidate.is_file() {
        return Ok(candidate);
      }
    }
  }
  Ok(exe)
}

/// Spawn the daemon child process (re-exec `scsh __daemon-serve`).
#[cfg(not(unix))]
pub fn spawn_daemon(mode: super::model::DaemonMode) -> std::io::Result<()> {
  let exe = scsh_executable()?;
  let port = daemon_port();
  let mut cmd = std::process::Command::new(exe);
  cmd.args(["__daemon-serve", "--mode", mode.as_str(), "--port", &port.to_string()]);
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());
  cmd.spawn()?;
  wait_for_daemon(Duration::from_secs(5))
}

#[cfg(unix)]
pub fn spawn_daemon(mode: super::model::DaemonMode) -> std::io::Result<()> {
  let exe = scsh_executable()?;
  let port = daemon_port();
  let mut cmd = std::process::Command::new(exe);
  cmd.args(["__daemon-serve", "--mode", mode.as_str(), "--port", &port.to_string()]);
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());
  {
    use std::os::unix::process::CommandExt;
    // Double-fork so the daemon survives the parent `scsh` process exiting; `setsid` detaches
    // from the terminal (async-signal-safe syscalls only in pre_exec).
    unsafe {
      cmd.pre_exec(|| super::paths::daemon_detach_child());
    }
  }
  cmd.spawn()?;
  std::thread::sleep(Duration::from_millis(50));
  wait_for_daemon(Duration::from_secs(5))
}

fn wait_for_daemon(timeout: Duration) -> std::io::Result<()> {
  let deadline = std::time::Instant::now() + timeout;
  while std::time::Instant::now() < deadline {
    if Client::daemon_alive() {
      return Ok(());
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "daemon did not start"))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::mpsc;

  #[test]
  fn client_session_url_uses_port() {
    struct PinDaemonPort {
      previous: Option<String>,
    }
    impl PinDaemonPort {
      fn set() -> (u16, Self) {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let previous = std::env::var("SCSH_DAEMON_PORT").ok();
        std::env::set_var("SCSH_DAEMON_PORT", port.to_string());
        (port, Self { previous })
      }
    }
    impl Drop for PinDaemonPort {
      fn drop(&mut self) {
        match &self.previous {
          Some(v) => std::env::set_var("SCSH_DAEMON_PORT", v),
          None => std::env::remove_var("SCSH_DAEMON_PORT"),
        }
      }
    }
    let (port, _pin) = PinDaemonPort::set();
    let c = Client::new("abcdef".into());
    assert!(c.session_url().contains("abcdef"));
    assert!(c.session_url().contains(&port.to_string()));
  }

  #[test]
  fn poster_batches_lines_by_proc() {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || poster_loop(1, "sess01".into(), rx));
    tx.send(PostJob::ProcLine { proc: 1, at: 1.0, line: "b".into() }).unwrap();
    tx.send(PostJob::ProcLine { proc: 0, at: 0.5, line: "a".into() }).unwrap();
    drop(tx);
    let _ = handle.join();
    // No daemon on port 1 — exercise is flush/group without panic; send_post fails silently.
  }

  #[test]
  fn group_line_buf_preserves_insertion_order() {
    let buf =
      vec![(1, 1.0, "first".into()), (2, 2.0, "other".into()), (1, 3.0, "second".into()), (1, 4.0, "third".into())];
    let groups = group_line_buf(&buf);
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].0, 1);
    assert_eq!(groups[0].1.len(), 1);
    assert_eq!(groups[0].1[0].1, "first");
    assert_eq!(groups[1].0, 2);
    assert_eq!(groups[2].0, 1);
    assert_eq!(groups[2].1.len(), 2);
    assert_eq!(groups[2].1[0].1, "second");
    assert_eq!(groups[2].1[1].1, "third");
  }
}
