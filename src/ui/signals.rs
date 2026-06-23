//! Process-group isolation and terminal-signal handling — scsh's safety net around a run.
//!
//! Two cooperating pieces keep a stray signal from wrecking a run or the terminal:
//!
//! * Every child (git, the container runtime, the build) is spawned into **its own process
//!   group** via the safe `Command::process_group(0)`, so a signal the terminal broadcasts to
//!   scsh's foreground group never reaches the children and kills the run.
//! * scsh catches SIGINT/SIGTERM through the safe `signal-hook` crate (std has no signal API):
//!   on either it **restores the terminal** (in case the live board's raw mode / mouse reporting
//!   is active — see [`super::screen`]) and **tears the children and containers down**, then
//!   exits with the conventional code. The interactive board runs in raw mode, where Ctrl-C
//!   arrives as a key (handled there), so this path matters most off a TTY and for an external
//!   `kill`.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// Guards [`install`] so the signal thread is started exactly once.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// PIDs of scsh's live children (each in its own process group). On abort we SIGTERM these,
/// wait one second, then SIGKILL any that remain.
static CHILD_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Named containers started by skill runs (`runtime`, `--name`). On abort we stop each one
/// gently, wait one second, then force-kill — killing the client alone can leave a
/// daemon-backed container running.
static CONTAINERS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Start scsh's terminal-signal handling (idempotent; no-op if registration fails). A background
/// thread owns SIGINT/SIGTERM: restore the terminal, terminate the children, exit. Call once,
/// before a run.
pub fn install() {
  if INSTALLED.swap(true, Ordering::SeqCst) {
    return;
  }
  let mut signals = match Signals::new([SIGINT, SIGTERM]) {
    Ok(s) => s,
    Err(_) => return, // couldn't register — fall back to the default disposition
  };
  thread::spawn(move || {
    for sig in signals.forever() {
      // A kill / terminal close / off-TTY Ctrl-C: put the terminal back (the live board may have
      // it in raw mode with mouse reporting on), tear everything down, then exit.
      super::screen::restore_terminal();
      terminate_all();
      std::process::exit(128 + sig);
    }
  });
}

/// Put a child in its OWN process group, so a terminal signal broadcast to scsh's foreground
/// group never reaches it. `Command::process_group` is safe std. No-op off unix.
pub fn isolate_child(cmd: &mut Command) {
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0); // 0 → a new group whose id is the child's own pid
  }
  #[cfg(not(unix))]
  let _ = cmd;
}

/// Track a live child so abort can take it down.
pub fn register_child(pid: u32) {
  if let Ok(mut v) = CHILD_PIDS.lock() {
    v.push(pid);
  }
}

/// Stop tracking a child once it has been reaped.
pub fn unregister_child(pid: u32) {
  if let Ok(mut v) = CHILD_PIDS.lock() {
    v.retain(|&p| p != pid);
  }
}

/// Track a named container for the duration of a skill run.
pub fn register_container(runtime: &str, name: &str) {
  if let Ok(mut v) = CONTAINERS.lock() {
    v.push((runtime.to_string(), name.to_string()));
  }
}

/// Stop tracking a container once its run has finished.
pub fn unregister_container(runtime: &str, name: &str) {
  if let Ok(mut v) = CONTAINERS.lock() {
    v.retain(|(r, n)| !(r == runtime && n == name));
  }
}

/// RAII guard: register on creation, unregister on drop.
pub struct ContainerGuard {
  runtime: String,
  name: String,
}

impl ContainerGuard {
  pub fn new(runtime: &str, name: &str) -> ContainerGuard {
    register_container(runtime, name);
    ContainerGuard { runtime: runtime.to_string(), name: name.to_string() }
  }
}

impl Drop for ContainerGuard {
  fn drop(&mut self) {
    unregister_container(&self.runtime, &self.name);
  }
}

/// SIGTERM every registered child, wait one second, then SIGKILL. Uses the `kill` command.
pub fn terminate_children() {
  let pids = CHILD_PIDS.lock().map(|v| v.clone()).unwrap_or_default();
  for pid in &pids {
    let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
  }
  if !pids.is_empty() {
    thread::sleep(Duration::from_secs(1));
  }
  for pid in &pids {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
  }
}

/// Stop one named container: SIGTERM via the runtime CLI, wait one second, then SIGKILL.
pub fn stop_container(runtime: &str, name: &str) {
  signal_container(runtime, name, "TERM");
  thread::sleep(Duration::from_secs(1));
  signal_container(runtime, name, "KILL");
}

fn signal_container(runtime: &str, name: &str, sig: &str) {
  let _ = Command::new(runtime)
    .args(["kill", "-s", sig, name])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();
}

/// SIGTERM/SIGKILL every registered child and every tracked container — used on Ctrl-C / SIGTERM.
pub fn terminate_all() {
  let containers = CONTAINERS.lock().map(|v| v.clone()).unwrap_or_default();
  terminate_children();
  for (runtime, name) in &containers {
    signal_container(runtime, name, "TERM");
  }
  if !containers.is_empty() {
    thread::sleep(Duration::from_secs(1));
  }
  for (runtime, name) in &containers {
    signal_container(runtime, name, "KILL");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(target_os = "linux")]
  #[test]
  fn isolated_child_runs_in_its_own_process_group() {
    // Field 5 of /proc/<pid>/stat is the process group id (after comm, which is in parens and
    // may contain spaces — so split after the last ')').
    fn pgid(stat_path: &str) -> i32 {
      let stat = std::fs::read_to_string(stat_path).unwrap();
      let after = &stat[stat.rfind(')').unwrap() + 1..];
      after.split_whitespace().nth(2).unwrap().parse().unwrap() // state ppid pgrp …
    }
    let ours = pgid("/proc/self/stat");
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    isolate_child(&mut cmd);
    let mut child = cmd.spawn().expect("spawn sleep");
    let id = child.id();
    let theirs = pgid(&format!("/proc/{id}/stat"));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(theirs, id as i32, "the child should lead its own process group");
    assert_ne!(theirs, ours, "the child must not share scsh's process group");
  }

  #[test]
  fn register_and_unregister_track_children() {
    let fake = 4_242_424; // unlikely to collide with a real pid during the test
    register_child(fake);
    assert!(CHILD_PIDS.lock().unwrap().contains(&fake));
    unregister_child(fake);
    assert!(!CHILD_PIDS.lock().unwrap().contains(&fake));
  }

  #[test]
  fn register_and_unregister_track_containers() {
    register_container("docker", "scsh-test");
    assert!(CONTAINERS.lock().unwrap().iter().any(|(r, n)| r == "docker" && n == "scsh-test"));
    unregister_container("docker", "scsh-test");
    assert!(!CONTAINERS.lock().unwrap().iter().any(|(r, n)| r == "docker" && n == "scsh-test"));
  }

  #[test]
  fn container_guard_unregisters_on_drop() {
    {
      let _g = ContainerGuard::new("docker", "scsh-guard-test");
      assert!(CONTAINERS.lock().unwrap().iter().any(|(_, n)| n == "scsh-guard-test"));
    }
    assert!(!CONTAINERS.lock().unwrap().iter().any(|(_, n)| n == "scsh-guard-test"));
  }
}
