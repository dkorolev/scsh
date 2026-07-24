//! Local HTTP daemon for browsing scsh run sessions in a browser.
//!
//! Serves a small web UI on `127.0.0.1:7274` (default) that collects events from
//! `scsh run` — image builds, container starts, and harness output — grouped by session id.

mod castprobe;
mod client;
mod db;
mod html;
mod jsonio;
mod model;
mod paths;
pub mod prune;
pub mod reap;
mod server;
mod setup;
mod supervisor;
mod websocket;
mod workflow;

pub use client::{post_once, spawn_daemon, Client};
/// Generate a session id: six lowercase letters (delegates to runtime nonce helper).
pub fn new_session_id() -> String {
  crate::runtime::random_nonce_6()
}

pub use model::{DaemonMode, ProcKind, ProcRecord, ProcStatus};
#[cfg(unix)]
pub use paths::daemon_detach_child;
pub use paths::{
  absolutize_repo_path, base_url, clear_session_cancel, consume_proc_restart, daemon_dir, daemon_port,
  daemon_port_reachable, daemon_reported_started_at, daemon_reported_version, now_unix_secs, read_live_pid,
  request_proc_restart, request_session_cancel, session_cancelled,
};
pub use server::{chapters_sidecar_path, Server};
pub(crate) use server::{write_start_recipe, INTERNAL_REPO};
/// Loop-iteration run ids are the job-wide naming scheme for cycles, so readers outside the
/// daemon (fleet aggregation) parse them through the same function the graph uses.
pub(crate) use workflow::parse_loop_iteration_id;
pub use workflow::workflow_meta_from_def;

const ENSURE_ATTEMPTS: usize = 3;

/// Ensure a daemon is running for a `scsh run`. Reuses any live daemon on the configured port
/// (persistent or ephemeral from another run). Otherwise spawns an ephemeral daemon that exits
/// after five minutes with no connected `scsh run` clients.
pub fn ensure_for_run() -> std::io::Result<()> {
  ensure_daemon(DaemonMode::Ephemeral)
}

/// Start a persistent daemon (`scsh daemon start`).
pub fn start_persistent() -> std::io::Result<()> {
  ensure_daemon(DaemonMode::Persistent)
}

fn ensure_daemon(mode: DaemonMode) -> std::io::Result<()> {
  for attempt in 0..ENSURE_ATTEMPTS {
    if Client::daemon_alive() {
      if mode == DaemonMode::Persistent && paths::read_persisted_mode(daemon_port()) == Some(DaemonMode::Ephemeral) {
        let _ = stop();
        clear_stale_daemon_state();
      } else {
        return Ok(());
      }
    } else {
      clear_stale_daemon_state();
    }
    match spawn_daemon(mode) {
      Ok(()) if Client::daemon_alive() => return Ok(()),
      Ok(()) => {
        clear_stale_daemon_state();
      }
      Err(e) if attempt + 1 == ENSURE_ATTEMPTS => return Err(e),
      Err(_) => clear_stale_daemon_state(),
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
  }
  Err(std::io::Error::other("session browser daemon did not become reachable on localhost"))
}

/// Drop a wedged PID file or stop a process that still holds the pid file but is not serving HTTP.
fn clear_stale_daemon_state() {
  let port = daemon_port();
  if read_live_pid(port).is_some() {
    let _ = stop();
  } else {
    let _ = std::fs::remove_file(paths::pid_file(port));
  }
}

/// Stop a running daemon (`scsh daemon stop`).
pub fn stop() -> std::io::Result<bool> {
  let port = daemon_port();
  let pid_path = paths::pid_file(port);
  let pid = match read_live_pid(port) {
    Some(p) => p,
    None => {
      let _ = std::fs::remove_file(&pid_path);
      return Ok(false);
    }
  };
  #[cfg(unix)]
  {
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    paths::signal_process(pid, SIGTERM);
    for _ in 0..20 {
      if !paths::pid_alive(pid) {
        break;
      }
      std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if paths::pid_alive(pid) {
      paths::signal_process(pid, SIGKILL);
    }
  }
  #[cfg(not(unix))]
  {
    // Cannot signal by PID on this platform; stop only clears a stale pid file.
    let _ = std::fs::remove_file(&pid_path);
    return Ok(false);
  }
  let _ = std::fs::remove_file(&pid_path);
  Ok(true)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::TcpListener;
  use std::sync::Mutex;

  static DAEMON_SPAWN_LOCK: Mutex<()> = Mutex::new(());

  fn unused_local_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
  }

  /// Point the daemon port AND scsh home (redb store dir) at test-owned values while a test
  /// runs, restoring both on drop — so a spawned daemon never touches the real `~/.scsh`.
  /// Only mutated under `DAEMON_SPAWN_LOCK`, so the process-global env is not raced.
  struct RestoreDaemonPortEnv {
    previous_port: Option<String>,
    previous_home: Option<String>,
    home_dir: std::path::PathBuf,
    /// Held for the guard's whole lifetime: SCSH_HOME is process-global, and every other
    /// test that reads or writes it serializes on the same lock (see `runtime::test_env_lock`).
    _env: std::sync::MutexGuard<'static, ()>,
  }

  impl RestoreDaemonPortEnv {
    fn set(port: u16) -> Self {
      let env = crate::runtime::test_env_lock();
      let previous_port = std::env::var(paths::PORT_ENV).ok();
      let previous_home = std::env::var(paths::HOME_ENV).ok();
      let home_dir = std::env::temp_dir().join(format!("scsh-home-{}", crate::runtime::random_nonce_6()));
      std::env::set_var(paths::PORT_ENV, port.to_string());
      std::env::set_var(paths::HOME_ENV, &home_dir);
      Self { previous_port, previous_home, home_dir, _env: env }
    }
  }

  impl Drop for RestoreDaemonPortEnv {
    fn drop(&mut self) {
      match &self.previous_port {
        Some(v) => std::env::set_var(paths::PORT_ENV, v),
        None => std::env::remove_var(paths::PORT_ENV),
      }
      match &self.previous_home {
        Some(v) => std::env::set_var(paths::HOME_ENV, v),
        None => std::env::remove_var(paths::HOME_ENV),
      }
      let _ = std::fs::remove_dir_all(&self.home_dir);
    }
  }

  struct EphemeralDaemonGuard {
    port: u16,
  }

  impl Drop for EphemeralDaemonGuard {
    fn drop(&mut self) {
      let _ = stop();
      let _ = std::fs::remove_file(paths::store_db_file(self.port));
      let _ = std::fs::remove_file(paths::mode_file(self.port));
    }
  }

  /// Wait until a daemon answers on the port AND has persisted the EXPECTED mode. TCP
  /// aliveness alone races the persistent-replaces-ephemeral handover: during the swap
  /// the OUTGOING daemon still answers while the mode file lags behind the new one.
  fn wait_daemon_mode(port: u16, mode: DaemonMode) {
    for _ in 0..100 {
      if paths::read_persisted_mode(port) == Some(mode) && Client::daemon_alive() {
        return;
      }
      std::thread::sleep(std::time::Duration::from_millis(50));
    }
  }

  #[test]
  fn ensure_for_run_spawns_ephemeral_daemon() {
    let _spawn = DAEMON_SPAWN_LOCK.lock().unwrap();
    let port = unused_local_port();
    let _env = RestoreDaemonPortEnv::set(port);
    let _guard = EphemeralDaemonGuard { port };
    let _ = stop();
    ensure_for_run().expect("ensure_for_run");
    wait_daemon_mode(port, DaemonMode::Ephemeral);
    assert!(Client::daemon_alive(), "daemon should accept TCP on {}", port);
    assert_eq!(paths::read_persisted_mode(port), Some(DaemonMode::Ephemeral));
  }

  #[test]
  fn daemon_serves_a_connection_burst_within_one_tick() {
    let _spawn = DAEMON_SPAWN_LOCK.lock().unwrap();
    let port = unused_local_port();
    let _env = RestoreDaemonPortEnv::set(port);
    let _guard = EphemeralDaemonGuard { port };
    let _ = stop();
    ensure_for_run().expect("ensure_for_run");
    wait_daemon_mode(port, DaemonMode::Ephemeral);
    // Twenty concurrent one-shot requests (each asks Connection: close, so each is its
    // own connection — the worst-case fetch burst). The accept loop must drain the
    // whole backlog per tick: one-accept-per-100ms-tick would need two full seconds.
    let start = std::time::Instant::now();
    let clients: Vec<_> = (0..20)
      .map(|_| {
        std::thread::spawn(move || {
          use std::io::{Read, Write};
          let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
          s.write_all(b"GET /api/v1/version HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
          let mut buf = String::new();
          s.read_to_string(&mut buf).unwrap();
          assert!(buf.starts_with("HTTP/1.1 200"), "burst request served: {buf}");
        })
      })
      .collect();
    for c in clients {
      c.join().unwrap();
    }
    let elapsed = start.elapsed();
    assert!(elapsed < std::time::Duration::from_millis(1500), "20-connection burst took {elapsed:?}");
  }

  /// One HTTP response read off a keep-alive test connection: status line + headers, then
  /// exactly the Content-Length body — reading to EOF would block on a live connection.
  fn read_framed_response(s: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
      if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        break end;
      }
      let n = s.read(&mut chunk).expect("read response");
      assert!(n > 0, "connection closed before a full response");
      buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = head
      .lines()
      .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
      .unwrap_or(0);
    while buf.len() - (header_end + 4) < content_length {
      let n = s.read(&mut chunk).expect("read body");
      assert!(n > 0, "connection closed mid-body");
      buf.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8_lossy(&buf).into_owned()
  }

  #[test]
  fn daemon_keeps_a_connection_alive_across_requests_and_honors_close() {
    let _spawn = DAEMON_SPAWN_LOCK.lock().unwrap();
    let port = unused_local_port();
    let _env = RestoreDaemonPortEnv::set(port);
    let _guard = EphemeralDaemonGuard { port };
    let _ = stop();
    ensure_for_run().expect("ensure_for_run");
    wait_daemon_mode(port, DaemonMode::Ephemeral);
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    // Two requests ride the same connection; each response is framed and keeps it open.
    for _ in 0..2 {
      s.write_all(b"GET /api/v1/version HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
      let resp = read_framed_response(&mut s);
      assert!(resp.starts_with("HTTP/1.1 200"), "keep-alive request served: {resp}");
      assert!(resp.to_ascii_lowercase().contains("connection: keep-alive"), "connection stays open: {resp}");
    }
    // An explicit Connection: close is honored: response, then EOF.
    s.write_all(b"GET /api/v1/version HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut rest = String::new();
    s.read_to_string(&mut rest).expect("read to close");
    assert!(rest.starts_with("HTTP/1.1 200"), "final request served: {rest}");
    assert!(rest.to_ascii_lowercase().contains("connection: close"), "close is labeled: {rest}");
    // An HTTP/1.0 request closes by default — such clients may frame the response by EOF.
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("reconnect");
    s.write_all(b"GET /api/v1/version HTTP/1.0\r\nHost: x\r\n\r\n").unwrap();
    let mut old = String::new();
    s.read_to_string(&mut old).expect("read to close");
    assert!(old.starts_with("HTTP/1.1 200"), "1.0 request served: {old}");
    assert!(old.to_ascii_lowercase().contains("connection: close"), "1.0 closes by default: {old}");
  }

  #[test]
  fn start_persistent_replaces_ephemeral_daemon() {
    let _spawn = DAEMON_SPAWN_LOCK.lock().unwrap();
    let port = unused_local_port();
    let _env = RestoreDaemonPortEnv::set(port);
    let _guard = EphemeralDaemonGuard { port };
    let _ = stop();
    ensure_for_run().expect("ensure_for_run");
    wait_daemon_mode(port, DaemonMode::Ephemeral);
    assert_eq!(paths::read_persisted_mode(port), Some(DaemonMode::Ephemeral));
    start_persistent().expect("start_persistent");
    wait_daemon_mode(port, DaemonMode::Persistent);
    assert!(Client::daemon_alive(), "persistent daemon should accept TCP on {}", port);
    assert_eq!(paths::read_persisted_mode(port), Some(DaemonMode::Persistent));
  }
}
