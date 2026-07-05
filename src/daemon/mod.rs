//! Local HTTP daemon for browsing scsh run sessions in a browser.
//!
//! Serves a small web UI on `127.0.0.1:7274` (default) that collects events from
//! `scsh run` — image builds, container starts, and harness output — grouped by session id.

mod client;
mod html;
mod jsonio;
mod model;
mod paths;
mod server;
mod websocket;

pub use client::{spawn_daemon, Client};
/// Generate a session id: six lowercase letters (delegates to runtime nonce helper).
pub fn new_session_id() -> String {
  crate::runtime::random_nonce_6()
}

pub use model::{DaemonMode, ProcKind, ProcStatus};
pub use paths::{absolutize_repo_path, base_url, daemon_dir, daemon_port, now_unix_secs, read_live_pid};
pub use server::Server;

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
  Err(std::io::Error::new(std::io::ErrorKind::Other, "session browser daemon did not become reachable on localhost"))
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

  struct RestoreDaemonPortEnv {
    previous: Option<String>,
  }

  impl RestoreDaemonPortEnv {
    fn set(port: u16) -> Self {
      let previous = std::env::var(paths::PORT_ENV).ok();
      std::env::set_var(paths::PORT_ENV, port.to_string());
      Self { previous }
    }
  }

  impl Drop for RestoreDaemonPortEnv {
    fn drop(&mut self) {
      match &self.previous {
        Some(v) => std::env::set_var(paths::PORT_ENV, v),
        None => std::env::remove_var(paths::PORT_ENV),
      }
    }
  }

  struct EphemeralDaemonGuard {
    port: u16,
  }

  impl Drop for EphemeralDaemonGuard {
    fn drop(&mut self) {
      let _ = stop();
      let _ = std::fs::remove_file(paths::state_file(self.port));
    }
  }

  fn wait_daemon_ready() {
    for _ in 0..40 {
      if Client::daemon_alive() {
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
    wait_daemon_ready();
    assert!(Client::daemon_alive(), "daemon should accept TCP on {}", port);
    assert_eq!(paths::read_persisted_mode(port), Some(DaemonMode::Ephemeral));
  }

  #[test]
  fn start_persistent_replaces_ephemeral_daemon() {
    let _spawn = DAEMON_SPAWN_LOCK.lock().unwrap();
    let port = unused_local_port();
    let _env = RestoreDaemonPortEnv::set(port);
    let _guard = EphemeralDaemonGuard { port };
    let _ = stop();
    ensure_for_run().expect("ensure_for_run");
    wait_daemon_ready();
    assert_eq!(paths::read_persisted_mode(port), Some(DaemonMode::Ephemeral));
    start_persistent().expect("start_persistent");
    wait_daemon_ready();
    assert!(Client::daemon_alive(), "persistent daemon should accept TCP on {}", port);
    assert_eq!(paths::read_persisted_mode(port), Some(DaemonMode::Persistent));
  }
}
