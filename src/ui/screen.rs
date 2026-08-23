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
use super::signals::{
  isolate_child, orphaned_child_group, register_child, terminate_all, terminate_child_group, unregister_child,
};
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
pub struct ActivityWatch<'a> {
  /// Polled (`~100ms`) for new content; a file that never appears counts as never active.
  pub file: std::path::PathBuf,
  /// Silence budget: kill the child when `file` has shown nothing novel for this long.
  pub limit: Duration,
  /// Tighter silence budgets for the launch phase, when a wedged run has burned nothing yet
  /// and an immediate relaunch is cheaper than waiting out the full inactivity budget.
  pub startup: Option<StartupStall>,
  /// Read the screen for a usage-limit banner and hold the clocks while one is up. `None`
  /// leaves every watchdog exactly as it was.
  pub limit_wait: Option<LimitWait<'a>>,
}

/// What to do when the harness stops because the account hit a usage limit.
///
/// A limited claude session is not a failed one: it is a session that will pick the task back up
/// when the limit resets, keeping everything it has worked out — which no scsh retry can, since
/// every attempt is a fresh clone, container, and conversation. But it looks identical to a
/// hang, so without this the inactivity watchdog kills it half an hour in and the retry backoff
/// gives up long before a reset hours away.
///
/// So while a limit banner is on screen, the inactivity and wall-clock clocks are FROZEN — not
/// extended, frozen — and they start again the moment the screen moves. A run that resumes has
/// spent nothing; a run that genuinely wedges behind the banner is still caught by [`max`] and,
/// after it, by the ordinary budgets it never got to spend.
///
/// [`max`]: LimitWait::max
pub struct LimitWait<'a> {
  /// Ceiling on one parked stretch, however long the provider says the reset is. Nothing here
  /// can prove the screen still means what it said, so the wait is always bounded.
  pub max: Duration,
  /// Where to drop tmux key names for the container's recorder to forward
  /// ([`crate::runtime::RUN_KEYS_DIR`]). Two claude screens stop dead waiting for a keypress
  /// that, unattended, never comes: the limit dialog (whose default choice IS the wait) and the
  /// post-reset "press enter to continue". One byte here restarts both.
  pub keys: std::path::PathBuf,
  /// The reset instant the provider reported, when it has. Asked while parked, because the
  /// number arrives with the run's own status-line capture rather than up front. Used only to
  /// shorten the wait — it never extends it past [`Self::max`].
  pub resets_at: Box<dyn Fn() -> Option<u64> + Send + Sync + 'a>,
  /// Called whenever the parked state changes, so the caller can say so on the board and in
  /// the session browser. Not called for the polls in between.
  pub on_state: Box<dyn Fn(Option<crate::limitwait::LimitState>, Option<u64>) + Send + Sync + 'a>,
}

/// Launch-phase stall policy for [`ActivityWatch`]. A harness that wedges before doing any
/// work — a container that never boots, a TUI that hangs on its first paint, a login screen
/// waiting for nobody — looks exactly like a slow start until the full inactivity budget
/// (minutes) burns down. But the launch phase has its own, much tighter honesty contract:
/// a healthy harness shows SOMETHING within seconds, and keeps showing new frames while it
/// boots. So during startup the silence budgets shrink, and a kill here is reported as
/// [`Killed::StartupStalled`] so the caller can force an immediate restart instead of a
/// backed-off retry — nothing of value was lost.
pub struct StartupStall {
  /// Kill when the watched file has shown nothing novel AT ALL this long after spawn.
  pub silence: Duration,
  /// After first output: kill when novelty stops for this long during the startup window.
  pub stall: Duration,
  /// How long after spawn the `stall` rule stays armed. A stall that BEGINS (last novel
  /// frame) inside this window is a startup stall; silence that begins later is the normal
  /// inactivity watchdog's business.
  pub window: Duration,
}

/// No terminal output at all for this long after spawn = startup stall.
pub const STARTUP_SILENCE_SECS: u64 = 60;
/// Output stopped for this long straight while the startup window was still open = startup stall.
pub const STARTUP_STALL_SECS: u64 = 30;
/// The startup window: how long after spawn the stall rule stays armed.
pub const STARTUP_WINDOW_SECS: u64 = 90;

impl StartupStall {
  /// The production policy: 60s of initial silence, or a 30s straight stall beginning within
  /// the first 90s, forces a restart.
  pub fn defaults() -> StartupStall {
    StartupStall {
      silence: Duration::from_secs(STARTUP_SILENCE_SECS),
      stall: Duration::from_secs(STARTUP_STALL_SECS),
      window: Duration::from_secs(STARTUP_WINDOW_SECS),
    }
  }

  /// [`defaults`] with each threshold independently perturbed by up to ±20%, driven by a
  /// fresh per-spawn `entropy` seed (wall-clock nanos mixed with the run's container name).
  ///
  /// A whole fleet launched at once against one slow provider would otherwise trip the fixed
  /// startup-silence threshold at the very same instant and force-restart in lockstep — the
  /// thundering-herd the retry backoff already jitters away one layer down. Spreading the
  /// thresholds fans those restarts out over a few seconds. Unlike the backoff's salt-keyed
  /// jitter this is deliberately NOT reproducible across relaunches: each spawn draws fresh.
  pub fn jittered(entropy: u64) -> StartupStall {
    let base = StartupStall::defaults();
    StartupStall {
      silence: jitter_20pct(base.silence, entropy),
      stall: jitter_20pct(base.stall, entropy.rotate_left(21)),
      window: jitter_20pct(base.window, entropy.rotate_left(42)),
    }
  }
}

/// Scale `d` by a random factor in `[0.8, 1.2]` selected from `seed`. The ±20% keeps the
/// watchdog's character intact (a 60s silence stays roughly a minute) while breaking up
/// synchronized restarts. Milliseconds granularity so short test durations still spread.
fn jitter_20pct(d: Duration, seed: u64) -> Duration {
  let mixed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(31);
  let permille = 800 + (mixed % 401); // 800..=1200 → ×0.8 .. ×1.2
  let scaled_ms = (d.as_millis() as u64).saturating_mul(permille) / 1000;
  Duration::from_millis(scaled_ms)
}

/// Stop waiting the moment the task crosses its own declared finish line, instead of waiting
/// for a harness process that may never exit.
///
/// A terminal harness and a completed task are not the same event. Some agent CLIs finish the
/// requested work, write their declared result, and then wedge with the TUI still open — the
/// run then burns its entire inactivity budget (an hour, for a step with a widened window) and
/// dies red, even though the work was done in the first few minutes.
///
/// The result file is bind-mounted, so the host can see it being written. Presence alone is not
/// enough — a half-written file is present too — so completion is inferred from QUIESCENCE: the
/// file's `(mtime, len)` stamp must stop changing for `quiet_for`, and only then is `confirm`
/// asked whether the content is a usable result. A writer still working keeps resetting the
/// clock.
///
/// Where a child runs: its working directory and the variables added to the inherited
/// environment. Used by workflow host steps, which execute in the caller's own repository and
/// receive their declared `inputs:` as environment variables.
pub struct ExecPlace<'a> {
  pub cwd: &'a std::path::Path,
  pub env: &'a [(String, String)],
}

/// Maximum output retained and forwarded for a child whose result is a bounded output tail.
#[derive(Clone, Copy)]
pub struct OutputLimit {
  pub lines: usize,
  pub bytes: usize,
}

/// Read one logical line while retaining only its newest `max_bytes` bytes.
fn read_line_tail<R: BufRead>(reader: &mut R, max_bytes: usize) -> std::io::Result<Option<(String, bool)>> {
  let mut kept = Vec::with_capacity(max_bytes.min(8192));
  let mut read_any = false;
  let mut trimmed = false;
  loop {
    let available = reader.fill_buf()?;
    if available.is_empty() {
      return Ok(read_any.then(|| (String::from_utf8_lossy(&kept).into_owned(), trimmed)));
    }
    let end = available.iter().position(|byte| *byte == b'\n').map(|i| i + 1).unwrap_or(available.len());
    let chunk = &available[..end];
    read_any = true;
    if chunk.len() >= max_bytes {
      trimmed |= !kept.is_empty() || chunk.len() > max_bytes;
      kept.clear();
      kept.extend_from_slice(&chunk[chunk.len() - max_bytes..]);
    } else {
      let overflow = kept.len().saturating_add(chunk.len()).saturating_sub(max_bytes);
      if overflow > 0 {
        kept.drain(..overflow);
        trimmed = true;
      }
      kept.extend_from_slice(chunk);
    }
    let complete = chunk.last() == Some(&b'\n');
    reader.consume(end);
    if complete {
      return Ok(Some((String::from_utf8_lossy(&kept).into_owned(), trimmed)));
    }
  }
}

/// This decides only WHEN TO STOP WAITING, never whether the run succeeded. The caller's normal
/// collection path — copy out, validate against the workflow schema, bounded correction retry —
/// stays authoritative, so an early stop can never launder a bad result into a pass.
pub struct DoneWatch {
  /// The declared result file, watched for the writer going quiet.
  pub file: std::path::PathBuf,
  /// How long `file`'s stamp must hold still before it counts as finished.
  pub quiet_for: Duration,
  /// Asked only once the file has gone quiet: is this actually a usable result? Keeps this
  /// module free of any knowledge about JSON, schemas, or git.
  pub confirm: Box<dyn Fn() -> bool + Send + Sync>,
}

/// The last `(mtime, len)` stamp seen for a [`DoneWatch`] file and how long it has held.
struct QuiescenceWatch {
  file: std::path::PathBuf,
  /// `None` until the file first appears; reset whenever it vanishes.
  stamp: Option<(std::time::SystemTime, u64)>,
  /// HOST-side instant at which `stamp` last changed. Deliberately not derived from the file's
  /// own mtime: that timestamp comes from the container's clock, and any skew against the host
  /// would make a "written more than N seconds ago" test fire early or never fire at all.
  /// Comparing stamps only for EQUALITY and timing the gap locally is immune to that.
  since: std::time::Instant,
}

impl QuiescenceWatch {
  fn new(file: &std::path::Path) -> Self {
    QuiescenceWatch { file: file.to_path_buf(), stamp: None, since: std::time::Instant::now() }
  }

  /// `true` once the file exists and its stamp has been unchanged for `quiet_for`. Length is
  /// tracked alongside mtime because bind-mount mtime granularity can be coarse enough to hide
  /// a rewrite that lands within the same tick.
  fn poll(&mut self, quiet_for: Duration) -> bool {
    let Ok(meta) = std::fs::metadata(&self.file) else {
      // Not written yet (or removed mid-write): nothing has gone quiet.
      self.stamp = None;
      return false;
    };
    let stamp = (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len());
    if self.stamp != Some(stamp) {
      self.stamp = Some(stamp);
      self.since = std::time::Instant::now();
      return false;
    }
    self.since.elapsed() >= quiet_for
  }
}

/// How often the screen is re-read for a usage-limit banner. Rendering the tail costs a JSON
/// parse and an ANSI strip per event; a limit is a multi-hour event, so seconds of latency in
/// noticing one is free, and paying this on every 100ms poll would not be.
const LIMIT_SCAN_EVERY: Duration = Duration::from_secs(2);

/// Minimum gap between keypresses sent to a parked screen. A dialog that ignores the first
/// Enter is a screen scsh has misread; hammering it would type into whatever is really there.
const LIMIT_KEY_COOLDOWN: Duration = Duration::from_secs(30);

/// How still the screen must already be before a limit banner is allowed to freeze the clocks.
///
/// A genuinely parked session is still for hours, so this costs nothing real — and it is what
/// keeps a misread screen from being expensive: output that merely mentions a limit comes from a
/// run that is still producing frames, and never parks.
const LIMIT_PARK_QUIET: Duration = Duration::from_secs(30);

/// [`LIMIT_PARK_QUIET`], but never more than half the silence budget it is guarding. Expressed
/// against `limit` rather than absolutely because that is what it actually means: "still for a
/// meaningful fraction of the time this run is allowed to be silent". A fixed 30s would be
/// unreachable under any budget shorter than that, which would disable the freeze entirely
/// instead of gating it.
fn park_quiet(limit: Duration) -> Duration {
  LIMIT_PARK_QUIET.min(limit / 2)
}

/// Slack allowed past the provider's own reset instant before a still-parked run is given up on.
/// Covers clock skew, a status line that refreshed a little stale, and the seconds claude takes
/// to notice its own reset — but not a screen that is simply stuck.
pub(crate) const LIMIT_RESET_GRACE: Duration = Duration::from_secs(300);

/// Bookkeeping for one [`LimitWait`]: what the screen last said, how long the run has been
/// parked on a limit, and how much of that the watchdogs must not count.
///
/// The frozen time is tracked in two pieces because the two clocks it feeds are different
/// shapes. The wall-clock budget is cumulative, so every closed stretch stays subtracted from
/// it forever. The inactivity budget is a gap since the last novel frame, and a resumed run
/// produces one immediately — so a closed stretch is pushed into `last_activity` once and then
/// forgotten, and only the stretch still open is subtracted.
struct LimitWatch {
  /// Next time the screen is worth rendering again.
  next_scan: std::time::Instant,
  /// What [`crate::limitwait::detect`] last read, and what was reported upward.
  state: Option<crate::limitwait::LimitState>,
  /// Start of the stretch currently being frozen; `None` while the run is making progress.
  parked_since: Option<std::time::Instant>,
  /// Frozen time from stretches that have already ended.
  frozen_closed: Duration,
  /// The provider's reset instant (unix epoch), once a status-line capture has carried one.
  resets_at: Option<u64>,
  /// When a key was last handed to the container, for [`LIMIT_KEY_COOLDOWN`].
  last_key: Option<std::time::Instant>,
  /// Monotonic record id so the same key can be delivered again after the cooldown.
  key_sequence: u64,
}

impl LimitWatch {
  fn new() -> Self {
    LimitWatch {
      next_scan: std::time::Instant::now(),
      state: None,
      parked_since: None,
      frozen_closed: Duration::ZERO,
      resets_at: None,
      last_key: None,
      key_sequence: 0,
    }
  }

  /// The stretch currently being frozen (zero when the run is moving).
  fn open_stretch(&self) -> Duration {
    self.parked_since.map(|t| t.elapsed()).unwrap_or_default()
  }

  /// Total time frozen so far — subtracted from the cumulative wall-clock budget.
  fn frozen(&self) -> Duration {
    self.frozen_closed + self.open_stretch()
  }

  /// Whether this parked stretch has reached either the provider's absolute reset plus grace
  /// or the policy ceiling. With no reported reset the ceiling is all there is.
  fn expired(&self, policy: &LimitWait<'_>, now_epoch: u64) -> bool {
    limit_wait_expired(self.open_stretch(), policy.max, self.resets_at, now_epoch)
  }

  /// Hand one tmux key name to the container's recorder, at most once per cooldown. Publication
  /// is atomic, and its directory is mounted read-only into the container, so neither a partial
  /// read nor a container-created symlink can redirect the host write. Failure is silent and
  /// harmless: the wait simply continues until its deadline.
  fn send_key(&mut self, policy: &LimitWait<'_>, key: &str) {
    if self.last_key.is_some_and(|t| t.elapsed() < LIMIT_KEY_COOLDOWN) {
      return;
    }
    self.last_key = Some(std::time::Instant::now());
    self.key_sequence = self.key_sequence.saturating_add(1);
    let _ = publish_key(&policy.keys, self.key_sequence, key);
  }
}

/// Write-close-rename publication replaces the destination entry itself, never anything a
/// pre-existing symlink points at. The channel parent is host-only; the container sees a
/// read-only bind mount of it.
fn publish_key(path: &std::path::Path, sequence: u64, key: &str) -> std::io::Result<()> {
  let parent = path.parent().ok_or_else(|| std::io::Error::other("key channel has no parent directory"))?;
  let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("key");
  let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
  let result = (|| {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
    writeln!(file, "{sequence} {key}")?;
    drop(file);
    std::fs::rename(&temp, path)
  })();
  if result.is_err() {
    let _ = std::fs::remove_file(&temp);
  }
  result
}

fn limit_wait_expired(open_stretch: Duration, max: Duration, resets_at: Option<u64>, now_epoch: u64) -> bool {
  open_stretch >= max || resets_at.is_some_and(|at| now_epoch >= at.saturating_add(LIMIT_RESET_GRACE.as_secs()))
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
  /// Complete raw cast lines since the last limit scan, kept only when something asks to read
  /// the screen. Held undecoded because rendering on every 100ms poll is needlessly costly.
  recent: Option<Vec<u8>>,
}

/// Spinner cycles are tiny; this only needs to exceed the largest realistic set of distinct
/// idle frames. Evicting truly old frames errs toward counting them as novel again — the
/// safe direction (it can only delay a kill, never cause a false one).
const NOVELTY_MEMORY: usize = 4096;
/// Per-poll read cap so one poll never stalls the supervision loop on a runaway file.
const NOVELTY_READ_CAP: u64 = 1 << 20;
/// Raw cast bytes kept for one limit scan. Generous next to the few hundred
/// characters any banner occupies: a redrawn TUI frame carries far more escape sequence than
/// text, so this renders down to a much smaller screenful.
const NOVELTY_TAIL_BYTES: usize = 64 * 1024;

impl NoveltyWatch {
  fn new(file: &std::path::Path, keep_tail: bool) -> Self {
    NoveltyWatch {
      file: file.to_path_buf(),
      offset: 0,
      carry: Vec::new(),
      seen: std::collections::HashSet::new(),
      order: std::collections::VecDeque::new(),
      recent: keep_tail.then(Vec::new),
    }
  }

  /// Render and consume complete cast lines gathered since the previous limit scan. `None`
  /// means the screen emitted no complete event, so the previous classification still stands.
  fn take_rendered_recent(&mut self) -> Option<String> {
    let recent = self.recent.as_mut()?;
    if recent.is_empty() {
      return None;
    }
    let bytes = std::mem::take(recent);
    Some(crate::ptyrec::cast_output_text(&String::from_utf8_lossy(&bytes)))
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
        let new = self.seen.insert(hash);
        if let Some(recent) = &mut self.recent {
          recent.extend_from_slice(&self.carry);
          recent.push(b'\n');
          if recent.len() > NOVELTY_TAIL_BYTES {
            recent.drain(..recent.len() - NOVELTY_TAIL_BYTES);
          }
        }
        self.carry.clear();
        if new {
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

/// Update a screen classification from scan-local cast events. A fresh limit phrase always wins.
/// Ordinary output clears informational `Resumed`, but cannot erase a confirmed stopped state:
/// tmux helper notices are terminal activity without evidence that Claude resumed.
fn recent_limit_state(
  novelty: &mut NoveltyWatch, previous: Option<crate::limitwait::LimitState>,
) -> Option<crate::limitwait::LimitState> {
  let Some(text) = novelty.take_rendered_recent() else { return previous };
  let detected = crate::limitwait::detect(&text);
  match (detected, previous) {
    (Some(state), _) => Some(state),
    (None, Some(state)) if state.is_stopped() => Some(state),
    (None, Some(crate::limitwait::LimitState::Resumed)) => None,
    (None, previous) => previous,
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
  /// The run stalled during its launch phase (see [`StartupStall`]): `silent` runs never
  /// produced a single novel frame, the rest went quiet mid-boot. Either way nothing of
  /// value was in flight, so the caller force-restarts immediately instead of backing off.
  StartupStalled { silent: bool },
  /// The harness was stopped by a usage limit and will not come back on its own — it said so,
  /// or it sat behind the banner past [`LimitWait::max`]. Distinct from every other kill: the
  /// run did not fail and retrying it now would only hit the same limit, so the caller waits
  /// for the reset instead of spending its backoff. `resets_at` is the provider's reset instant
  /// (unix epoch) when it reported one.
  LimitExhausted { resets_at: Option<u64> },
  /// Not a failure: the task's declared result file went quiet and passed [`DoneWatch::confirm`],
  /// so the harness was stopped deliberately rather than waited out. The caller still validates
  /// the result before calling the run a success.
  Done,
}

/// Stop a watchdog-killed child and everything it started.
///
/// Killing the direct child is not enough for anything that forks — and a workflow host step's
/// `sh -c "make gate-all"` is exactly that. Its descendants would survive, keep running on the
/// developer's machine, and keep the stdout/stderr pipes open, so [`drain_pumps`] below would
/// wait forever and a detected timeout would present as a hang. Signal the whole process group
/// (which [`isolate_child`] made this child the leader of), then the child itself — the latter
/// is also the only path off unix, where there are no groups.
fn kill_child_tree(child: &mut std::process::Child) {
  terminate_child_group(child.id());
  let _ = child.kill();
}

/// How long the output readers get to finish after a child was killed. A descendant that put
/// itself in a NEW session escapes even a group signal and can hold the pipes open indefinitely;
/// abandoning two blocked reader threads is strictly better than never returning.
const PUMP_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// The floor under a self-exited command's drain deadline. When the child is really gone its
/// pipes are already at EOF, so the readers join in microseconds and this costs nothing; it exists
/// only so a command finishing on the last tick of its budget is not misread as a timeout because
/// the joiner thread lost a scheduling race.
const PUMP_DRAIN_CLOSE_GRACE: Duration = Duration::from_millis(250);

/// How long the readers get to finish, given how the child ended and what is left of its budget.
///
/// A killed child gets [`PUMP_DRAIN_GRACE`]. One that exited on its own gets the remainder of its
/// wall-clock budget — the command is not over until its pipes close — but never less than
/// [`PUMP_DRAIN_CLOSE_GRACE`], so finishing on the last tick of a timeout cannot be misreported as
/// overrunning it. An untimed command waits as long as it takes (`None`).
fn drain_deadline(killed: Killed, timeout: Option<Duration>, elapsed: Duration) -> Option<Duration> {
  match killed {
    Killed::No => timeout.map(|limit| limit.saturating_sub(elapsed).max(PUMP_DRAIN_CLOSE_GRACE)),
    _ => Some(PUMP_DRAIN_GRACE),
  }
}

/// Join the output-reader threads. A command with no watchdog waits normally. Timed commands use
/// their remaining wall-clock budget, and killed commands use [`PUMP_DRAIN_GRACE`]. Abandoned
/// readers may still append a few late lines to a finished proc's buffer; a stale tail is a
/// cosmetic cost, a hung run is not.
fn drain_pumps(pumps: Vec<JoinHandle<()>>, deadline: Option<Duration>) -> bool {
  if deadline.is_none() {
    for p in pumps {
      let _ = p.join();
    }
    return true;
  }
  let (tx, rx) = std::sync::mpsc::channel::<()>();
  let joiner = thread::spawn(move || {
    for p in pumps {
      let _ = p.join();
    }
    let _ = tx.send(());
  });
  if rx.recv_timeout(deadline.unwrap()).is_err() {
    drop(joiner); // detach: the readers keep waiting on a pipe nobody will close
    false
  } else {
    true
  }
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
    let (status, _killed, last, _trimmed) = self.exec(program, args, None, None, None, None, None, None, None)?;
    Ok((status.success(), last))
  }

  /// Last `max` lines captured for this proc (stdout/stderr pump output).
  pub fn tail_lines(&self, max: usize) -> Vec<String> {
    self.model.lock().unwrap().tail_lines(self.i, max)
  }

  /// Like [`Proc::run`] but kills the child past the wall-clock `timeout` and/or when
  /// `watch` sees no screen activity for its limit (`None`s wait forever), and stops it early
  /// when `done` sees the task's result file finished. Returns `(success, why_killed, last_line)`.
  pub fn run_watched(
    &self, program: &str, args: &[String], timeout: Option<Duration>, watch: Option<&ActivityWatch<'_>>,
    done: Option<&DoneWatch>,
  ) -> std::io::Result<(bool, Killed, Option<String>)> {
    let (status, killed, last, _trimmed) = self.exec(program, args, None, timeout, watch, done, None, None, None)?;
    Ok((status.success(), killed, last))
  }

  /// Like [`Proc::run_watched`], but in an explicit working directory and environment, and
  /// reporting the child's exact **exit code**. For a workflow host step, whose non-zero exit
  /// is a result to hand downstream rather than a failure of the step itself. `cast` mirrors
  /// the already-captured stream into an asciicast without putting the command under a PTY.
  pub fn run_in(
    &self, program: &str, args: &[String], place: &ExecPlace, timeout: Option<Duration>, output_limit: OutputLimit,
    cast: Option<crate::ptyrec::CastSink>,
  ) -> std::io::Result<(Option<i32>, Killed, Option<String>, bool)> {
    let (status, killed, last, trimmed) =
      self.exec(program, args, None, timeout, None, None, Some(place), Some(output_limit), cast)?;
    Ok((status.code(), killed, last, trimmed))
  }

  /// Spawn `program args`, pump both output streams into the model as timestamped lines,
  /// optionally feed `stdin` then EOF, and optionally kill on `timeout`. The single core the
  /// public `run*` methods delegate to.
  ///
  /// A child killed by one of the watchdogs is taken down as a whole process TREE, and the
  /// reader threads are then drained under a deadline — see [`kill_child_tree`] and
  /// [`PUMP_DRAIN_GRACE`]. Both exist so "the watchdog fired" can never become "scsh hung".
  #[allow(clippy::too_many_arguments)]
  fn exec(
    &self, program: &str, args: &[String], stdin: Option<&[u8]>, timeout: Option<Duration>,
    watch: Option<&ActivityWatch<'_>>, done: Option<&DoneWatch>, place: Option<&ExecPlace>,
    output_limit: Option<OutputLimit>, cast: Option<crate::ptyrec::CastSink>,
  ) -> std::io::Result<(std::process::ExitStatus, Killed, Option<String>, bool)> {
    let started = self.start_instant();
    let mut command = Command::new(program);
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    if let Some(place) = place {
      command.current_dir(place.cwd);
      command.envs(place.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    isolate_child(&mut command);
    let mut child = command.spawn()?;
    let pid = child.id();
    register_child(pid);

    let last = Arc::new(Mutex::new(None::<String>));
    let output_trimmed = Arc::new(AtomicBool::new(false));
    let mut pumps: Vec<JoinHandle<()>> = Vec::new();
    if let Some(out) = child.stdout.take() {
      pumps.push(self.pump(out, started, Arc::clone(&last), output_limit, Arc::clone(&output_trimmed), cast.clone()));
    }
    if let Some(err) = child.stderr.take() {
      pumps.push(self.pump(err, started, Arc::clone(&last), output_limit, Arc::clone(&output_trimmed), cast.clone()));
    }
    // Feed stdin only after the pumps are draining output, so a large payload can't deadlock
    // against a full output pipe. Dropping the handle signals EOF.
    if let Some(bytes) = stdin {
      if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(bytes);
      }
    }

    let mut killed = Killed::No;
    let status = if timeout.is_none() && watch.is_none() && done.is_none() {
      child.wait()?
    } else {
      // The activity clock starts now: a watched file that never appears (or never shows a
      // novel line) still trips the watchdog once the limit elapses.
      let keep_tail = watch.is_some_and(|w| w.limit_wait.is_some());
      let mut novelty = watch.map(|w| NoveltyWatch::new(&w.file, keep_tail));
      let mut quiescence = done.map(|d| QuiescenceWatch::new(&d.file));
      let mut limits = watch.and_then(|w| w.limit_wait.as_ref()).map(|_| LimitWatch::new());
      let watch_started = std::time::Instant::now();
      let mut last_activity = watch_started;
      let mut saw_novelty = false;
      loop {
        if let Some(s) = child.try_wait()? {
          break s;
        }
        // Read the cast FIRST — the novelty hashes and, when a limit wait is armed, the
        // rendered screen come from the same bytes, and the limit scan below is worthless
        // against a tail nothing has read into yet.
        let novel_activity = novelty.as_mut().is_some_and(NoveltyWatch::poll);
        if novel_activity {
          saw_novelty = true;
          last_activity = std::time::Instant::now();
        }
        // Then read the screen, before any watchdog does its arithmetic: a run parked on a
        // usage limit has not stalled and has not overrun anything, and every clock below has
        // to know that before it decides. See [`LimitWait`].
        if let (Some(w), Some(policy), Some(lw), Some(nov)) =
          (watch, watch.and_then(|w| w.limit_wait.as_ref()), limits.as_mut(), novelty.as_mut())
        {
          if std::time::Instant::now() >= lw.next_scan {
            lw.next_scan = std::time::Instant::now() + LIMIT_SCAN_EVERY;
            let seen = recent_limit_state(nov, lw.state);
            // The reset instant arrives with the run's own status-line capture, which is only
            // written once the session has talked to the provider — so it is asked for while
            // parked, not up front, and re-asked until it answers.
            if seen.is_some_and(|s| s.is_parked()) && lw.resets_at.is_none() {
              lw.resets_at = (policy.resets_at)();
            }
            if seen != lw.state {
              lw.state = seen;
              (policy.on_state)(seen, lw.resets_at);
              if seen.is_none() || seen == Some(crate::limitwait::LimitState::Resumed) {
                lw.resets_at = None;
              }
            }
          }
          // Nothing here freezes a clock over time in which something happened. Reading a
          // banner off a screen is a guess, and the cost of a wrong one — hours of watchdog
          // immunity for a genuinely wedged run — is far worse than the cost of ignoring a
          // right one for a few extra seconds. So the freeze is gated on the screen actually
          // being STILL: a run that merely printed the words (a skill summarizing quota, an
          // agent quoting an error) keeps producing novel frames and is never parked, and one
          // that starts producing them again is unparked whatever the banner still says.
          //
          // Safe in both directions, which is why the guard is cheap: a still screen is
          // frozen here, and a moving one keeps resetting the inactivity clock on its own.
          let quiet = last_activity.elapsed() >= park_quiet(w.limit);
          if quiet {
            if let Some(key) = lw.state.and_then(crate::limitwait::LimitState::wants_key) {
              lw.send_key(policy, key);
            }
          }
          let moved_since_park = lw.parked_since.is_some_and(|since| last_activity > since);
          let park =
            lw.state.is_some_and(|s| s.is_parked()) && !moved_since_park && (lw.parked_since.is_some() || quiet);
          if lw.state == Some(crate::limitwait::LimitState::Refused) && quiet {
            // Said outright that it will not come back. Nothing is gained by holding the
            // container open, and retrying now would only hit the same limit. Stillness is
            // required because an active task can display or quote the same prose.
            kill_child_tree(&mut child);
            killed = Killed::LimitExhausted { resets_at: lw.resets_at };
            break child.wait()?;
          } else if park {
            if lw.parked_since.is_none() {
              // The quiet gate proved that nothing happened throughout this interval. Freeze
              // it from the last real frame, not from this later confirmation instant.
              lw.parked_since = Some(last_activity);
            }
            if lw.expired(policy, crate::daemon::now_unix_secs()) {
              kill_child_tree(&mut child);
              killed = Killed::LimitExhausted { resets_at: lw.resets_at };
              break child.wait()?;
            }
          } else if let Some(since) = lw.parked_since.take() {
            // Moving again (or never really stopped): bank the stretch.
            let stretch = since.elapsed();
            lw.frozen_closed += stretch;
            // The inactivity clock is pushed forward only when its silence gap actually spans
            // the park. A run that resumed by producing output moved `last_activity` to now
            // already; adding the stretch on top would put it in the future and buy a wedged
            // harness a free extra window.
            if last_activity < since {
              last_activity = last_activity.checked_add(stretch).unwrap_or(last_activity);
            }
          }
        }
        let frozen = limits.as_ref().map(LimitWatch::frozen).unwrap_or_default();
        let parked = limits.as_ref().map(LimitWatch::open_stretch).unwrap_or_default();
        let limit_stopped =
          limits.as_ref().and_then(|lw| lw.state).is_some_and(crate::limitwait::LimitState::is_stopped);
        if let Some(limit) = timeout {
          if started.elapsed().saturating_sub(frozen) >= limit {
            kill_child_tree(&mut child);
            killed = Killed::Timeout;
            break child.wait()?;
          }
        }
        // Checked before the inactivity watchdog: a harness that finished its work and then
        // wedged is silent on both counts, and "the result is written" is the truthful reading
        // of that silence. `confirm` runs only after quiescence, so the cost is one stat per
        // poll until the file actually settles.
        if let Some(d) = done {
          if quiescence.as_mut().is_some_and(|q| q.poll(d.quiet_for)) && (d.confirm)() {
            kill_child_tree(&mut child);
            killed = Killed::Done;
            break child.wait()?;
          }
        }
        if let Some(w) = watch {
          // Startup rules first (they are strictly tighter): the initial-silence budget
          // until the first novel frame, then the stall budget for any silence that BEGINS
          // while the startup window is still open.
          let silent_for = last_activity.elapsed().saturating_sub(parked);
          if let Some(su) = &w.startup {
            let limit = if saw_novelty { su.stall } else { su.silence };
            let stall_began_in_window = last_activity.duration_since(watch_started) < su.window;
            // A confirmed limit screen still has to pass the quiet gate before clocks freeze,
            // but a shorter jittered startup threshold must not kill it during that guard.
            if !limit_stopped && stall_began_in_window && silent_for >= limit {
              kill_child_tree(&mut child);
              killed = Killed::StartupStalled { silent: !saw_novelty };
              break child.wait()?;
            }
          }
          if silent_for >= w.limit {
            kill_child_tree(&mut child);
            killed = Killed::Inactive;
            break child.wait()?;
          }
        }
        thread::sleep(Duration::from_millis(100));
      }
    };
    let drained = drain_pumps(pumps, drain_deadline(killed, timeout, started.elapsed()));
    // Both sweeps run after the leader was reaped, so both ask [`orphaned_child_group`] whether
    // the group is still ours before signalling it — a recycled pid must not cost a stranger a
    // SIGKILL.
    if !drained && killed == Killed::No {
      // The direct child exited but a descendant retained its output pipes. The command is not
      // complete until those pipes close, so the same wall-clock timeout tears its group down.
      if orphaned_child_group(pid) {
        terminate_child_group(pid);
      }
      killed = Killed::Timeout;
    } else if killed == Killed::No && orphaned_child_group(pid) {
      // A background descendant may close or redirect the inherited pipes. It must still not
      // escape a completed command and continue mutating the caller's machine.
      terminate_child_group(pid);
    }
    unregister_child(pid);
    let last = last.lock().unwrap().clone();
    Ok((status, killed, last, output_trimmed.load(Ordering::Relaxed)))
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

  fn finish(&self, status: Status, detail: Option<&str>, fail_reason: Option<&str>) {
    let elapsed = self.start_instant().elapsed().as_secs_f64();
    self.finish_with(status, detail, fail_reason, elapsed);
  }

  fn finish_with(&self, status: Status, detail: Option<&str>, fail_reason: Option<&str>, elapsed: f64) {
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
    &self, reader: R, started: Instant, last: Arc<Mutex<Option<String>>>, output_limit: Option<OutputLimit>,
    output_trimmed: Arc<AtomicBool>, cast: Option<crate::ptyrec::CastSink>,
  ) -> JoinHandle<()> {
    let (i, attended, tail, model, sink) =
      (self.i, self.attended, self.tail, Arc::clone(&self.model), self.sink.clone());
    thread::spawn(move || {
      let process_line = |raw: String, read_trimmed: bool| {
        if let Some(cast) = &cast {
          let terminal_line = if let Some(line) = raw.strip_suffix('\n') {
            if line.ends_with('\r') {
              raw.clone()
            } else {
              format!("{line}\r\n")
            }
          } else if raw.ends_with('\r') {
            format!("{raw}\n")
          } else {
            format!("{raw}\r\n")
          };
          cast.output(&terminal_line);
        }
        if read_trimmed {
          output_trimmed.store(true, Ordering::Relaxed);
        }
        let mut cleaned = clean_line(&raw);
        if cleaned.is_empty() {
          return;
        }
        if let Some(limit) = output_limit {
          if cleaned.len() > limit.bytes {
            let mut cut = cleaned.len() - limit.bytes;
            while !cleaned.is_char_boundary(cut) {
              cut += 1;
            }
            cleaned = cleaned[cut..].to_string();
            output_trimmed.store(true, Ordering::Relaxed);
          }
        }
        let at = started.elapsed().as_secs_f64();
        // Every line goes to the session browser, bounded output or not: the daemon keeps its
        // own newest-5000 window (`push_proc_lines`), so capping here would leave the browser
        // showing a build's HEAD while the terminal, the model, and the published `output`
        // field all show its TAIL — the half that says why a gate went red.
        if let Some(s) = &sink {
          s.proc_line(i, at, &cleaned);
        }
        {
          let mut m = model.lock().unwrap();
          if let Some(limit) = output_limit {
            if m.push_line_bounded(i, at, cleaned.clone(), limit.lines, limit.bytes) {
              output_trimmed.store(true, Ordering::Relaxed);
            }
          } else {
            m.push_line(i, at, cleaned.clone());
          }
          if attended {
            m.set_note(i, Some(cleaned.clone()));
          }
        }
        if !attended && tail {
          eprintln!("  {}", style(&cleaned).dim());
        }
        *last.lock().unwrap() = Some(cleaned);
      };

      let mut reader = BufReader::new(reader);
      if let Some(limit) = output_limit {
        while let Ok(Some((raw, trimmed))) = read_line_tail(&mut reader, limit.bytes) {
          process_line(raw, trimmed);
        }
      } else {
        for line in reader.lines() {
          let Ok(raw) = line else { break };
          process_line(raw, false);
        }
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
    Status::Graceful => (style("!").green().bold(), true),
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
  fn startup_jitter_stays_within_20pct_and_varies_by_seed() {
    // Every threshold lands inside [0.8×, 1.2×] of its base, for a spread of seeds.
    for seed in [0u64, 1, 7, 42, 999, u64::MAX, 0x9E37_79B9_7F4A_7C15] {
      let j = StartupStall::jittered(seed);
      for (got, base) in
        [(j.silence, STARTUP_SILENCE_SECS), (j.stall, STARTUP_STALL_SECS), (j.window, STARTUP_WINDOW_SECS)]
      {
        let lo = Duration::from_millis(base * 800);
        let hi = Duration::from_millis(base * 1200);
        assert!(got >= lo && got <= hi, "seed {seed}: {got:?} outside [{lo:?}, {hi:?}] for base {base}s");
      }
    }
    // Different seeds spread the silence threshold apart — the whole point of the jitter.
    let a = StartupStall::jittered(1).silence;
    let b = StartupStall::jittered(2).silence;
    assert_ne!(a, b, "distinct seeds must not collide on the same threshold");
    // The three thresholds are perturbed independently (rotated seed), not by one shared factor.
    let j = StartupStall::jittered(12345);
    let silence_ratio = j.silence.as_millis() * STARTUP_STALL_SECS as u128;
    let stall_ratio = j.stall.as_millis() * STARTUP_SILENCE_SECS as u128;
    assert_ne!(silence_ratio, stall_ratio, "thresholds must jitter independently");
  }

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
  fn a_command_that_finishes_on_the_last_tick_still_gets_time_to_drain() {
    let limit = Duration::from_secs(90);
    // Mid-run: whatever is left of the budget.
    assert_eq!(drain_deadline(Killed::No, Some(limit), Duration::from_secs(30)), Some(Duration::from_secs(60)));
    // On the wire, and past it: never zero, or the joiner loses a scheduling race and a command
    // that exited 0 is reported as having overrun its timeout.
    assert_eq!(drain_deadline(Killed::No, Some(limit), limit), Some(PUMP_DRAIN_CLOSE_GRACE));
    assert_eq!(drain_deadline(Killed::No, Some(limit), Duration::from_secs(120)), Some(PUMP_DRAIN_CLOSE_GRACE));
    // No budget at all: the readers are waited out, as they always were.
    assert_eq!(drain_deadline(Killed::No, None, Duration::from_secs(30)), None);
    // A killed child's pipes may be held by an escapee, so that wait is always bounded.
    assert_eq!(drain_deadline(Killed::Timeout, None, Duration::from_secs(30)), Some(PUMP_DRAIN_GRACE));
  }

  #[test]
  fn bounded_line_reader_keeps_the_tail_without_buffering_the_whole_line() {
    let input = vec![b'x'; 100_000];
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    let (line, trimmed) = read_line_tail(&mut reader, 1024).unwrap().expect("one unterminated line");
    assert!(trimmed);
    assert_eq!(line.len(), 1024);
    assert!(line.bytes().all(|byte| byte == b'x'));
    assert!(read_line_tail(&mut reader, 1024).unwrap().is_none());
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
    let (ok, killed, _) =
      p.run_watched("sleep", &["5".to_string()], Some(Duration::from_millis(150)), None, None).unwrap();
    assert_eq!(killed, Killed::Timeout);
    assert!(!ok, "the 5s sleep must be killed by the 150ms timeout");
  }

  /// Whether `pid` is still running, excluding a dead process waiting to be reaped.
  #[cfg(unix)]
  fn process_running(pid: u32) -> bool {
    let Ok(output) =
      std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).stderr(Stdio::null()).output()
    else {
      return false;
    };
    output.status.success() && !String::from_utf8_lossy(&output.stdout).trim_start().starts_with('Z')
  }

  #[cfg(unix)]
  #[test]
  fn proc_run_watched_kills_the_childs_descendants_too() {
    // The realistic shape of a killed child: `sh` is not the process doing the work. Here it
    // forks a long sleeper and waits on it, as `make` forks a compiler. Killing `sh` alone
    // would leave that sleeper running AND holding the output pipe, so the pump threads would
    // never see EOF — the watchdog would fire and `exec` would still never return. Both halves
    // are asserted: it comes back on the timeout's clock, and nothing is left behind.
    let ui = LiveUi::new(false, None);
    let p = ui.proc("tree", false);
    p.start();
    let script = "sleep 45 & printf 'child=%s\\n' \"$!\"; wait".to_string();
    let began = Instant::now();
    let (_ok, killed, _) =
      p.run_watched("sh", &["-c".to_string(), script], Some(Duration::from_millis(300)), None, None).unwrap();
    assert_eq!(killed, Killed::Timeout);
    assert!(began.elapsed() < Duration::from_secs(30), "returned in {:?}, not on the sleeper's clock", began.elapsed());

    let line = p.tail_lines(20).into_iter().find(|l| l.starts_with("child=")).expect("the fork announced its pid");
    let child: u32 = line.trim_start_matches("child=").trim().parse().expect("a pid");
    // The group SIGKILL is asynchronous and init reaps at its own pace; poll rather than race.
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_running(child) && Instant::now() < deadline {
      thread::sleep(Duration::from_millis(100));
    }
    assert!(!process_running(child), "pid {child} outlived the kill — it reached `sh` only");
  }

  #[cfg(unix)]
  #[test]
  fn proc_run_watched_times_out_descendants_after_the_shell_exits() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("detached tree", false);
    p.start();
    let began = Instant::now();
    let (_ok, killed, _) = p
      .run_watched("sh", &["-c".to_string(), "sleep 45 &".to_string()], Some(Duration::from_millis(300)), None, None)
      .unwrap();
    assert_eq!(killed, Killed::Timeout);
    assert!(began.elapsed() < Duration::from_secs(30), "returned in {:?}, not on the timeout's clock", began.elapsed());
  }

  #[cfg(unix)]
  #[test]
  fn proc_run_watched_cleans_up_background_descendants_that_closed_the_pipes() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("redirected tree", false);
    p.start();
    let (ok, killed, _) = p
      .run_watched(
        "sh",
        &["-c".to_string(), "sleep 45 >/dev/null 2>&1 & printf 'child=%s\\n' \"$!\"".to_string()],
        Some(Duration::from_secs(5)),
        None,
        None,
      )
      .unwrap();
    assert!(ok);
    assert_eq!(killed, Killed::No);
    let line = p.tail_lines(20).into_iter().find(|line| line.starts_with("child=")).expect("child pid");
    let child: u32 = line.trim_start_matches("child=").parse().unwrap();
    assert!(!process_running(child), "background pid {child} survived its completed shell");
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
      startup: None,
      limit_wait: None,
    };
    let (ok, killed, _) = p.run_watched("sleep", &["5".to_string()], None, Some(&watch), None).unwrap();
    assert_eq!(killed, Killed::Inactive);
    assert!(!ok);
  }

  // ---- usage-limit wait ----
  //
  // The whole point of the feature: a claude session stopped by a usage limit looks EXACTLY
  // like a wedged one — a static screen — and without this it is killed half an hour in and
  // retried from a fresh clone, losing everything the agent worked out. These tests pin the
  // three outcomes: hold the clocks while it waits, give up when it says it will not resume,
  // and press the key the screens that need one are waiting for.

  const SIX_HOURS: Duration = Duration::from_secs(6 * 60 * 60);

  #[test]
  fn a_known_reset_does_not_expire_at_the_old_halfway_point() {
    let resets_at = 600;
    let halfway = (resets_at + LIMIT_RESET_GRACE.as_secs()) / 2;
    assert!(!limit_wait_expired(Duration::from_secs(halfway), SIX_HOURS, Some(resets_at), halfway));
  }

  #[test]
  fn a_known_reset_expires_exactly_at_reset_plus_grace() {
    let resets_at = 10_000;
    let expiry = resets_at + LIMIT_RESET_GRACE.as_secs();
    assert!(!limit_wait_expired(Duration::from_secs(1_000), SIX_HOURS, Some(resets_at), expiry - 1));
    assert!(limit_wait_expired(Duration::from_secs(1_000), SIX_HOURS, Some(resets_at), expiry));
    assert!(limit_wait_expired(Duration::ZERO, SIX_HOURS, Some(u64::MAX), u64::MAX));
  }

  #[test]
  fn an_unknown_reset_expires_only_at_the_policy_maximum() {
    assert!(!limit_wait_expired(SIX_HOURS - Duration::from_nanos(1), SIX_HOURS, None, u64::MAX));
    assert!(limit_wait_expired(SIX_HOURS, SIX_HOURS, None, 0));
  }

  #[test]
  fn a_distant_reset_cannot_extend_the_policy_maximum() {
    let now = 10_000;
    let resets_at = now + 24 * 60 * 60;
    assert!(!limit_wait_expired(SIX_HOURS - Duration::from_nanos(1), SIX_HOURS, Some(resets_at), now));
    assert!(limit_wait_expired(SIX_HOURS, SIX_HOURS, Some(resets_at), now));
  }

  /// Write an asciicast v3 file whose output events render to `screens`, in order.
  #[cfg(unix)]
  fn write_cast(path: &std::path::Path, screens: &[&str]) {
    let mut out = String::from("{\"version\": 3, \"term\": {\"cols\": 80, \"rows\": 24}}\n");
    for (i, screen) in screens.iter().enumerate() {
      out.push_str(&format!("[{}.0, \"o\", {}]\n", i, crate::json::quote(screen)));
    }
    std::fs::write(path, out).unwrap();
  }

  #[cfg(unix)]
  fn append_cast(path: &std::path::Path, index: usize, screen: &str) {
    let mut out = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(out, "[{}.0, \"o\", {}]", index, crate::json::quote(screen)).unwrap();
  }

  #[cfg(unix)]
  fn limit_watch(file: &std::path::Path, keys: &std::path::Path, max: Duration) -> ActivityWatch<'static> {
    ActivityWatch {
      file: file.to_path_buf(),
      // Deliberately tiny: without the limit wait this alone would kill the child at once, so
      // a passing test proves the clock really is frozen rather than merely generous.
      limit: Duration::from_millis(200),
      startup: None,
      limit_wait: Some(LimitWait {
        max,
        keys: keys.to_path_buf(),
        resets_at: Box::new(|| None),
        on_state: Box::new(|_, _| {}),
      }),
    }
  }

  #[cfg(unix)]
  #[test]
  fn key_publication_replaces_a_symlink_without_following_it() {
    use std::os::unix::fs::symlink;

    let dir = std::env::temp_dir().join(format!("scsh-key-publish-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir(&dir).unwrap();
    let victim = dir.join("victim");
    let key = dir.join("key");
    std::fs::write(&victim, "untouched").unwrap();
    symlink(&victim, &key).unwrap();

    publish_key(&key, 1, "Enter").unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "1 Enter\n");
    assert!(!std::fs::symlink_metadata(&key).unwrap().file_type().is_symlink());

    publish_key(&key, 2, "Enter").unwrap();
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "2 Enter\n", "a repeated key gets a new record id");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "atomic publication leaves no temporary file");
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[cfg(unix)]
  #[test]
  fn a_run_waiting_out_a_usage_limit_is_not_killed_as_inactive() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("limited", false);
    p.start();
    let dir = std::env::temp_dir();
    let file = dir.join(format!("scsh-limit-armed-{}.cast", std::process::id()));
    let keys = dir.join(format!("scsh-limit-armed-{}.keys", std::process::id()));
    let _ = std::fs::remove_file(&keys);
    write_cast(
      &file,
      &["working…", "Usage limit reached \u{b7} continuing automatically at 8:50am \u{b7} esc to cancel"],
    );

    let watch = limit_watch(&file, &keys, Duration::from_secs(2));
    let started = Instant::now();
    let (ok, killed, _) = p.run_watched("sleep", &["30".to_string()], None, Some(&watch), None).unwrap();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&keys);

    // Held past the 200ms inactivity budget, then stopped at the wait's own ceiling — with the
    // reason that says "quota", not "this harness hung".
    assert_eq!(killed, Killed::LimitExhausted { resets_at: None });
    assert!(!ok);
    assert!(started.elapsed() >= Duration::from_secs(2), "the inactivity clock was frozen, not merely widened");
    assert!(started.elapsed() < Duration::from_secs(10), "the wait is bounded by `max`");
  }

  #[cfg(unix)]
  #[test]
  fn a_limit_banner_during_startup_outlives_the_shortest_stall_threshold() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("limited-during-startup", false);
    p.start();
    let dir = std::env::temp_dir();
    let nonce = crate::runtime::random_nonce_6();
    let file = dir.join(format!("scsh-limit-startup-{nonce}.cast"));
    let keys = dir.join(format!("scsh-limit-startup-{nonce}.keys"));
    write_cast(
      &file,
      &["starting claude", "Usage limit reached \u{b7} continuing automatically at 8:50am \u{b7} esc to cancel"],
    );
    let writer_file = file.clone();
    let writer = std::thread::spawn(move || {
      std::thread::sleep(Duration::from_millis(300));
      append_cast(&writer_file, 1, "tmux helper attached to session");
    });

    let mut watch = limit_watch(&file, &keys, Duration::from_millis(600));
    let states = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let callback_states = std::sync::Arc::clone(&states);
    watch.limit_wait.as_mut().unwrap().on_state = Box::new(move |state, _| callback_states.lock().unwrap().push(state));
    watch.startup = Some(StartupStall {
      silence: Duration::from_millis(50),
      stall: Duration::from_millis(50),
      window: Duration::from_secs(2),
    });
    let started = Instant::now();
    let (ok, killed, _) = p.run_watched("sleep", &["30".to_string()], None, Some(&watch), None).unwrap();
    writer.join().unwrap();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&keys);

    assert_eq!(killed, Killed::LimitExhausted { resets_at: None });
    assert!(!ok);
    assert!(started.elapsed() >= Duration::from_millis(600), "the quota wait, not startup jitter, owned the run");
    assert!(started.elapsed() < Duration::from_secs(2), "the quiet gate was included in the frozen stretch");
    let states = states.lock().unwrap();
    assert!(states.contains(&Some(crate::limitwait::LimitState::Waiting)), "the live process reported awaiting limits");
    assert!(!states.contains(&None), "the unrelated tmux notice did not clear the stopped state");
  }

  #[cfg(unix)]
  #[test]
  fn a_harness_that_says_it_will_not_resume_is_given_up_on_at_once() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("refused", false);
    p.start();
    let dir = std::env::temp_dir();
    let file = dir.join(format!("scsh-limit-refused-{}.cast", std::process::id()));
    let keys = dir.join(format!("scsh-limit-refused-{}.keys", std::process::id()));
    write_cast(
      &file,
      &[
        "Usage limit reached \u{b7} continuing automatically at 8:50am \u{b7} esc to cancel",
        "Automatic continue stopped after repeated usage-limit hits \u{b7} this task will not resume on its own",
      ],
    );

    // A long ceiling: nothing but the screen's own verdict should end this wait.
    let watch = limit_watch(&file, &keys, Duration::from_secs(3600));
    let started = Instant::now();
    let (ok, killed, _) = p.run_watched("sleep", &["30".to_string()], None, Some(&watch), None).unwrap();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&keys);

    assert_eq!(killed, Killed::LimitExhausted { resets_at: None });
    assert!(!ok);
    assert!(started.elapsed() < Duration::from_secs(5), "a stated refusal ends the wait immediately");
  }

  #[cfg(unix)]
  #[test]
  fn active_output_that_quotes_a_refusal_is_not_killed() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("quotes-refusal", false);
    p.start();
    let dir = std::env::temp_dir();
    let nonce = crate::runtime::random_nonce_6();
    let file = dir.join(format!("scsh-limit-quoted-refusal-{nonce}.cast"));
    let keys = dir.join(format!("scsh-limit-quoted-refusal-{nonce}.keys"));
    write_cast(&file, &["The provider said this task will not resume on its own; investigating why."]);
    let writer_file = file.clone();
    let writer = std::thread::spawn(move || {
      for (index, word) in
        ["checking", "reading", "tracing", "testing", "fixing", "building", "reviewing", "finishing"].iter().enumerate()
      {
        std::thread::sleep(Duration::from_millis(100));
        append_cast(&writer_file, index + 1, word);
      }
    });

    let mut watch = limit_watch(&file, &keys, Duration::from_secs(2));
    watch.limit = Duration::from_millis(500);
    let (ok, killed, _) = p.run_watched("sleep", &["1".to_string()], None, Some(&watch), None).unwrap();
    writer.join().unwrap();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&keys);

    assert_eq!(killed, Killed::No);
    assert!(ok, "continued novel output proves the refusal prose was only being quoted");
  }

  #[cfg(unix)]
  #[test]
  fn active_output_that_quotes_a_limit_prompt_gets_no_keypress() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("quotes-limit-prompt", false);
    p.start();
    let dir = std::env::temp_dir();
    let nonce = crate::runtime::random_nonce_6();
    let file = dir.join(format!("scsh-limit-quoted-prompt-{nonce}.cast"));
    let keys = dir.join(format!("scsh-limit-quoted-prompt-{nonce}.keys"));
    write_cast(&file, &["The error said: Your usage limit has reset \u{b7} press enter to continue"]);
    let writer_file = file.clone();
    let writer = std::thread::spawn(move || {
      for (index, word) in
        ["checking", "reading", "tracing", "testing", "fixing", "building", "reviewing", "finishing"].iter().enumerate()
      {
        std::thread::sleep(Duration::from_millis(100));
        append_cast(&writer_file, index + 1, word);
      }
    });

    let mut watch = limit_watch(&file, &keys, Duration::from_secs(2));
    watch.limit = Duration::from_millis(500);
    let (ok, killed, _) = p.run_watched("sleep", &["1".to_string()], None, Some(&watch), None).unwrap();
    writer.join().unwrap();
    let sent = std::fs::read_to_string(&keys).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&keys);

    assert_eq!(killed, Killed::No);
    assert!(ok, "the quoted prompt did not stop active work");
    assert!(sent.is_empty(), "the quoted prompt must not inject Enter into the active TUI");
  }

  #[cfg(unix)]
  #[test]
  fn explicit_usage_limit_reset_restores_normal_watchdog_accounting() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("resumed-after-limit", false);
    p.start();
    let dir = std::env::temp_dir();
    let nonce = crate::runtime::random_nonce_6();
    let file = dir.join(format!("scsh-limit-stale-{nonce}.cast"));
    let keys = dir.join(format!("scsh-limit-stale-{nonce}.keys"));
    write_cast(&file, &["Usage limit reached \u{b7} continuing automatically at 8:50am \u{b7} esc to cancel"]);
    let writer_file = file.clone();
    let writer = std::thread::spawn(move || {
      std::thread::sleep(Duration::from_millis(300));
      append_cast(&writer_file, 1, "Usage limit reset \u{b7} continuing automatically");
      std::thread::sleep(Duration::from_millis(150));
      append_cast(&writer_file, 2, "ordinary work resumed");
    });

    let watch = limit_watch(&file, &keys, Duration::from_secs(2));
    let started = Instant::now();
    let (ok, killed, _) = p.run_watched("sleep", &["30".to_string()], None, Some(&watch), None).unwrap();
    writer.join().unwrap();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&keys);

    assert_eq!(killed, Killed::Inactive, "the explicit reset restores the ordinary silence budget");
    assert!(!ok);
    assert!(started.elapsed() < Duration::from_secs(4), "the reset state did not keep the quota wait open");
  }

  #[cfg(unix)]
  #[test]
  fn stopped_limit_scans_are_sticky_until_an_explicit_resume() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!("scsh-limit-recent-{}.cast", crate::runtime::random_nonce_6()));
    write_cast(&file, &["Usage limit reached \u{b7} continuing automatically at 8:50am \u{b7} esc to cancel"]);
    let mut novelty = NoveltyWatch::new(&file, true);

    assert!(novelty.poll());
    let waiting = Some(crate::limitwait::LimitState::Waiting);
    assert_eq!(recent_limit_state(&mut novelty, None), waiting);
    append_cast(&file, 1, "tmux helper attached to session");
    assert!(novelty.poll());
    assert_eq!(recent_limit_state(&mut novelty, waiting), waiting);
    append_cast(&file, 2, "Usage limit reset \u{b7} continuing automatically");
    assert!(novelty.poll());
    let resumed = Some(crate::limitwait::LimitState::Resumed);
    assert_eq!(recent_limit_state(&mut novelty, waiting), resumed);
    append_cast(&file, 3, "ordinary work resumed");
    assert!(novelty.poll());
    assert_eq!(recent_limit_state(&mut novelty, resumed), None);
    append_cast(
      &file,
      4,
      "Usage limit reached again after you continued \u{b7} continuing automatically at 10:30am \u{b7} esc to cancel",
    );
    assert!(novelty.poll());
    assert_eq!(recent_limit_state(&mut novelty, None), waiting);

    let _ = std::fs::remove_file(&file);
  }

  #[cfg(unix)]
  #[test]
  fn the_screens_that_need_a_keypress_get_one() {
    // Unattended, both of these sit forever: the limit dialog (whose DEFAULT choice is the
    // wait scsh wants) and the post-reset "press enter to continue".
    for (name, screen) in [
      ("dialog", "You've hit your session limit \u{b7} resets 8:50am (Europe/Stockholm)"),
      ("stale", "Your usage limit has reset \u{b7} press enter to continue"),
    ] {
      let ui = LiveUi::new(false, None);
      let p = ui.proc(name, false);
      p.start();
      let dir = std::env::temp_dir();
      let file = dir.join(format!("scsh-limit-key-{name}-{}.cast", std::process::id()));
      let keys = dir.join(format!("scsh-limit-key-{name}-{}.keys", std::process::id()));
      let _ = std::fs::remove_file(&keys);
      write_cast(&file, &["working…", screen]);

      let watch = limit_watch(&file, &keys, Duration::from_millis(600));
      let (_, killed, _) = p.run_watched("sleep", &["30".to_string()], None, Some(&watch), None).unwrap();
      let sent = std::fs::read_to_string(&keys).unwrap_or_default();
      let _ = std::fs::remove_file(&file);
      let _ = std::fs::remove_file(&keys);

      assert_eq!(killed, Killed::LimitExhausted { resets_at: None }, "{name}");
      assert_eq!(sent.split_whitespace().collect::<Vec<_>>(), ["1", "Enter"], "{name}: one Enter record is published");
    }
  }

  #[cfg(unix)]
  #[test]
  fn an_ordinary_frozen_screen_is_still_an_inactivity_kill() {
    // The guard on all of the above: arming the limit wait must not turn every wedged harness
    // into a patient one. A screen that says nothing about limits is killed on schedule.
    let ui = LiveUi::new(false, None);
    let p = ui.proc("wedged", false);
    p.start();
    let dir = std::env::temp_dir();
    let file = dir.join(format!("scsh-limit-none-{}.cast", std::process::id()));
    let keys = dir.join(format!("scsh-limit-none-{}.keys", std::process::id()));
    write_cast(&file, &["compiling scsh v1.42.0", "  Finished dev profile"]);

    let watch = limit_watch(&file, &keys, Duration::from_secs(3600));
    let started = Instant::now();
    let (ok, killed, _) = p.run_watched("sleep", &["30".to_string()], None, Some(&watch), None).unwrap();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&keys);

    assert_eq!(killed, Killed::Inactive);
    assert!(!ok);
    assert!(started.elapsed() < Duration::from_secs(5), "killed on the 200ms budget, not the hour-long limit ceiling");
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
    let watch =
      ActivityWatch { file: file.clone(), limit: Duration::from_millis(600), startup: None, limit_wait: None };
    let (ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, Some(&watch), None).unwrap();
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
    let watch =
      ActivityWatch { file: file.clone(), limit: Duration::from_millis(500), startup: None, limit_wait: None };
    let (ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, Some(&watch), None).unwrap();
    let _ = std::fs::remove_file(&file);
    assert_eq!(killed, Killed::Inactive, "repeating frames are not activity");
    assert!(!ok);
  }

  #[test]
  fn startup_watch_kills_a_child_that_never_says_anything() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("mute", false);
    p.start();
    // The cast never appears: the tight initial-silence budget fires, not the (much longer)
    // inactivity limit, and the kill is reported as a SILENT startup stall.
    let watch = ActivityWatch {
      file: std::env::temp_dir().join(format!("scsh-startup-mute-{}", std::process::id())),
      limit: Duration::from_secs(20),
      startup: Some(StartupStall {
        silence: Duration::from_millis(200),
        stall: Duration::from_millis(400),
        window: Duration::from_millis(900),
      }),
      limit_wait: None,
    };
    let started = Instant::now();
    let (ok, killed, _) = p.run_watched("sleep", &["5".to_string()], None, Some(&watch), None).unwrap();
    assert_eq!(killed, Killed::StartupStalled { silent: true });
    assert!(!ok);
    assert!(started.elapsed() < Duration::from_secs(3), "killed on the startup budget, not the 20s watchdog");
  }

  #[cfg(unix)]
  #[test]
  fn startup_watch_kills_a_child_that_goes_quiet_mid_boot() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("wedges-early", false);
    p.start();
    let file = std::env::temp_dir().join(format!("scsh-startup-wedge-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    // A couple of novel frames right away (so the initial-silence rule is satisfied), then
    // silence while the startup window is still open: the stall budget fires long before the
    // 20s inactivity limit. `exec` so the wedge IS this child (see the DoneWatch test).
    let script = format!("echo boot-a >> {f}; echo boot-b >> {f}; exec sleep 30", f = file.display());
    let watch = ActivityWatch {
      file: file.clone(),
      limit: Duration::from_secs(20),
      startup: Some(StartupStall {
        silence: Duration::from_millis(2000),
        stall: Duration::from_millis(400),
        window: Duration::from_millis(5000),
      }),
      limit_wait: None,
    };
    let started = Instant::now();
    let (ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, Some(&watch), None).unwrap();
    let _ = std::fs::remove_file(&file);
    assert_eq!(killed, Killed::StartupStalled { silent: false }, "a stall inside the window is a startup stall");
    assert!(!ok);
    assert!(started.elapsed() < Duration::from_secs(5), "killed on the stall budget, not the 20s watchdog");
  }

  #[cfg(unix)]
  #[test]
  fn startup_watch_disarms_once_the_run_outlives_its_window() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("settles", false);
    p.start();
    let file = std::env::temp_dir().join(format!("scsh-startup-settled-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    // Novel frames past the end of the startup window, then silence: the silence BEGAN after
    // the window closed, so the ordinary inactivity watchdog owns it — Inactive, not a
    // startup stall.
    let script =
      format!("for w in a b c d e f g h; do echo tok-$w >> {}; sleep 0.1; done; exec sleep 30", file.display());
    let watch = ActivityWatch {
      file: file.clone(),
      limit: Duration::from_millis(700),
      startup: Some(StartupStall {
        silence: Duration::from_millis(400),
        stall: Duration::from_millis(500),
        window: Duration::from_millis(300),
      }),
      limit_wait: None,
    };
    let (ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, Some(&watch), None).unwrap();
    let _ = std::fs::remove_file(&file);
    assert_eq!(killed, Killed::Inactive, "a stall that begins after the window is plain inactivity");
    assert!(!ok);
  }

  /// The case this exists for: the agent writes its result and the CLI then wedges forever.
  /// The child never exits and never goes near its (deliberately long) inactivity limit, so
  /// only the completion watch can stop it.
  #[cfg(unix)]
  #[test]
  fn done_watch_stops_a_harness_that_finished_its_work_and_wedged() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("wedged", false);
    p.start();
    let result = std::env::temp_dir().join(format!("scsh-done-wedge-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&result);
    // Write the result once, then hang forever with a silent screen.
    // `exec` so the wedge IS this child: killing a plain `sh` would leave its `sleep`
    // grandchild holding the output pipe, and the join below would wait that out instead.
    let script = format!(r#"echo '{{"message":"done"}}' > {}; exec sleep 30"#, result.display());
    let done = DoneWatch { file: result.clone(), quiet_for: Duration::from_millis(300), confirm: Box::new(|| true) };
    // An inactivity limit far too long to be what stops this run.
    let watch = ActivityWatch { file: result.clone(), limit: Duration::from_secs(20), startup: None, limit_wait: None };
    let started = Instant::now();
    let (_ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, Some(&watch), Some(&done)).unwrap();
    let _ = std::fs::remove_file(&result);
    assert_eq!(killed, Killed::Done, "a written, quiet result ends the wait");
    assert!(started.elapsed() < Duration::from_secs(10), "stopped on the result, not the 20s watchdog");
  }

  /// A writer still working must keep resetting the quiescence clock — the whole point of
  /// waiting for quiet rather than trusting mere presence, since a half-written file is present.
  #[cfg(unix)]
  #[test]
  fn done_watch_waits_while_the_result_is_still_being_written() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("writing", false);
    p.start();
    let result = std::env::temp_dir().join(format!("scsh-done-partial-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&result);
    // Appends for ~800ms — every append restarts the 300ms quiet window — then exits on its own.
    let script = format!(r#"for w in a b c d e f g h; do echo "$w" >> {}; sleep 0.1; done"#, result.display());
    let done = DoneWatch { file: result.clone(), quiet_for: Duration::from_millis(300), confirm: Box::new(|| true) };
    let (ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, None, Some(&done)).unwrap();
    let body = std::fs::read_to_string(&result).unwrap_or_default();
    let _ = std::fs::remove_file(&result);
    assert_eq!(killed, Killed::No, "an active writer is never cut off mid-write");
    assert!(ok);
    assert_eq!(body.lines().count(), 8, "every line the writer intended survived");
  }

  /// Quiescence alone is not completion. A `commits: true` step writes its result and only then
  /// commits; stopping in that gap would throw the commit away. `confirm` is the veto, and it
  /// keeps being asked until it agrees.
  #[cfg(unix)]
  #[test]
  fn done_watch_defers_to_confirm_while_the_commit_is_still_pending() {
    let ui = LiveUi::new(false, None);
    let p = ui.proc("committing", false);
    p.start();
    let result = std::env::temp_dir().join(format!("scsh-done-commit-{}.json", std::process::id()));
    let committed = std::env::temp_dir().join(format!("scsh-done-ref-{}", std::process::id()));
    let _ = std::fs::remove_file(&result);
    let _ = std::fs::remove_file(&committed);
    // Result lands immediately and goes quiet; the "commit" only appears ~700ms later.
    let script = format!(
      r#"echo '{{"message":"done"}}' > {}; sleep 0.7; touch {}; exec sleep 30"#,
      result.display(),
      committed.display()
    );
    let done = DoneWatch {
      file: result.clone(),
      quiet_for: Duration::from_millis(200),
      confirm: {
        let c = committed.clone();
        Box::new(move || c.exists())
      },
    };
    let (_ok, killed, _) = p.run_watched("sh", &["-c".to_string(), script], None, None, Some(&done)).unwrap();
    let had_commit = committed.exists();
    let _ = std::fs::remove_file(&result);
    let _ = std::fs::remove_file(&committed);
    assert_eq!(killed, Killed::Done);
    assert!(had_commit, "the run was not stopped until the commit had landed");
  }

  #[test]
  fn quiescence_resets_whenever_the_stamp_changes() {
    let file = std::env::temp_dir().join(format!("scsh-quiesce-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let mut q = QuiescenceWatch::new(&file);
    assert!(!q.poll(Duration::ZERO), "a file that does not exist yet has not gone quiet");
    std::fs::write(&file, b"{}").unwrap();
    assert!(!q.poll(Duration::ZERO), "the first sighting only starts the clock");
    assert!(q.poll(Duration::ZERO), "an unchanged stamp is quiet");
    std::fs::write(&file, b"{\"a\":1}").unwrap();
    assert!(!q.poll(Duration::ZERO), "a changed stamp restarts the clock");
    assert!(!q.poll(Duration::from_secs(60)), "and the new window has not elapsed");
    let _ = std::fs::remove_file(&file);
    assert!(!q.poll(Duration::ZERO), "a vanished file is not quiet");
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
