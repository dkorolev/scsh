//! The interactive live board's **pure model** — everything that decides *what the screen
//! shows*, with no terminal I/O, so it can be unit-tested exhaustively. The terminal driver
//! (raw mode, mouse, redraw) lives in [`super::screen`] and is the only side-effecting part.
//!
//! A run is a list of [`Proc`]s (the image build, then one per skill). Each is a collapsible
//! row: a ▶/▼ triangle, a status glyph, the label, a smart elapsed clock, and a dim note.
//! Click a row (the driver maps the mouse to a proc) to [`Model::toggle`] it; expanding shows
//! that proc's captured output, every line stamped with `+<elapsed>` **relative to when that
//! proc started**. Scroll within the expanded block to read it all; End jumps to the fleet tail.

use super::clock::format_elapsed;
use super::FRAMES;

/// A proc's lifecycle state, which drives its glyph and colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
  /// Declared but not started yet (e.g. a skill waiting for the image build).
  Queued,
  Running,
  Ok,
  Fail,
}

/// One captured output line, with the time (seconds since the proc started) it arrived — so the
/// expanded view can stamp it `+1.2s` relative to that proc's own start.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
  pub at: f64,
  pub text: String,
}

/// One collapsible process row plus its captured output.
#[derive(Clone, Debug)]
pub struct Proc {
  pub label: String,
  pub status: Status,
  /// Elapsed seconds, refreshed by the driver each tick while running (and frozen at finish).
  pub elapsed: f64,
  /// Every captured output line, in arrival order, each tagged with its relative time.
  pub lines: Vec<Line>,
  /// The dim trailing note on the header — the latest output line while running.
  pub note: Option<String>,
  /// The dim/red detail on the header once finished (e.g. the result headline, or why it failed).
  pub detail: Option<String>,
  pub expanded: bool,
}

/// A styled run of text within a rendered row. The driver maps [`Sty`] to terminal colour; tests
/// read [`Row::plain`] (the concatenated text), so the model stays output-format-agnostic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sty {
  Plain,
  Dim,
  Bold,
  Cyan,
  Green,
  Red,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
  pub text: String,
  pub sty: Sty,
}

impl Seg {
  fn new(text: impl Into<String>, sty: Sty) -> Seg {
    Seg { text: text.into(), sty }
  }
}

/// One rendered line of the board. `proc` is `Some(i)` for a clickable header row (so the driver
/// can map a mouse click back to a proc) and `None` for an output/detail line.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
  pub proc: Option<usize>,
  pub segs: Vec<Seg>,
}

impl Row {
  /// The row's text with styling discarded — what tests assert on, and what width is measured by.
  pub fn plain(&self) -> String {
    self.segs.iter().map(|s| s.text.as_str()).collect()
  }
}

/// The whole live board: the procs, the scroll offset, and follow flags.
#[derive(Debug, Default)]
pub struct Model {
  pub procs: Vec<Proc>,
  /// Index of the first laid-out row shown at the top of the viewport.
  scroll: usize,
  /// While true, the viewport sticks to the bottom of the whole board (like `tail -f`). The user
  /// turns it off by scrolling up, and back on with End / scroll-to-bottom.
  follow: bool,
}

impl Model {
  pub fn new() -> Model {
    Model { procs: Vec::new(), scroll: 0, follow: true }
  }

  /// Add a proc, returning its index (the handle the driver/worker uses to update it).
  pub fn add(&mut self, label: impl Into<String>) -> usize {
    self.procs.push(Proc {
      label: label.into(),
      status: Status::Queued,
      elapsed: 0.0,
      lines: Vec::new(),
      note: None,
      detail: None,
      expanded: false,
    });
    self.procs.len() - 1
  }

  pub fn set_status(&mut self, i: usize, status: Status) {
    if let Some(p) = self.procs.get_mut(i) {
      p.status = status;
    }
  }

  pub fn set_elapsed(&mut self, i: usize, elapsed: f64) {
    if let Some(p) = self.procs.get_mut(i) {
      p.elapsed = elapsed;
    }
  }

  pub fn set_note(&mut self, i: usize, note: Option<String>) {
    if let Some(p) = self.procs.get_mut(i) {
      p.note = note;
    }
  }

  pub fn set_detail(&mut self, i: usize, detail: Option<String>) {
    if let Some(p) = self.procs.get_mut(i) {
      p.detail = detail;
    }
  }

  /// Append a captured output line, stamped with `at` seconds since the proc started.
  pub fn push_line(&mut self, i: usize, at: f64, text: impl Into<String>) {
    if let Some(p) = self.procs.get_mut(i) {
      p.lines.push(Line { at, text: text.into() });
    }
  }

  /// Last `max` captured lines for proc `i` (for failure messages).
  pub fn tail_lines(&self, i: usize, max: usize) -> Vec<String> {
    let Some(p) = self.procs.get(i) else {
      return Vec::new();
    };
    let start = p.lines.len().saturating_sub(max);
    p.lines[start..].iter().map(|l| l.text.clone()).collect()
  }

  /// Toggle a proc's expanded state (what a mouse click on its header does). Expanding scrolls
  /// the viewport to that proc's header so you can read its output from the top; collapsing
  /// leaves the scroll position unchanged.
  pub fn toggle(&mut self, i: usize) {
    if let Some(p) = self.procs.get_mut(i) {
      p.expanded = !p.expanded;
      if p.expanded {
        self.follow = false;
        self.scroll = self.proc_first_row_index(i);
      }
    }
  }

  /// Expand or collapse every proc at once (the `e` / `c` keys). Resumes global tail-follow when
  /// expanding all.
  pub fn set_all_expanded(&mut self, expanded: bool) {
    for p in &mut self.procs {
      p.expanded = expanded;
    }
    if expanded {
      self.follow = true;
    }
  }

  /// Lay the whole board out into rows (headers + any expanded output), each clipped to `width`
  /// so nothing wraps. `frame` advances the running spinner glyph. Pure: the driver renders the
  /// returned segments, tests read [`Row::plain`].
  pub fn layout(&self, width: usize, frame: u64) -> Vec<Row> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for (i, p) in self.procs.iter().enumerate() {
      rows.push(fit_row(header_row_framed(i, p, frame), width));
      if p.expanded {
        if p.lines.is_empty() {
          rows.push(fit_row(Row { proc: None, segs: vec![Seg::new("    (no output yet)", Sty::Dim)] }, width));
        } else {
          for line in &p.lines {
            rows.push(fit_row(output_row(line), width));
          }
        }
      }
    }
    rows
  }

  /// Total laid-out row count (independent of the spinner frame, so any frame works).
  pub fn total_rows(&self, width: usize) -> usize {
    self.layout(width, 0).len()
  }

  /// The slice of rows visible in a `height`-tall viewport, plus the scroll offset actually used
  /// (after applying "follow the tail" and clamping). The driver draws the rows and remembers the
  /// offset so it can map a mouse click at screen-row `r` back to laid-out row `offset + r`.
  pub fn view(&self, width: usize, height: usize, frame: u64) -> (Vec<Row>, usize) {
    let all = self.layout(width, frame);
    let height = height.max(1);
    let max_off = all.len().saturating_sub(height);
    let off = self.viewport_offset(height, max_off);
    let end = (off + height).min(all.len());
    (all[off..end].to_vec(), off)
  }

  /// Scroll by `delta` rows (negative = up/back, positive = down/forward) within a `height`-tall
  /// viewport. Scrolling up drops global follow; reaching the bottom restores it.
  pub fn scroll_by(&mut self, delta: isize, width: usize, height: usize) {
    let total = self.total_rows(width);
    let height = height.max(1);
    let max_off = total.saturating_sub(height);
    let cur = self.viewport_offset(height, max_off);
    let next = cur.saturating_add_signed(delta).min(max_off);
    self.scroll = next;
    self.follow = next >= max_off;
  }

  /// Jump to the top (stops following) or the bottom (resumes global following).
  pub fn scroll_to_top(&mut self) {
    self.scroll = 0;
    self.follow = false;
  }
  pub fn scroll_to_bottom(&mut self) {
    self.follow = true;
  }

  /// Laid-out row index of this proc's header row.
  fn proc_first_row_index(&self, proc_index: usize) -> usize {
    let mut row = 0usize;
    for (i, p) in self.procs.iter().enumerate() {
      if i == proc_index {
        return row;
      }
      row += rows_for_proc(p);
    }
    row.saturating_sub(1)
  }

  /// First visible row offset for the current follow mode.
  fn viewport_offset(&self, _height: usize, max_off: usize) -> usize {
    if self.follow {
      max_off
    } else {
      self.scroll.min(max_off)
    }
  }
}

/// Layout row count for one proc: one header, plus expanded output (or a placeholder).
fn rows_for_proc(p: &Proc) -> usize {
  1 + if p.expanded {
    if p.lines.is_empty() {
      1
    } else {
      p.lines.len()
    }
  } else {
    0
  }
}

/// Keyboard shortcut shown on proc row `i`: `[0]`..`[9]`, then `[A]`..`[Z]`.
pub fn shortcut_label(proc_index: usize) -> String {
  if proc_index <= 9 {
    format!("[{}] ", proc_index)
  } else if proc_index <= 35 {
    format!("[{}] ", char::from(b'A' + (proc_index - 10) as u8))
  } else {
    String::new()
  }
}

/// Map a shortcut key to a proc index (`0`→0, …, `9`→9, `A`→10, …, `Z`→35).
pub fn proc_index_from_key(c: char) -> Option<usize> {
  if c.is_ascii_digit() {
    Some((c as u8 - b'0') as usize)
  } else if c.is_ascii_alphabetic() {
    Some(10 + (c.to_ascii_uppercase() as u8 - b'A') as usize)
  } else {
    None
  }
}

/// The status glyph (and its colour) for a proc: an animated spinner while running, else ✓/✗/·.
fn glyph(status: Status, frame: u64) -> Seg {
  match status {
    Status::Queued => Seg::new("·", Sty::Dim),
    Status::Running => Seg::new(FRAMES[(frame as usize) % FRAMES.len()], Sty::Cyan),
    Status::Ok => Seg::new("✓", Sty::Green),
    Status::Fail => Seg::new("✗", Sty::Red),
  }
}

/// Build the clickable header row for proc `i`: `▼ ⠼ label  1.2s  note`, with the running
/// spinner glyph at `frame`. Full-width; [`fit_row`] clips it to the terminal in [`Model::layout`].
fn header_row_framed(i: usize, p: &Proc, frame: u64) -> Row {
  let triangle = if p.expanded { "▼ " } else { "▶ " };
  // A keyboard-shortcut hint: press the labelled digit or letter to toggle this row.
  // Rows 0–9 show [0]..[9]; rows 10–35 show [A]..[Z]. Rows beyond that are still clickable.
  let key = shortcut_label(i);
  let mut segs = vec![
    Seg::new(triangle, Sty::Dim),
    Seg::new(key, Sty::Dim),
    glyph(p.status, frame),
    Seg::new(" ", Sty::Plain),
    Seg::new(p.label.clone(), Sty::Bold),
    Seg::new("  ", Sty::Plain),
    Seg::new(format_elapsed(p.elapsed), Sty::Dim),
  ];
  // Trailing note while running, or the final detail once done (red if it failed).
  let tail = match p.status {
    Status::Ok | Status::Fail => p.detail.as_deref(),
    _ => p.note.as_deref(),
  };
  if let Some(t) = tail.filter(|t| !t.is_empty()) {
    let sty = if p.status == Status::Fail { Sty::Red } else { Sty::Dim };
    segs.push(Seg::new("  ", Sty::Plain));
    segs.push(Seg::new(t.to_string(), sty));
  }
  Row { proc: Some(i), segs }
}

/// Build one expanded output row: `    +1.2s  the line`. Full-width; [`fit_row`] clips it.
fn output_row(line: &Line) -> Row {
  Row {
    proc: None,
    segs: vec![
      Seg::new("    ", Sty::Plain),
      Seg::new(format!("+{}", format_elapsed(line.at)), Sty::Dim),
      Seg::new("  ", Sty::Plain),
      Seg::new(line.text.clone(), Sty::Plain),
    ],
  }
}

/// Clip a row's segments so its total display width never exceeds `width` (rows never wrap):
/// keep whole segments while they fit, clip the one that straddles the edge (with an ellipsis),
/// and drop the rest.
fn fit_row(row: Row, width: usize) -> Row {
  let mut out = Vec::new();
  let mut used = 0usize;
  for seg in row.segs {
    if used >= width {
      break;
    }
    let w = display_width(&seg.text);
    if used + w <= width {
      used += w;
      out.push(seg);
    } else {
      let clipped = clip(&seg.text, width - used);
      if !clipped.is_empty() {
        out.push(Seg { text: clipped, sty: seg.sty });
      }
      break;
    }
  }
  Row { proc: row.proc, segs: out }
}

/// Display width of a string (Unicode-aware, ANSI already stripped upstream by `clean_line`).
fn display_width(s: &str) -> usize {
  console::measure_text_width(s)
}

/// Clip `s` to at most `max` display columns, adding an ellipsis when it overflows.
fn clip(s: &str, max: usize) -> String {
  if max == 0 {
    return String::new();
  }
  console::truncate_str(s, max, "…").into_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn demo_model() -> Model {
    let mut m = Model::new();
    let b = m.add("build");
    m.set_status(b, Status::Ok);
    m.set_elapsed(b, 4.0);
    m.push_line(b, 0.5, "STEP 1/3");
    m.push_line(b, 3.9, "done");
    let s = m.add("opencode: add");
    m.set_status(s, Status::Running);
    m.set_elapsed(s, 1.2);
    m.set_note(s, Some("2 + 3 = 5".into()));
    m.push_line(s, 1.0, "running add");
    m
  }

  #[test]
  fn collapsed_layout_is_one_row_per_proc() {
    let m = demo_model();
    let rows = m.layout(80, 0);
    assert_eq!(rows.len(), 2, "collapsed: one header each");
    assert_eq!(rows[0].proc, Some(0));
    assert_eq!(rows[1].proc, Some(1));
    // The collapsed header carries the triangle, glyph, label and clock.
    assert!(rows[0].plain().starts_with("▶ [0] "), "collapsed triangle + shortcut: {:?}", rows[0].plain());
    assert!(rows[0].plain().contains("build"));
    assert!(rows[0].plain().contains("4s"));
  }

  #[test]
  fn expanding_reveals_timestamped_output() {
    let mut m = demo_model();
    m.toggle(0);
    let rows = m.layout(80, 0);
    // header + 2 output lines + the second proc's header.
    assert_eq!(rows.len(), 4);
    assert!(rows[0].plain().starts_with("▼ [0] "), "expanded triangle + shortcut");
    assert_eq!(rows[1].proc, None, "output line is not a click target");
    assert!(rows[1].plain().contains("+0.5s") && rows[1].plain().contains("STEP 1/3"), "{:?}", rows[1].plain());
    assert!(rows[2].plain().contains("+3s") && rows[2].plain().contains("done"), "{:?}", rows[2].plain());
  }

  #[test]
  fn expanded_with_no_output_shows_a_placeholder() {
    let mut m = Model::new();
    let i = m.add("empty");
    m.toggle(i);
    let rows = m.layout(80, 0);
    assert_eq!(rows.len(), 2);
    assert!(rows[1].plain().contains("no output"), "{:?}", rows[1].plain());
  }

  #[test]
  fn running_glyph_animates_with_the_frame() {
    let m = demo_model();
    // segs: [0] = triangle, [1] = the [N] shortcut, [2] = the animated status glyph.
    let g0 = m.layout(80, 0)[1].segs[2].text.clone();
    let g1 = m.layout(80, 1)[1].segs[2].text.clone();
    assert_ne!(g0, g1, "the running spinner glyph should advance with the frame");
    assert!(FRAMES.contains(&g0.as_str()) && FRAMES.contains(&g1.as_str()));
  }

  #[test]
  fn running_shows_note_finished_shows_detail() {
    let mut m = Model::new();
    let i = m.add("s");
    m.set_status(i, Status::Running);
    m.set_note(i, Some("latest line".into()));
    m.set_detail(i, Some("final detail".into()));
    assert!(m.layout(80, 0)[0].plain().contains("latest line"), "running uses note");
    m.set_status(i, Status::Ok);
    assert!(m.layout(80, 0)[0].plain().contains("final detail"), "finished uses detail");
  }

  #[test]
  fn rows_never_exceed_the_width() {
    let mut m = Model::new();
    let i = m.add("a-very-long-skill-label-that-keeps-going-and-going");
    m.set_status(i, Status::Running);
    m.set_note(i, Some("an extremely long trailing note ".repeat(20)));
    m.toggle(i);
    m.push_line(i, 1.0, "x".repeat(500));
    for w in [10usize, 20, 40, 80] {
      for row in m.layout(w, 0) {
        assert!(display_width(&row.plain()) <= w, "row '{}' exceeds width {w}", row.plain());
      }
    }
  }

  #[test]
  fn view_follows_the_tail_then_honors_scroll() {
    let mut m = Model::new();
    let i = m.add("s");
    m.toggle(i);
    for n in 0..20 {
      m.push_line(i, n as f64, format!("line {n}"));
    }
    let total = m.total_rows(80);
    assert_eq!(total, 21, "1 header + 20 lines");

    // Expand scrolls to the proc header (top of its block).
    let (vis, off) = m.view(80, 5, 0);
    assert_eq!(vis.len(), 5);
    assert_eq!(off, 0);
    assert!(vis[1].plain().contains("line 0"));

    // Scroll down through the output.
    m.scroll_by(100, 80, 5);
    let (_, off2) = m.view(80, 5, 0);
    assert_eq!(off2, total - 5);
    assert!(m.view(80, 5, 0).0.last().unwrap().plain().contains("line 19"));

    // Scrolling back to the bottom resumes global following.
    m.scroll_by(100, 80, 5);
    assert_eq!(m.view(80, 5, 0).1, total - 5);
  }

  #[test]
  fn expanded_proc_opens_at_its_header_not_the_fleet_tail() {
    let mut m = Model::new();
    let a = m.add("skill-a");
    let _b = m.add("skill-b");
    m.toggle(a);
    for n in 0..20 {
      m.push_line(a, n as f64, format!("a-{n}"));
    }
    let total = m.total_rows(80);
    assert_eq!(total, 22, "a: header+20 lines, b: header");

    let (vis, off) = m.view(80, 5, 0);
    assert_eq!(off, 0, "proc a starts at row 0");
    assert!(vis[0].plain().contains("skill-a"));
    assert!(vis[1].plain().contains("a-0"));
    assert!(!vis.iter().any(|r| r.plain().contains("a-19")));
  }

  #[test]
  fn new_lines_do_not_move_viewport_while_reading_from_the_top() {
    let mut m = Model::new();
    let a = m.add("skill-a");
    let _b = m.add("skill-b");
    m.toggle(a);
    for n in 0..10 {
      m.push_line(a, n as f64, format!("a-{n}"));
    }
    let (vis, off) = m.view(80, 5, 0);
    assert!(vis.iter().any(|r| r.plain().contains("a-0")));

    m.push_line(a, 11.0, "a-10");
    m.push_line(a, 12.0, "a-11");
    let (vis2, off2) = m.view(80, 5, 0);
    assert_eq!(off2, off);
    assert!(vis2.iter().any(|r| r.plain().contains("a-0")));
    assert!(!vis2.iter().any(|r| r.plain().contains("a-11")));
  }

  #[test]
  fn expand_later_proc_scrolls_to_its_header() {
    let mut m = Model::new();
    for _ in 0..6 {
      m.add("header-only");
    }
    let target = m.add("skill-f");
    m.toggle(target);
    for n in 0..5 {
      m.push_line(target, n as f64, format!("f-{n}"));
    }
    let (vis, off) = m.view(80, 5, 0);
    assert_eq!(off, 6, "six collapsed headers precede skill-f");
    assert!(vis[0].plain().contains("skill-f"));
    assert!(vis.get(1).is_some_and(|r| r.plain().contains("f-0")));
  }

  #[test]
  fn scroll_to_top_and_bottom() {
    let mut m = Model::new();
    let i = m.add("s");
    m.toggle(i);
    for n in 0..30 {
      m.push_line(i, n as f64, format!("l{n}"));
    }
    m.scroll_to_top();
    assert_eq!(m.view(80, 5, 0).1, 0);
    m.scroll_to_bottom();
    assert_eq!(m.view(80, 5, 0).1, m.total_rows(80) - 5);
  }

  #[test]
  fn shortcut_labels_cover_many_procs() {
    assert_eq!(shortcut_label(0), "[0] ");
    assert_eq!(shortcut_label(9), "[9] ");
    assert_eq!(shortcut_label(10), "[A] ");
    assert_eq!(shortcut_label(15), "[F] ");
    assert_eq!(proc_index_from_key('0'), Some(0));
    assert_eq!(proc_index_from_key('5'), Some(5));
    assert_eq!(proc_index_from_key('a'), Some(10));
    assert_eq!(proc_index_from_key('F'), Some(15));
  }

  #[test]
  fn toggle_flips_only_the_target_proc() {
    let mut m = demo_model();
    assert!(!m.procs[0].expanded && !m.procs[1].expanded);
    m.toggle(1);
    assert!(!m.procs[0].expanded && m.procs[1].expanded);
    m.set_all_expanded(true);
    assert!(m.procs[0].expanded && m.procs[1].expanded);
    m.set_all_expanded(false);
    assert!(!m.procs[0].expanded && !m.procs[1].expanded);
  }
}
