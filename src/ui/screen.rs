//! The interactive live board's **terminal driver** — the only side-effecting half of the UI.
//!
//! On an attended terminal it goes into raw mode with mouse reporting on (but **NOT** the
//! alternate screen — the board is drawn INLINE in the normal buffer, so the terminal's own
//! scrollback keeps working and the run never blanks the whole screen). A render+event loop on
//! its own thread animates the [`Model`], redraws it in place each tick (≈12 fps), and turns
//! input into model edits —
//!
//! * **left-click a row** → toggle that proc's triangle (expand / collapse its output),
//! * **wheel / ↑↓ / PgUp·PgDn / Home·End** → scroll (it follows the tail until you scroll up),
//! * **e / c** → expand / collapse every proc, **Ctrl-C** → abort the run.
//!
//! The board is anchored just below whatever was printed before the run and is capped to the
//! screen height (taller output scrolls within the board, not the screen). Worker threads never
//! touch the terminal; they only edit the shared `Model` through a [`Proc`] handle (mark started,
//! pump a child's output in as timestamped lines, finish ✓/✗). On finish the driver **wipes the
//! live region and prints a compact, collapsed ✓/✗ summary in its place** — so what's left is
//! one line per proc, in the normal scrollback, never the whole expanded board.
//!
//! Off a TTY there is no take-over: each proc announces itself with a `▶` line and a plain ✓/✗
//! line (the build proc also echoes its output), so pipes and CI stay readable.

use std::io::{stderr, BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use console::{style, Style};
use crossterm::event::{
  poll, read, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
  KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, Clear, ClearType};
use crossterm::{cursor, queue, style::Print, terminal};

use super::clock::{clean_line, format_elapsed};
use super::live::{Model, Row, Status, Sty};
use super::signals::{isolate_child, register_child, terminate_all, unregister_child};
use super::TICK;

/// Optional session-browser event sink (see [`crate::daemon::Client`]).
pub type EventSink = std::sync::Arc<crate::daemon::Client>;

/// True while raw mode / mouse reporting is active, so [`restore_terminal`] is idempotent and a
/// signal handler or panic can always put the terminal back.
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// True while the keyboard-enhancement protocol is pushed (so we pop exactly what we pushed).
static ENHANCED: AtomicBool = AtomicBool::new(false);

/// Put the terminal back the way we found it: show the cursor, turn off mouse reporting and raw
/// mode. Idempotent and safe to call from a panic hook or signal handler — it no-ops unless a TUI
/// is actually active. (The board is drawn inline in the normal buffer — there is no alternate
/// screen to leave — so the caller is responsible for clearing the live region first.)
pub fn restore_terminal() {
  if !TUI_ACTIVE.swap(false, Ordering::SeqCst) {
    return;
  }
  let mut out = stderr();
  if ENHANCED.swap(false, Ordering::SeqCst) {
    let _ = queue!(out, PopKeyboardEnhancementFlags);
  }
  let _ = queue!(out, DisableMouseCapture, cursor::Show);
  let _ = out.flush();
  let _ = disable_raw_mode();
}

/// The live board UI for a whole run. Attended: drives the inline board on a background thread.
/// Off a TTY: a no-op shell whose [`Proc`] handles print plain lines.
pub struct LiveUi {
  attended: bool,
  model: Arc<Mutex<Model>>,
  /// Per-proc start instants for elapsed time on the live board.
  starts: Arc<Mutex<Vec<Option<Instant>>>>,
  stop: Arc<AtomicBool>,
  /// Screen row where the inline board's first line was last drawn — published by the render thread
  /// so `finish` can clear from there downward.
  top: Arc<AtomicUsize>,
  render: Option<JoinHandle<()>>,
  sink: Option<EventSink>,
}

impl LiveUi {
  /// Start a live board. `attended` should be [`console::user_attended_stderr`]; when false the
  /// board degrades to plain lines and never touches the terminal. An optional `sink` forwards
  /// proc lifecycle events to the session browser daemon.
  pub fn new(attended: bool, sink: Option<EventSink>) -> LiveUi {
    let model = Arc::new(Mutex::new(Model::new()));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let top = Arc::new(AtomicUsize::new(0));
    let render = if attended && enter_tui() {
      install_panic_hook();
      let (m, s, st, tp) = (Arc::clone(&model), Arc::clone(&starts), Arc::clone(&stop), Arc::clone(&top));
      Some(thread::spawn(move || render_loop(m, s, st, tp)))
    } else {
      None
    };
    // If we asked for a TUI but couldn't enter it (enter_tui returned false), fall back to plain.
    let attended = render.is_some();
    LiveUi { attended, model, starts, stop, top, render, sink }
  }

  /// Declare a proc (the image build, or a skill) up front, returning the handle a worker drives.
  /// `tail` only matters off-TTY: a tailing proc echoes its output lines (used for the build).
  pub fn proc(&self, label: impl Into<String>, tail: bool) -> Proc {
    let label = label.into();
    let i = {
      let mut m = self.model.lock().unwrap();
      m.add(label.clone())
    };
    self.starts.lock().unwrap().push(None);
    let sink = self.sink.clone();
    Proc {
      i,
      label,
      attended: self.attended,
      tail,
      model: Arc::clone(&self.model),
      starts: Arc::clone(&self.starts),
      sink,
    }
  }

  /// Pin the board viewport to the top (manifest-first row order). Called once all procs are
  /// declared so [0] lines up with the first skill row.
  pub fn pin_board_to_top(&self) {
    self.model.lock().unwrap().scroll_to_top();
  }

  /// End the run: stop the render loop, then (when we ran the board) wipe the live region and
  /// print a compact, collapsed ✓/✗ summary in its place — so what's left on screen is just one
  /// line per proc, in the normal scrollback. Off a TTY the per-proc lines already streamed.
  pub fn finish(mut self) {
    self.stop.store(true, Ordering::SeqCst);
    if let Some(h) = self.render.take() {
      let _ = h.join();
    }
    if self.attended {
      // The render thread parked the board at `top`; clear from there down (raw mode), restore
      // the terminal, then print the summary in cooked mode where it scrolls normally.
      let top = self.top.load(Ordering::SeqCst) as u16;
      let mut out = stderr();
      let _ = queue!(out, cursor::MoveTo(0, top), Clear(ClearType::FromCursorDown));
      let _ = out.flush();
      restore_terminal();
      for line in summary_lines(&self.model.lock().unwrap()) {
        eprintln!("{line}");
      }
    }
  }
}

impl Drop for LiveUi {
  fn drop(&mut self) {
    // Belt and braces: if `finish` wasn't called (e.g. an early return), still restore the term.
    self.stop.store(true, Ordering::SeqCst);
    if let Some(h) = self.render.take() {
      let _ = h.join();
    }
    restore_terminal();
  }
}

/// Screen-activity watchdog for [`Proc::run_watched`]: the growing file whose CONTENT is the
/// heartbeat (for a skill run, the bind-mounted asciinema cast), and how long it may go
/// without anything new before the child is killed as inactive.
///
/// Raw growth is not liveness: a wedged agent's TUI spinner redraws forever, so the cast keeps
/// growing while nothing happens underneath (observed live: a 30-minute grok hang whose cast
/// grew the whole time). Activity therefore counts only when the file gains a line whose
/// normalized content is NOVEL — the asciicast event timestamp is stripped and digits are
/// erased, so a spinner cycling a fixed frame set (even with a ticking elapsed-seconds
/// counter) stops registering once every frame has been seen, while genuine agent output
/// keeps producing never-seen lines.
pub struct ActivityWatch {
  /// Polled (`~100ms`) for new content; a file that never appears counts as never active.
  pub file: std::path::PathBuf,
  /// Silence budget: kill the child when `file` has shown nothing novel for this long.
  pub limit: Duration,
}

/// Bounded memory of normalized cast-line hashes already seen, plus the read cursor into the
/// watched file. Backs one [`ActivityWatch`] evaluation loop.
struct NoveltyWatch {
  file: std::path::PathBuf,
  /// Byte offset of the next unread byte (reset when the file shrinks or vanishes).
  offset: u64,
  /// Trailing bytes of an incomplete final line, kept until its newline arrives.
  carry: Vec<u8>,
  seen: std::collections::HashSet<u64>,
  /// Insertion order for FIFO eviction, so `seen` stays bounded on long runs.
  order: std::collections::VecDeque<u64>,
}

/// Spinner cycles are tiny; this only needs to exceed the largest realistic set of distinct
/// idle frames. Evicting truly old frames errs toward counting them as novel again — the
/// safe direction (it can only delay a kill, never cause a false one).
const NOVELTY_MEMORY: usize = 4096;
/// Per-poll read cap so one poll never stalls the supervision loop on a runaway file.
const NOVELTY_READ_CAP: u64 = 1 << 20;

impl NoveltyWatch {
  fn new(file: &std::path::Path) -> Self {
    NoveltyWatch {
      file: file.to_path_buf(),
      offset: 0,
      carry: Vec::new(),
      seen: std::collections::HashSet::new(),
      order: std::collections::VecDeque::new(),
    }
  }

  /// Hash one raw cast line with its volatile parts erased: the leading `[<time>,` of an
  /// asciicast event is dropped (every frame has a fresh timestamp) and ASCII digits are
  /// skipped (elapsed-seconds counters and percent readouts tick without meaning progress).
  fn normalized_hash(line: &[u8]) -> u64 {
    use std::hash::Hasher;
    let start =
      if line.first() == Some(&b'[') { line.iter().position(|b| *b == b',').map(|i| i + 1).unwrap_or(0) } else { 0 };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in &line[start..] {
      if !b.is_ascii_digit() {
        h.write_u8(*b);
      }
    }
    h.finish()
  }

  /// Read whatever the file gained since the last poll; `true` when any complete new line
  /// hashes to something not seen before (= genuine screen novelty).
  fn poll(&mut self) -> bool {
    use std::io::{Read, Seek};
    let Ok(meta) = std::fs::metadata(&self.file) else { return false };
    if meta.len() < self.offset {
      // Truncated or replaced (a re-recorded cast): start over; fresh content counts anew.
      self.offset = 0;
      self.carry.clear();
    }
    if meta.len() == self.offset {
      return false;
    }
    let Ok(mut f) = std::fs::File::open(&self.file) else { return false };
    if f.seek(std::io::SeekFrom::Start(self.offset)).is_err() {
      return false;
    }
    let mut chunk = Vec::new();
    let Ok(read) = f.take(NOVELTY_READ_CAP).read_to_end(&mut chunk) else { return false };
    self.offset += read as u64;
    let mut novel = false;
    for byte in chunk {
      if byte == b'\n' {
        let hash = Self::normalized_hash(&self.carry);
        self.carry.clear();
        if self.seen.insert(hash) {
          novel = true;
          self.order.push_back(hash);
          if self.order.len() > NOVELTY_MEMORY {
            if let Some(old) = self.order.pop_front() {
              self.seen.remove(&old);
            }
          }
        }
      } else {
        self.carry.push(byte);
      }
    }
    novel
  }
}

/// Why an [`ActivityWatch`]ed child was killed, if it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Killed {
  /// Not killed — the child exited on its own (its exit status is the verdict).
  No,
  /// The wall-clock `timeout` elapsed.
  Timeout,
  /// The watched file showed no activity for the watchdog's limit.
  Inactive,
}

/// A worker's handle to one proc: mark it started, run a child while pumping its output into the
/// model as timestamped lines, and finish it ✓/✗.
#[derive(Clone)]
pub struct Proc {
  i: usize,
  label: String,
  attended: bool,
  tail: bool,
  model: Arc<Mutex<Model>>,
  starts: Arc<Mutex<Vec<Option<Instant>>>>,
  sink: Option<EventSink>,
}

impl Proc {
  /// Row index in the live board (and in the session browser).
  pub fn index(&self) -> usize {
    self.i
  }

  /// Mark the proc running and start its clock. Off-TTY, announce it with a `▶` line.
  pub fn start(&self) {
    let mut starts = self.starts.lock().unwrap();
    let started = starts.get_mut(self.i).unwrap();
    if started.is_none() {
      *started = Some(Instant::now());
    }
    drop(starts);
    self.model.lock().unwrap().set_status(self.i, Status::Running);
    if let Some(s) = &self.sink {
      s.proc_start(self.i);
    }
    if !self.attended {
      eprintln!("{} {}…", style("▶").cyan(), style(&self.label).bold());
    }
  }

  /// Set the dim header note (a phase, e.g. "cloning…"). Forwards to the session browser when connected.
  pub fn note(&self, msg: &str) {
    if let Some(s) = &self.sink {
      s.proc_note(self.i, msg);
    }
    if self.attended {
      self.model.lock().unwrap().set_note(self.i, Some(msg.to_string()));
    }
  }

  /// Append a timestamped line to this proc's captured output. Off-TTY, only tailing procs
  /// (image builds) echo lines to the terminal; skill rows keep clone/fsck chatter on the board.
  pub fn emit(&self, msg: &str) {
    let at = self.start_instant().elapsed().as_secs_f64();
    if let Some(s) = &self.sink {
      s.proc_line(self.i, at, msg);
    }
    // Attended board and daemon-backed off-TTY runs keep lines in the model; plain off-TTY runs
    // only echo (main behavior) unless a sink needs the lines for the session browser.
    if self.attended || self.sink.is_some() {
      self.model.lock().unwrap().push_line(self.i, at, msg.to_string());
    }
    if !self.attended && (self.tail || self.sink.is_none()) {
      eprintln!("  {}", style(msg).dim());
    }
  }

  /// Run `program args` to completion, pumping each output line into the model (stamped relative
  /// to this proc's start) and onto the header note. Returns `(success, last_line)`.
  pub fn run(&self, program: &str, args: &[String]) -> std::io::Result<(bool, Option<String>)> {
    let (ok, _killed, last) = self.exec(program, args, None, None, None)?;
    Ok((ok, last))
  }

  /// Last `max` lines captured for this proc (stdout/stderr pump output).
  pub fn tail_lines(&self, max: usize) -> Vec<String> {
    self.model.lock().unwrap().tail_lines(self.i, max)
  }

  /// Like [`Proc::run`] but kills the child past the wall-clock `timeout` and/or when
  /// `watch` sees no screen activity for its limit (`None`s wait forever). Returns
  /// `(success, why_killed, last_line)`.
  pub fn run_watched(
    &self, program: &str, args: &[String], timeout: Option<Duration>, watch: Option<&ActivityWatch>,
  ) -> std::io::Result<(bool, Killed, Option<String>)> {
    self.exec(program, args, None, timeout, watch)
  }

  /// Like [`Proc::run`] but feeds `stdin` to the child and then closes it (EOF) — how the image
  /// build streams the generated Dockerfile to `docker build -` while its output is tailed.
  pub fn run_with_stdin(
    &self, program: &str, args: &[String], stdin: &[u8],
  ) -> std::io::Result<(bool, Option<String>)> {
    let (ok, _killed, last) = self.exec(program, args, Some(stdin), None, None)?;
    Ok((ok, last))
  }

  /// Spawn `program args`, pump both output streams into the model as timestamped lines,
  /// optionally feed `stdin` then EOF, and optionally kill on `timeout`. The single core the
  /// public `run*` methods delegate to.
  fn exec(
    &self, program: &str, args: &[String], stdin: Option<&[u8]>, timeout: Option<Duration>,
    watch: Option<&ActivityWatch>,
  ) -> std::io::Result<(bool, Killed, Option<String>)> {
    let started = self.start_instant();
    let mut command = Command::new(program);
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    isolate_child(&mut command);
    let mut child = command.spawn()?;
    let pid = child.id();
    register_child(pid);

    let last = Arc::new(Mutex::new(None::<String>));
    let mut pumps: Vec<JoinHandle<()>> = Vec::new();
    if let Some(out) = child.stdout.take() {
      pumps.push(self.pump(out, started, Arc::clone(&last)));
    }
    if let Some(err) = child.stderr.take() {
      pumps.push(self.pump(err, started, Arc::clone(&last)));
    }
    // Feed stdin only after the pumps are draining output, so a large payload can't deadlock
    // against a full output pipe. Dropping the handle signals EOF.
    if let Some(bytes) = stdin {
      if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(bytes);
      }
    }

    let mut killed = Killed::No;
    let status = if timeout.is_none() && watch.is_none() {
      child.wait()?
    } else {
      // The activity clock starts now: a watched file that never appears (or never shows a
      // novel line) still trips the watchdog once the limit elapses.
      let mut novelty = watch.map(|w| NoveltyWatch::new(&w.file));
      let mut last_activity = std::time::Instant::now();
      loop {
        if let Some(s) = child.try_wait()? {
          break s;
        }
        if let Some(limit) = timeout {
          if started.elapsed() >= limit {
            let _ = child.kill();
            killed = Killed::Timeout;
            break child.wait()?;
          }
        }
        if let Some(w) = watch {
          if novelty.as_mut().is_some_and(NoveltyWatch::poll) {
            last_activity = std::time::Instant::now();
          }
          if last_activity.elapsed() >= w.limit {
            let _ = child.kill();
            killed = Killed::Inactive;
            break child.wait()?;
          }
        }
        thread::sleep(Duration::from_millis(100));
      }
    };
    unregister_child(pid);
    for p in pumps {
      let _ = p.join();
    }
    let last = last.lock().unwrap().clone();
    Ok((status.success(), killed, last))
  }

  /// Finish green: set the proc ✓, freeze its clock, and attach an optional detail. Off-TTY,
  /// print the plain `✓ label  elapsed  detail` line now.
  pub fn finish_ok(&self, detail: Option<&str>) {
    self.finish(Status::Ok, detail, None);
  }

  /// Finish orange: the durable result is valid, but the harness or container did not complete
  /// its teardown cleanly. Dependencies may proceed; the infrastructure wrinkle stays visible.
  pub fn finish_graceful(&self, detail: Option<&str>) {
    self.finish(Status::Graceful, detail, None);
  }

  /// Finish as never-run: a workflow step decided out of the run (gate false, or a needed
  /// step was skipped). Renders ⊘ with the reason, on the board and in the session browser.
  pub fn finish_skipped(&self, why: &str) {
    self.finish(Status::Skipped, Some(why), None);
  }

  /// Finish red: as [`Proc::finish_ok`] but ✗ (the detail renders in red).
  pub fn finish_fail(&self, reason: &str, detail: Option<&str>) {
    crate::failure::log_proc(reason, &self.label, detail);
    let combined = detail.map(|d| crate::failure::format_detail(reason, d));
    self.finish(Status::Fail, combined.as_deref(), Some(reason));
  }

  /// Finish green with an EXPLICIT elapsed time instead of this proc's own clock, and clear its
  /// note. Used for a cache hit, which does no work of its own but should show the original run's
  /// duration (not ~0s), so the board reads the same as when the result was first produced.
  pub fn finish_ok_elapsed(&self, detail: Option<&str>, elapsed: f64) {
    self.model.lock().unwrap().set_note(self.i, None);
    self.finish_with(Status::Ok, detail, None, Some(elapsed));
  }

  fn finish(&self, status: Status, detail: Option<&str>, fail_reason: Option<&str>) {
    self.finish_with(status, detail, fail_reason, None);
  }

  fn finish_with(&self, status: Status, detail: Option<&str>, fail_reason: Option<&str>, elapsed: Option<f64>) {
    let elapsed = elapsed.unwrap_or_else(|| self.start_instant().elapsed().as_secs_f64());
    {
      let mut m = self.model.lock().unwrap();
      m.set_elapsed(self.i, elapsed);
      m.set_status(self.i, status);
      m.set_detail(self.i, detail.filter(|d| !d.is_empty()).map(str::to_string));
    }
    if let Some(s) = &self.sink {
      let ps = match status {
        Status::Ok => crate::daemon::ProcStatus::Ok,
        Status::Graceful => crate::daemon::ProcStatus::Graceful,
        Status::Fail => crate::daemon::ProcStatus::Fail,
        Status::Running => crate::daemon::ProcStatus::Running,
        Status::Queued => crate::daemon::ProcStatus::Waiting,
        Status::Skipped => crate::daemon::ProcStatus::Skipped,
      };
      s.proc_finish(self.i, ps, fail_reason, detail, elapsed);
    }
    if !self.attended {
      eprintln!("{}", summary_line(&self.label, status, elapsed, detail));
    }
  }

  fn start_instant(&self) -> Instant {
    self.starts.lock().unwrap().get(self.i).copied().flatten().unwrap_or_else(Instant::now)
  }

  /// Read a child stream line by line, cleaning each, recording the latest, appending it to the
  /// model (stamped relative to `started`) and onto the header note. Off-TTY a tailing proc
  /// echoes the line so the build log survives in pipes/CI.
  fn pump<R: Read + Send + 'static>(
    &self, reader: R, started: Instant, last: Arc<Mutex<Option<String>>>,
  ) -> JoinHandle<()> {
    let (i, attended, tail, model, sink) =
      (self.i, self.attended, self.tail, Arc::clone(&self.model), self.sink.clone());
    thread::spawn(move || {
      for line in BufReader::new(reader).lines() {
        let Ok(raw) = line else { break };
        let cleaned = clean_line(&raw);
        if cleaned.is_empty() {
          continue;
        }
        let at = started.elapsed().as_secs_f64();
        if let Some(s) = &sink {
          s.proc_line(i, at, &cleaned);
        }
        {
          let mut m = model.lock().unwrap();
          m.push_line(i, at, cleaned.clone());
          if attended {
            m.set_note(i, Some(cleaned.clone()));
          }
        }
        if !attended && tail {
          eprintln!("  {}", style(&cleaned).dim());
        }
        *last.lock().unwrap() = Some(cleaned);
      }
    })
  }
}

// --- terminal setup / teardown ------------------------------------------------------------

/// Enter raw mode with mouse reporting and a hidden cursor — but NO alternate screen. The board
/// is drawn INLINE in the normal buffer, so the terminal's own scrollback keeps working and the
/// run never blanks the whole screen (and there's nothing to restore that a tmux/VS-Code-style
/// terminal might mishandle). Returns false (terminal untouched) on any failure, so the caller
/// can fall back to plain lines.
fn enter_tui() -> bool {
  if enable_raw_mode().is_err() {
    return false;
  }
  let mut out = stderr();
  if queue!(out, EnableMouseCapture, cursor::Hide).and_then(|_| out.flush()).is_err() {
    let _ = disable_raw_mode();
    return false;
  }
  // Ask the terminal to disambiguate Ctrl+<digit> (and friends) via the keyboard-enhancement
  // protocol, so Ctrl+2..Ctrl+9 arrive as the digit + Ctrl instead of legacy control bytes
  // (Ctrl+2 = NUL, Ctrl+3 = ESC, …) that can't be told apart — that's why, without this, only
  // Ctrl+1 (which is a plain `1`) used to work. Terminals without the protocol simply ignore it,
  // and the plain digit still toggles there.
  if supports_keyboard_enhancement().unwrap_or(false)
    && queue!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))
      .and_then(|_| out.flush())
      .is_ok()
  {
    ENHANCED.store(true, Ordering::SeqCst);
  }
  TUI_ACTIVE.store(true, Ordering::SeqCst);
  true
}

/// Restore the terminal before running the previous panic hook, so a panic mid-run doesn't leave
/// the user in raw mode with mouse reporting on.
fn install_panic_hook() {
  static HOOKED: AtomicBool = AtomicBool::new(false);
  if HOOKED.swap(true, Ordering::SeqCst) {
    return;
  }
  let prev = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    restore_terminal();
    prev(info);
  }));
}

// --- the render + event loop --------------------------------------------------------------

fn render_loop(
  model: Arc<Mutex<Model>>, starts: Arc<Mutex<Vec<Option<Instant>>>>, stop: Arc<AtomicBool>, top_out: Arc<AtomicUsize>,
) {
  // The board is drawn inline, starting just below whatever was printed before the run (the
  // preflight + auth lines). Capture that row once; the board then floats UP only if it would
  // run past the bottom of the screen.
  let anchor = cursor::position().map(|(_, r)| r).unwrap_or(0);
  let mut frame: u64 = 0;
  let mut last_rows: Vec<Row> = Vec::new();
  let mut board_top = anchor; // where the board was drawn last frame (for hit-testing + clearing)
  while !stop.load(Ordering::SeqCst) {
    // 1. Handle input that arrived in the last tick (drain, so a flurry of wheel events is snappy).
    if poll(TICK).unwrap_or(false) {
      while let Ok(ev) = read() {
        if handle_event(ev, &model, &last_rows, board_top) {
          return; // a Ctrl-C abort already restored the terminal and exited the run
        }
        if !poll(Duration::from_millis(0)).unwrap_or(false) {
          break;
        }
      }
    }
    // 2. Tick the clocks of running procs, then redraw the board in place.
    tick_clocks(&model, &starts);
    let (rows, top) = draw(&model, frame, anchor, board_top);
    last_rows = rows;
    board_top = top;
    top_out.store(top as usize, Ordering::SeqCst);
    frame = frame.wrapping_add(1);
  }
}

/// Refresh each running proc's elapsed time from its start instant (finished procs are frozen).
fn tick_clocks(model: &Arc<Mutex<Model>>, starts: &Arc<Mutex<Vec<Option<Instant>>>>) {
  let starts = starts.lock().unwrap();
  let mut m = model.lock().unwrap();
  for (i, p) in m.procs.iter_mut().enumerate() {
    if p.status == Status::Running {
      if let Some(Some(start)) = starts.get(i) {
        p.elapsed = start.elapsed().as_secs_f64();
      }
    }
  }
}

/// Translate one input event into a model edit. Returns true if the run must abort now (Ctrl-C).
/// `board_top` is the screen row the board's first line was drawn at last frame.
fn handle_event(ev: Event, model: &Arc<Mutex<Model>>, last_rows: &[Row], board_top: u16) -> bool {
  let (w, h) = terminal::size().unwrap_or((80, 24));
  let width = w as usize;
  let page = (h as usize).saturating_sub(1).max(1); // a "page" is the visible board height
  match ev {
    Event::Mouse(me) => match me.kind {
      MouseEventKind::Down(MouseButton::Left) => {
        // Map the click to the row drawn there last frame, and toggle its proc (if it's a header).
        if me.row >= board_top {
          let idx = (me.row - board_top) as usize;
          if let Some(Some(p)) = last_rows.get(idx).map(|r| r.proc) {
            model.lock().unwrap().toggle(p);
          }
        }
      }
      MouseEventKind::ScrollUp => model.lock().unwrap().scroll_by(-3, width, page),
      MouseEventKind::ScrollDown => model.lock().unwrap().scroll_by(3, width, page),
      _ => {}
    },
    Event::Key(ke) if ke.kind != KeyEventKind::Release => {
      let ctrl = ke.modifiers.contains(KeyModifiers::CONTROL);
      match ke.code {
        KeyCode::Char('c') if ctrl => {
          // Raw mode swallows SIGINT, so Ctrl-C arrives as a key: restore, kill children, exit.
          restore_terminal();
          terminate_all();
          std::process::exit(130);
        }
        KeyCode::Up => model.lock().unwrap().scroll_by(-1, width, page),
        KeyCode::Down => model.lock().unwrap().scroll_by(1, width, page),
        KeyCode::PageUp => model.lock().unwrap().scroll_by(-(page as isize), width, page),
        KeyCode::PageDown => model.lock().unwrap().scroll_by(page as isize, width, page),
        KeyCode::Home => model.lock().unwrap().scroll_to_top(),
        KeyCode::End => model.lock().unwrap().scroll_to_bottom(),
        // Toggle a proc by its shortcut label ([0]..[9], then [A]..[Z]).
        // With keyboard-enhancement on, Ctrl+digit and Ctrl+letter arrive as the char + Ctrl;
        // the modifier is ignored, so a plain digit or letter toggles too.
        KeyCode::Char(d) if d.is_ascii_digit() || d.is_ascii_alphabetic() => {
          if let Some(idx) = super::live::proc_index_from_key(d) {
            let mut m = model.lock().unwrap();
            if idx < m.procs.len() {
              m.toggle(idx);
            }
          }
        }
        KeyCode::Char('e') => model.lock().unwrap().set_all_expanded(true),
        KeyCode::Char('c') => model.lock().unwrap().set_all_expanded(false),
        _ => {}
      }
    }
    _ => {} // resize and the rest just trigger a normal redraw next tick
  }
  false
}

/// Redraw the board inline and return the rows drawn plus the screen row they started at. The
/// board is capped to the screen height (rows beyond it scroll within the board, not the screen),
/// anchored just below the pre-run output, and floated up only if it would overrun the bottom.
fn draw(model: &Arc<Mutex<Model>>, frame: u64, anchor: u16, prev_top: u16) -> (Vec<Row>, u16) {
  let (w, h) = terminal::size().unwrap_or((80, 24));
  let (width, screen_h) = (w as usize, h as usize);
  let max_h = screen_h.saturating_sub(1).max(1); // never the full screen — leave the bottom row
  let rows = {
    let m = model.lock().unwrap();
    let height = m.total_rows(width).min(max_h).max(1);
    m.view(width, height, frame).0
  };
  let board_top = (anchor as usize).min(screen_h.saturating_sub(rows.len())) as u16;

  let mut out = stderr().lock();
  // Wipe whatever the board occupied last frame (from the higher of the two tops, to catch a
  // board that shrank), then paint each row at its absolute position (no newlines → no scroll).
  let _ = queue!(out, cursor::MoveTo(0, board_top.min(prev_top)), Clear(ClearType::FromCursorDown));
  for (r, row) in rows.iter().enumerate() {
    let _ = queue!(out, cursor::MoveTo(0, board_top + r as u16), Print(render_row(row)));
  }
  let _ = out.flush();
  (rows, board_top)
}

/// Style one model row into a printable string (segments coloured per [`Sty`]).
fn render_row(row: &Row) -> String {
  row.segs.iter().map(|s| sty(s.sty).apply_to(&s.text).to_string()).collect()
}

fn sty(s: Sty) -> Style {
  match s {
    Sty::Plain => Style::new(),
    Sty::Dim => Style::new().dim(),
    Sty::Bold => Style::new().bold(),
    Sty::Cyan => Style::new().cyan(),
    Sty::Green => Style::new().green().bold(),
    Sty::Orange => Style::new().yellow().bold(),
    Sty::Red => Style::new().red().bold(),
  }
}

// --- the persistent summary printed after the run -----------------------------------------

/// One `✓ label  elapsed  detail` (or ✗) line, matching the old board's finished line. Used both
/// for the post-run summary (attended) and for the live plain-line path (off-TTY).
/// A proc still `Queued` at summary time never ran (the run aborted first, e.g. on a failed
/// image build) — it renders as the board's dim `·` with "not started", never as a ✓.
fn summary_line(label: &str, status: Status, elapsed: f64, detail: Option<&str>) -> String {
  if status == Status::Queued {
    return format!("{} {}  {}", style("·").dim(), style(label).bold(), style("not started").dim());
  }
  if status == Status::Skipped {
    let mut line = format!("{} {}", style("⊘").dim(), style(label).bold());
    if let Some(d) = detail.filter(|d| !d.is_empty()) {
      line.push_str(&format!("  {}", style(d).dim()));
    }
    return line;
  }
  let (glyph, ok) = match status {
    Status::Fail => (style("✗").red().bold(), false),
    Status::Graceful => (style("!").yellow().bold(), true),
    _ => (style("✓").green().bold(), true),
  };
  let mut line = format!("{glyph} {}  {}", style(label).bold(), style(format_elapsed(elapsed)).dim());
  if let Some(d) = detail.filter(|d| !d.is_empty()) {
    let d = if ok { style(d).dim() } else { style(d).red() };
    line.push_str(&format!("  {d}"));
  }
  line
}

/// The whole run's summary: one ✓/✗ line per proc, in declared order.
fn summary_lines(model: &Model) -> Vec<String> {
  model.procs.iter().map(|p| summary_line(&p.label, p.status, p.elapsed, p.detail.as_deref())).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn summary_line_is_check_or_cross_with_detail() {
    let ok = console::strip_ansi_codes(&summary_line("add", Status::Ok, 5.0, Some("2 + 3 = 5"))).into_owned();
    assert_eq!(ok, "✓ add  5s  2 + 3 = 5");
    let bad = console::strip_ansi_codes(&summary_line("multiply", Status::Fail, 0.0, Some("X required"))).into_owned();
    assert_eq!(bad, "✗ multiply  0.0s  X required");
    let bare = console::strip_ansi_codes(&summary_line("build", Status::Ok, 4.0, None)).into_owned();
    assert_eq!(bare, "✓ build  4s");
    let skipped = console::strip_ansi_codes(&summary_line(
      "claude: review",
      Status::Skipped,
      0.0,
      Some("skipped — its when: gate is false"),
    ))
    .into_owned();
    assert_eq!(skipped, "⊘ claude: review  skipped — its when: gate is false");
    let queued = console::strip_ansi_codes(&summary_line("claude: add", Status::Queued, 0.0, None)).into_owned();
    assert_eq!(queued, "· claude: add  not started");
  }

  #[test]
  fn summary_lists_every_proc_in_order() {
    let mut m = Model::new();
    let b = m.add("build");
    m.set_status(b, Status::Ok);
    m.set_elapsed(b, 4.0);
    let s = m.add("add");
    m.set_status(s, Status::Fail);
    m.set_detail(s, Some("boom".into()));
    let lines: Vec<String> = summary_lines(&m).iter().map(|l| console::strip_ansi_codes(l).into_owned()).collect();
    assert_eq!(lines, vec!["✓ build  4s".to_string(), "✗ add  0.0s  boom".to_string()]);
  }

  #[cfg(unix)]
  #[test]
  fn emit_off_tty_without_sink_echoes_but_does_not_record_model_lines() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("skill", false);
    p.start();
    p.emit("git fsck --no-progress…");
    let m = ui.model.lock().unwrap();
    assert_eq!(m.procs[0].lines.len(), 0);
  }

  #[cfg(unix)]
  #[test]
  fn emit_off_tty_with_sink_records_lines_without_echo_for_non_tail() {
    struct PinDaemonPort {
      previous: Option<String>,
    }
    impl PinDaemonPort {
      fn ephemeral() -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let previous = std::env::var("SCSH_DAEMON_PORT").ok();
        std::env::set_var("SCSH_DAEMON_PORT", port.to_string());
        Self { previous }
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
    let _pin = PinDaemonPort::ephemeral();
    let client = std::sync::Arc::new(crate::daemon::Client::new("abcdef".into()));
    let ui = LiveUi::new(false, Some(client.clone()));
    let p = ui.proc("skill", false);
    p.start();
    p.emit("daemon line");
    let m = ui.model.lock().unwrap();
    assert_eq!(m.procs[0].lines.len(), 1);
    assert_eq!(m.procs[0].lines[0].text, "daemon line");
    client.flush();
  }

  // The off-TTY Proc path runs real (tiny) subprocesses, pumping their output into the model as
  // timestamped lines — the same code the attended TUI uses, minus the terminal.
  #[cfg(unix)]
  #[test]
  fn proc_pumps_timestamped_lines_into_the_model() {
    let ui = LiveUi::new(false, None); // off-TTY: no terminal take-over
    let p = ui.proc("seq", false);
    p.start();
    let (ok, last) = p.run("seq", &["3".to_string()]).unwrap();
    assert!(ok);
    assert_eq!(last.as_deref(), Some("3"));
    p.finish_ok(Some("done"));
    let m = ui.model.lock().unwrap();
    let texts: Vec<&str> = m.procs[0].lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["1", "2", "3"]);
    // Every captured line carries a non-negative relative timestamp.
    assert!(m.procs[0].lines.iter().all(|l| l.at >= 0.0));
    assert_eq!(m.procs[0].status, Status::Ok);
  }

  #[test]
  fn restarting_one_logical_proc_preserves_its_original_clock() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("schema repair", false);
    p.start();
    let first = p.starts.lock().unwrap()[p.index()];
    p.clone().start();
    let second = p.starts.lock().unwrap()[p.index()];
    assert_eq!(second, first);
  }

  #[cfg(unix)]
  #[test]
  fn proc_run_watched_kills_an_overrunning_child() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("sleep", false);
    p.start();
    let (ok, killed, _) = p.run_watched("sleep", &["5".to_string()], Some(Duration::from_millis(150)), None).unwrap();
    assert_eq!(killed, Killed::Timeout);
    assert!(!ok, "the 5s sleep must be killed by the 150ms timeout");
  }

  #[test]
  fn proc_run_watched_kills_a_child_whose_screen_never_moves() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("frozen", false);
    p.start();
    // The watched file never appears, so the watchdog fires long before the 5s sleep ends.
    let watch = ActivityWatch {
      file: std::env::temp_dir().join(format!("scsh-watch-never-{}", std::process::id())),
      limit: Duration::from_millis(200),
    };
    let (ok, killed, _) = p.run_watched("sleep", &["5".to_string()], None, Some(&watch)).unwrap();
    assert_eq!(killed, Killed::Inactive);
    assert!(!ok);
  }

  #[test]
  fn proc_run_watched_lets_an_active_screen_run_to_completion() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("active", false);
    p.start();
    let file = std::env::temp_dir().join(format!("scsh-watch-grow-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    // The child appends a NOVEL line every 100ms — well inside the 600ms budget — then exits 0.
    // (Letters, not a counter: digits are normalized away, so `line 1`/`line 2` would count
    // as the same frame — that is the spinner-thrash case the watchdog now kills.)
    let script = format!("for w in a b c d e f g h; do echo tok-$w >> {}; sleep 0.1; done", file.display());
    let watch = ActivityWatch { file: file.clone(), limit: Duration::from_millis(600) };
    let (ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, Some(&watch)).unwrap();
    let _ = std::fs::remove_file(&file);
    assert_eq!(killed, Killed::No);
    assert!(ok, "an active child must not be killed by the watchdog");
  }

  #[test]
  fn proc_run_watched_kills_a_spinner_that_repeats_the_same_frames() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("spinner", false);
    p.start();
    let file = std::env::temp_dir().join(format!("scsh-watch-spin-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    // The file GROWS constantly, but every event is the same frame up to its timestamp and a
    // ticking seconds counter — a wedged TUI's spinner. The old size-based watchdog never
    // fired on this (observed live: a 30-minute grok hang with a growing cast).
    let script = format!(
      r#"i=0; while true; do echo "[$i.5, \"o\", \"thinking ${{i}}s\"]" >> {}; i=$((i+1)); sleep 0.05; done"#,
      file.display()
    );
    let watch = ActivityWatch { file: file.clone(), limit: Duration::from_millis(500) };
    let (ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, Some(&watch)).unwrap();
    let _ = std::fs::remove_file(&file);
    assert_eq!(killed, Killed::Inactive, "repeating frames are not activity");
    assert!(!ok);
  }

  #[test]
  fn novelty_normalization_erases_timestamps_and_digits_only() {
    // Same asciicast frame at different times / tick counts → one hash (a spinner).
    let a = NoveltyWatch::normalized_hash(br#"[1.02, "o", "thinking 3s"]"#);
    let b = NoveltyWatch::normalized_hash(br#"[87.9, "o", "thinking 41s"]"#);
    assert_eq!(a, b);
    // Genuinely different content → different hashes (streamed tokens are progress).
    let c = NoveltyWatch::normalized_hash(br#"[88.0, "o", "wrote do-while.txt"]"#);
    assert_ne!(a, c);
    // Non-event lines (the asciicast header) hash on their full digit-stripped content.
    let h1 = NoveltyWatch::normalized_hash(br#"{"version": 2, "width": 200}"#);
    let h2 = NoveltyWatch::normalized_hash(br#"{"version": 2, "width": 100}"#);
    let h3 = NoveltyWatch::normalized_hash(br#"{"version": 2, "height": 50}"#);
    assert_eq!(h1, h2, "digits are erased everywhere");
    assert_ne!(h1, h3);
  }
}
