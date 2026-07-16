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
mod websocket;
mod workflow;

pub use client::{post_once, spawn_daemon, Client};
/// Generate a session id: six lowercase letters (delegates to runtime nonce helper).
pub fn new_session_id() -> String {
  crate::runtime::random_nonce_6()
}

pub use model::{DaemonMode, ProcKind, ProcRecord, ProcStatus};
pub use paths::{
  absolutize_repo_path, base_url, consume_proc_restart, daemon_dir, daemon_port, daemon_port_reachable, now_unix_secs,
  read_live_pid, request_proc_restart,
};
pub(crate) use server::INTERNAL_REPO;
pub use server::{chapters_sidecar_path, Server};
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
