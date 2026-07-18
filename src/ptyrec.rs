//! First-party PTY cast recorder (`scsh __record-pty`): run a command under a real
//! pseudo-terminal and write its output as an asciicast v3 recording, mirroring the raw
//! stream to stdout for the parent's live-board pump. This is what records image builds —
//! scsh itself is the recorder, so every build is a cast on every machine, with no host
//! `asciinema` (or any other tool) required. Std + libc only.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::json;

/// The C bits a PTY needs, declared by hand like the rest of scsh (no libc crate): the
/// `posix_openpt` family lives in plain libc on both macOS and glibc — unlike `openpty`,
/// which glibc historically kept in `-lutil`.
mod sys {
  pub const O_RDWR: i32 = 2;
  #[cfg(target_os = "linux")]
  pub const O_NOCTTY: i32 = 0o400;
  #[cfg(not(target_os = "linux"))]
  pub const O_NOCTTY: i32 = 0x2_0000;
  #[cfg(target_os = "linux")]
  pub const TIOCSWINSZ: u64 = 0x5414;
  #[cfg(not(target_os = "linux"))]
  pub const TIOCSWINSZ: u64 = 0x8008_7467;
  #[cfg(target_os = "linux")]
  pub const TIOCSCTTY: u64 = 0x540E;
  #[cfg(not(target_os = "linux"))]
  pub const TIOCSCTTY: u64 = 0x2000_7461;

  #[repr(C)]
  pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
  }

  #[link(name = "c")]
  extern "C" {
    pub fn posix_openpt(flags: i32) -> i32;
    pub fn grantpt(fd: i32) -> i32;
    pub fn unlockpt(fd: i32) -> i32;
    pub fn ptsname(fd: i32) -> *mut std::ffi::c_char;
    pub fn open(path: *const std::ffi::c_char, flags: i32, ...) -> i32;
    pub fn ioctl(fd: i32, req: u64, ...) -> i32;
    pub fn setsid() -> i32;
  }
}

/// Record `argv` under a fresh PTY of `cols`×`rows` into `cast_path` (asciicast v3,
/// NDJSON, flushed per event so the daemon's cast probe streams it live). The PTY output
/// is also mirrored to this process's stdout, byte for byte, so a parent pumping our
/// stdout sees exactly what a terminal would. Returns the child's exit code (127 when it
/// could not be spawned, 1 on recorder I/O failure).
pub fn record(cast_path: &Path, cols: u16, rows: u16, argv: &[String]) -> i32 {
  let (master, slave) = match open_pty(cols, rows) {
    Ok(pair) => pair,
    Err(e) => {
      eprintln!("scsh __record-pty: could not open a pty: {e}");
      return 1;
    }
  };

  let mut cast = match File::create(cast_path).map(BufWriter::new) {
    Ok(f) => f,
    Err(e) => {
      eprintln!("scsh __record-pty: could not create {}: {e}", cast_path.display());
      return 1;
    }
  };
  let header = format!(
    "{{\"version\": 3, \"term\": {{\"cols\": {cols}, \"rows\": {rows}}}, \"timestamp\": {}}}\n",
    crate::now_secs()
  );
  if cast.write_all(header.as_bytes()).and_then(|()| cast.flush()).is_err() {
    eprintln!("scsh __record-pty: could not write the cast header");
    return 1;
  }

  let mut cmd = Command::new(&argv[0]);
  cmd.args(&argv[1..]);
  let (child_in, child_out, child_err) = match (slave.try_clone(), slave.try_clone()) {
    (Ok(stdin), Ok(stdout)) => (stdin, stdout, slave),
    _ => {
      eprintln!("scsh __record-pty: could not clone the pty fd");
      return 1;
    }
  };
  cmd.stdin(Stdio::from(child_in)).stdout(Stdio::from(child_out)).stderr(Stdio::from(child_err));
  // New session with the PTY as controlling terminal, so the child sees a real tty
  // (BuildKit, podman, and Apple `container` all switch to their progress TUI on isatty).
  // SAFETY: setsid and ioctl are async-signal-safe, fine between fork and exec.
  unsafe {
    cmd.pre_exec(|| {
      sys::setsid();
      sys::ioctl(0, sys::TIOCSCTTY, 0);
      Ok(())
    });
  }
  let mut child = match cmd.spawn() {
    Ok(c) => c,
    Err(e) => {
      eprintln!("scsh __record-pty: could not spawn {}: {e}", argv[0]);
      return 127;
    }
  };
  // Drop the Command NOW: it still holds the parent's copies of the slave fds (the Stdio
  // handoff dup2s them into the child but keeps the originals until the Command drops),
  // and on Linux the master never reports EOF/EIO while ANY slave fd is open — the pump
  // would block forever after the child exits. (macOS tears the pty down with its session
  // leader, which is why this only bites Linux.)
  drop(cmd);

  pump(master, &mut cast);
  let _ = cast.flush();

  match child.wait() {
    Ok(status) => status.code().unwrap_or(1),
    Err(e) => {
      eprintln!("scsh __record-pty: wait failed: {e}");
      1
    }
  }
}

/// A connected (master, slave) PTY pair sized to `cols`×`rows`.
fn open_pty(cols: u16, rows: u16) -> std::io::Result<(OwnedFd, OwnedFd)> {
  // SAFETY: plain POSIX PTY allocation; every fd is checked before OwnedFd takes it, and
  // ptsname's static buffer is read before any other PTY call could clobber it.
  unsafe {
    let master = sys::posix_openpt(sys::O_RDWR | sys::O_NOCTTY);
    if master < 0 {
      return Err(std::io::Error::last_os_error());
    }
    let master = OwnedFd::from_raw_fd(master);
    if sys::grantpt(master.as_raw_fd()) != 0 || sys::unlockpt(master.as_raw_fd()) != 0 {
      return Err(std::io::Error::last_os_error());
    }
    let name = sys::ptsname(master.as_raw_fd());
    if name.is_null() {
      return Err(std::io::Error::last_os_error());
    }
    let slave = sys::open(name, sys::O_RDWR | sys::O_NOCTTY);
    if slave < 0 {
      return Err(std::io::Error::last_os_error());
    }
    let slave = OwnedFd::from_raw_fd(slave);
    let ws = sys::Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    sys::ioctl(slave.as_raw_fd(), sys::TIOCSWINSZ, &ws as *const sys::Winsize);
    Ok((master, slave))
  }
}

/// Drain the PTY master into cast events and this process's stdout until the child hangs
/// up (EOF on macOS, EIO on Linux). Event times are intervals since the previous event —
/// asciicast v3 — and each event line is flushed so a mid-build reader sees live growth.
fn pump(master: OwnedFd, cast: &mut BufWriter<File>) {
  let mut pty = File::from(master);
  let mut stdout = std::io::stdout();
  let started = Instant::now();
  let mut last_event = 0f64;
  // Carry for a UTF-8 sequence split across reads; ANSI output is UTF-8 in practice, and
  // anything genuinely invalid becomes U+FFFD rather than corrupting the cast JSON.
  let mut pending: Vec<u8> = Vec::new();
  let mut buf = [0u8; 8192];
  loop {
    match pty.read(&mut buf) {
      Ok(0) => break,
      Ok(n) => {
        let _ = stdout.write_all(&buf[..n]);
        let _ = stdout.flush();
        pending.extend_from_slice(&buf[..n]);
        let text = take_decodable_prefix(&mut pending);
        if !text.is_empty() {
          let now = started.elapsed().as_secs_f64();
          let interval = now - last_event;
          last_event = now;
          let line = format!("[{interval:.6}, \"o\", {}]\n", json::quote(&text));
          if cast.write_all(line.as_bytes()).and_then(|()| cast.flush()).is_err() {
            break;
          }
        }
      }
      Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
      Err(_) => break, // Linux reports EIO once the child side is gone.
    }
  }
  // A trailing incomplete sequence at hangup can never complete; flush it lossily.
  if !pending.is_empty() {
    let text = String::from_utf8_lossy(&pending).into_owned();
    let now = started.elapsed().as_secs_f64();
    let line = format!("[{:.6}, \"o\", {}]\n", now - last_event, json::quote(&text));
    let _ = cast.write_all(line.as_bytes());
  }
}

/// Remove and return the longest prefix of `pending` that does not end mid-UTF-8-sequence,
/// decoded lossily (invalid interior bytes become U+FFFD). At most three bytes — one
/// incomplete trailing character — stay behind for the next read to complete.
fn take_decodable_prefix(pending: &mut Vec<u8>) -> String {
  let keep = trailing_incomplete_len(pending);
  let cut = pending.len() - keep;
  let text = String::from_utf8_lossy(&pending[..cut]).into_owned();
  pending.drain(..cut);
  text
}

/// Length (0..=3) of an incomplete UTF-8 sequence at the very end of `bytes`.
fn trailing_incomplete_len(bytes: &[u8]) -> usize {
  for back in 1..=3.min(bytes.len()) {
    let b = bytes[bytes.len() - back];
    let needed = match b {
      0xC0..=0xDF => 2,
      0xE0..=0xEF => 3,
      0xF0..=0xF7 => 4,
      _ => continue, // continuation or ASCII byte — keep scanning back
    };
    // A start byte `back` positions from the end began a `needed`-byte character; it is
    // incomplete exactly when fewer than `needed` of its bytes have arrived.
    return if needed > back { back } else { 0 };
  }
  0
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_cast(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("scsh-ptyrec-{name}-{}.cast", crate::runtime::random_nonce_6()))
  }

  #[test]
  fn records_a_command_as_a_valid_v3_cast_and_propagates_exit() {
    let cast = temp_cast("ok");
    let argv =
      vec!["/bin/sh".to_string(), "-c".to_string(), "printf 'hello \\033[1mbold\\033[0m\\r\\n'; exit 0".to_string()];
    let code = record(&cast, 120, 40, &argv);
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(&cast).unwrap();
    let mut lines = text.lines();
    let header = lines.next().expect("header");
    assert!(header.contains("\"version\": 3") && header.contains("\"cols\": 120"), "{header}");
    let mut total = 0f64;
    let mut saw_bold = false;
    for line in lines {
      let parsed = json::parse(line).expect("event line parses");
      let json::Value::Array(items) = parsed else { panic!("event is an array: {line}") };
      let json::Value::Number(t) = &items[0] else { panic!("interval first: {line}") };
      total += t;
      if let json::Value::String(data) = &items[2] {
        if data.contains("\u{1b}[1mbold") {
          saw_bold = true;
        }
      }
    }
    assert!(saw_bold, "ANSI output captured through the pty: {text}");
    assert!(total >= 0.0);
    std::fs::remove_file(&cast).unwrap();
  }

  #[test]
  fn child_exit_code_and_missing_binary_are_reported() {
    let cast = temp_cast("codes");
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), "exit 3".to_string()];
    assert_eq!(record(&cast, 80, 24, &argv), 3);
    let argv = vec!["/definitely/not/a/binary".to_string()];
    assert_eq!(record(&cast, 80, 24, &argv), 127);
    let _ = std::fs::remove_file(&cast);
  }

  #[test]
  fn utf8_split_across_reads_stays_intact() {
    // "é" = 0xC3 0xA9: feed the bytes through the carry logic directly.
    let mut pending = vec![b'a', 0xC3];
    assert_eq!(take_decodable_prefix(&mut pending), "a");
    pending.push(0xA9);
    assert_eq!(take_decodable_prefix(&mut pending), "é");
    // A lone invalid byte is replaced, never corrupting the stream.
    let mut bad = vec![0xFF, b'b'];
    assert_eq!(take_decodable_prefix(&mut bad), "\u{FFFD}b");
  }
}
