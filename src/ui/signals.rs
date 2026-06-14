//! Process-group isolation and terminal-signal handling — scsh's safety net around a run.
//!
//! Two cooperating pieces keep a stray signal from wrecking a run or the terminal:
//!
//! * Every child (git, the container runtime, the build) is spawned into **its own process
//!   group** via the safe `Command::process_group(0)`, so a signal the terminal broadcasts to
//!   scsh's foreground group never reaches the children and kills the run.
//! * scsh catches SIGINT/SIGTERM through the safe `signal-hook` crate (std has no signal API):
//!   on either it **restores the terminal** (in case the live board's raw mode / mouse reporting
//!   is active — see [`super::screen`]) and **tears the children down** (`kill`), then exits
//!   with the conventional code. The interactive board runs in raw mode, where Ctrl-C arrives as
//!   a key (handled there), so this path matters most off a TTY and for an external `kill`.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// Guards [`install`] so the signal thread is started exactly once.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// PIDs of scsh's live children (each in its own process group). On a signal the handler kills
/// these so a run aborts without orphaning containers.
static CHILD_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

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
      // it in raw mode with mouse reporting on), tear the isolated children down, then exit.
      super::screen::restore_terminal();
      terminate_children();
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

/// Track a live child so a signal can take it (and its container) down.
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

/// SIGTERM every registered child (the container runtime then tears its container down). Uses the
/// `kill` command, not FFI.
pub fn terminate_children() {
  let pids = CHILD_PIDS.lock().map(|v| v.clone()).unwrap_or_default();
  for pid in pids {
    let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
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
}
