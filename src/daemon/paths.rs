//! Paths and liveness probes for the session browser daemon.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::model::DEFAULT_PORT;

/// Environment variable overriding the daemon HTTP port.
pub const PORT_ENV: &str = "SCSH_DAEMON_PORT";

/// Environment variable overriding the scsh home dir (default `~/.scsh`). Set by tests so a
/// daemon never touches the real home; users may set it to relocate persistent state.
pub const HOME_ENV: &str = "SCSH_HOME";

/// The scsh home dir — `$SCSH_HOME`, else `~/.scsh` — holding persistent state (the daemon
/// store DB). Created on first use by callers. Falls back to the daemon temp dir only if no
/// home is resolvable (headless/detached with no `HOME`), so the daemon always has somewhere.
pub fn scsh_home_dir() -> PathBuf {
  if let Some(dir) = std::env::var_os(HOME_ENV).filter(|s| !s.is_empty()) {
    return PathBuf::from(dir);
  }
  match std::env::var_os("HOME").filter(|s| !s.is_empty()) {
    Some(home) => PathBuf::from(home).join(".scsh"),
    None => daemon_dir(),
  }
}

/// The daemon's redb store: `~/.scsh/daemon-<port>.redb`. Per-port so daemons on different
/// ports don't contend for one file (redb allows a single process to hold a DB at a time).
pub fn store_db_file(port: u16) -> PathBuf {
  scsh_home_dir().join(format!("daemon-{port}.redb"))
}

/// Current unix timestamp in seconds.
pub fn now_unix_secs() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Resolve the daemon port from the environment or default.
pub fn daemon_port() -> u16 {
  std::env::var(PORT_ENV).ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// Directory under the system temp dir for daemon artifacts (PID + state JSON).
pub fn daemon_dir() -> PathBuf {
  std::env::temp_dir().join("scsh-daemon")
}

pub fn pid_file(port: u16) -> PathBuf {
  daemon_dir().join(format!("daemon-{port}.pid"))
}

pub fn prune_file(port: u16) -> PathBuf {
  daemon_dir().join(format!("prune-{port}.json"))
}

/// A tiny cross-process marker holding the running daemon's mode (`persistent`/`ephemeral`).
/// The store DB is redb, which only one process may open at a time, so the mode — which the
/// CLI reads to decide whether to replace a running ephemeral daemon — lives in this plain
/// file next to the PID instead.
pub fn mode_file(port: u16) -> PathBuf {
  daemon_dir().join(format!("daemon-{port}.mode"))
}

/// True when TCP connects to the daemon's localhost port within a short timeout.
pub fn daemon_port_reachable(port: u16) -> bool {
  let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("valid localhost address");
  TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// True when the port serves scsh session-browser HTTP (not merely an open TCP socket).
pub fn daemon_api_responds(port: u16) -> bool {
  if !daemon_port_reachable(port) {
    return false;
  }
  let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("valid localhost address");
  let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
    return false;
  };
  stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
  stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
  let req = "GET /api/v1/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
  if stream.write_all(req.as_bytes()).is_err() {
    return false;
  }
  let mut resp = String::new();
  if stream.read_to_string(&mut resp).is_err() {
    return false;
  }
  resp.starts_with("HTTP/1.1 200") && resp.contains("application/json")
}

/// Daemon mode from the cross-process mode marker, when present and valid.
pub fn read_persisted_mode(port: u16) -> Option<super::model::DaemonMode> {
  let text = std::fs::read_to_string(mode_file(port)).ok()?;
  super::model::DaemonMode::parse(text.trim())
}

/// Write the mode marker (best-effort) so `read_persisted_mode` can report it cross-process.
pub fn write_mode_marker(port: u16, mode: super::model::DaemonMode) {
  let _ = std::fs::create_dir_all(daemon_dir());
  let _ = std::fs::write(mode_file(port), mode.as_str());
}

/// Send a POSIX signal to `pid` (no-op on non-Unix).
pub fn signal_process(pid: u32, sig: i32) {
  #[cfg(unix)]
  {
    // SAFETY: `kill` with a positive pid and a valid signal number is defined for process signaling.
    unsafe {
      libc::kill(pid as i32, sig);
    }
  }
}

/// True when a process with `pid` appears to be alive.
pub fn pid_alive(pid: u32) -> bool {
  if pid == 0 {
    return false;
  }
  #[cfg(unix)]
  {
    // SAFETY: `kill(pid, 0)` probes existence without delivering a signal.
    unsafe { libc::kill(pid as i32, 0) == 0 }
  }
  #[cfg(not(unix))]
  {
    false
  }
}

/// True when `pid` is a live `scsh __daemon-serve` process (cmdline probe; Unix only).
pub fn is_scsh_daemon_pid(pid: u32) -> bool {
  #[cfg(unix)]
  {
    process_args(pid).is_some_and(|args| args.contains("scsh") && args.contains("__daemon-serve"))
  }
  #[cfg(not(unix))]
  {
    let _ = pid;
    false
  }
}

#[cfg(unix)]
fn process_args(pid: u32) -> Option<String> {
  let output = std::process::Command::new("ps").arg("-p").arg(pid.to_string()).arg("-o").arg("args=").output().ok()?;
  if !output.status.success() {
    return None;
  }
  Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Read the PID written by a running daemon, if the process is still alive and is our daemon.
pub fn read_live_pid(port: u16) -> Option<u32> {
  let text = std::fs::read_to_string(pid_file(port)).ok()?;
  let pid = text.trim().parse::<u32>().ok()?;
  if pid_alive(pid) && is_scsh_daemon_pid(pid) {
    Some(pid)
  } else {
    None
  }
}

/// Where browser-created PROJECTS live: `$SCSH_HOME/projects/<name>` — fresh git repos the
/// daemon scaffolds so tests and demos can start from the web UI with no terminal at all.
/// Created on the fly; a bare (slash-free) name in the "open" box resolves here.
pub fn projects_dir() -> std::path::PathBuf {
  crate::runtime::scsh_home().join("projects")
}

/// Base URL for the daemon on localhost.
pub fn base_url(port: u16) -> String {
  format!("http://127.0.0.1:{port}")
}

pub fn session_url(port: u16, session_id: &str) -> String {
  format!("{}/job/{}", base_url(port), session_id)
}

/// Canonical absolute path for display and storage (resolves relative paths against cwd).
pub fn absolutize_repo_path(path: &Path) -> String {
  let path = if path.is_absolute() {
    path.to_path_buf()
  } else {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
  };
  std::fs::canonicalize(&path).unwrap_or(path).to_string_lossy().into_owned()
}

#[cfg(unix)]
/// Pre-exec child step: parent exits immediately; surviving child becomes session leader.
/// Called from `Command::pre_exec` (async-signal-safe syscalls only; combined with spawn
/// this is the second fork of the double-fork detach pattern).
pub fn daemon_detach_child() -> std::io::Result<()> {
  // SAFETY: first fork — parent exits so only the child continues (double-fork detach).
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    return Err(std::io::Error::last_os_error());
  }
  if pid > 0 {
    // SAFETY: parent of the first fork must not run daemon setup; `_exit` avoids atexit handlers.
    unsafe { libc::_exit(0) };
  }
  // SAFETY: `setsid` in the surviving child creates a new session (async-signal-safe in pre_exec).
  let sid = unsafe { libc::setsid() };
  if sid < 0 {
    return Err(std::io::Error::last_os_error());
  }
  Ok(())
}

#[cfg(unix)]
mod libc {
  #[link(name = "c")]
  extern "C" {
    pub fn kill(pid: i32, sig: i32) -> i32;
    pub fn fork() -> i32;
    pub fn setsid() -> i32;
    pub fn _exit(code: i32) -> !;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pid_file_names_include_port() {
    let p = pid_file(7274);
    assert!(p.to_string_lossy().contains("7274"));
    assert!(p.to_string_lossy().ends_with(".pid"));
  }

  #[test]
  fn session_url_format() {
    let u = session_url(7274, "abcdef");
    assert_eq!(u, "http://127.0.0.1:7274/job/abcdef");
  }
}
