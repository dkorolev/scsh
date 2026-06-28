//! A self-contained demo of the live board, behind the hidden `scsh __ui-demo` command — it
//! needs no container runtime or model, so it's the runnable demo for the feature and the target
//! of the PTY test.
//!
//! * `scsh __ui-demo --frames` dumps a few **deterministic plain frames** (collapsed, expanded,
//!   and a scrolled window) — a CI-runnable proof of the layout, the ▶/▼ triangles, the
//!   per-line `+<elapsed>` stamps, and scrolling, with no terminal needed.
//! * `scsh __ui-demo` runs the **real interactive board** over a few scripted `sh` subprocesses,
//!   so a human (or the PTY harness) can click rows open/closed and scroll a genuine run.

use super::live::{Model, Row, Status};
use super::screen::LiveUi;

/// Entry point for `scsh __ui-demo [--frames]`.
pub fn run(frames: bool) -> i32 {
  if frames {
    frames_demo()
  } else {
    live_demo()
  }
}

/// Print a row's plain (unstyled) text — what the driver would render, minus the colour.
fn print_rows(rows: &[Row]) {
  for r in rows {
    println!("{}", r.plain());
  }
}

/// Deterministic, no-TTY proof: build a model by hand and dump three frames.
fn frames_demo() -> i32 {
  let width = 64;

  let mut m = Model::new();
  let b = m.add("using podman · build");
  m.set_status(b, Status::Ok);
  m.set_elapsed(b, 4.0);
  m.push_line(b, 0.3, "STEP 1/3 : FROM debian:bookworm-slim");
  m.push_line(b, 2.1, "STEP 2/3 : apt-get install toolchain");
  m.push_line(b, 3.8, "STEP 3/3 : done");
  let a = m.add("opencode: add");
  m.set_status(a, Status::Running);
  m.set_elapsed(a, 1.6);
  m.set_note(a, Some("2 + 3 = 5".into()));
  m.push_line(a, 0.2, "cloning…");
  m.push_line(a, 0.9, "$ python3 scripts/add.py");
  m.push_line(a, 1.5, "2 + 3 = 5");
  let mul = m.add("opencode: multiply");
  m.set_status(mul, Status::Fail);
  m.set_detail(mul, Some("X is required".into()));

  println!("=== FRAME 1 — collapsed (one row per proc; press 0/1/… — or click — to open) ===");
  print_rows(&m.layout(width, 0));

  println!();
  println!("=== FRAME 2 — build + add expanded (▼; each output line carries a +<elapsed> stamp) ===");
  m.toggle(b);
  m.toggle(a);
  print_rows(&m.layout(width, 0));

  println!();
  println!("=== FRAME 3 — scrolling: a long expanded proc in a 6-row window (from the top) ===");
  let mut m2 = Model::new();
  let s = m2.add("opencode: review");
  m2.set_status(s, Status::Running);
  m2.set_elapsed(s, 6.0);
  m2.toggle(s);
  for n in 1..=12 {
    m2.push_line(s, n as f64 * 0.5, format!("scanning file {n}"));
  }
  let (vis, off) = m2.view(width, 6, 0);
  println!("(showing rows {}..{} of {} — expand opens here; scroll down for the rest)", off, off + vis.len(), m2.total_rows(width));
  print_rows(&vis);
  0
}

/// `sh -c <script>` argv.
fn sh(script: &str) -> Vec<String> {
  vec!["-c".into(), script.into()]
}

/// The real interactive board over scripted subprocesses — for a human or the PTY harness.
fn live_demo() -> i32 {
  super::signals::install();
  let ui = LiveUi::new(console::user_attended_stderr());

  let build = ui.proc("using demo · build", true);
  build.start();
  let ok = build
    .run("sh", &sh("echo 'STEP 1/3 : FROM debian'; sleep 0.6; echo 'STEP 2/3 : apt-get install'; sleep 0.6; echo 'STEP 3/3 : done'; sleep 0.4"))
    .map(|(ok, _)| ok)
    .unwrap_or(false);
  if !ok {
    build.finish_fail(Some("build failed"));
    ui.finish();
    return 1;
  }
  build.finish_ok(None);

  std::thread::scope(|scope| {
    let add = ui.proc("opencode: add", false);
    let mul = ui.proc("opencode: multiply", false);
    scope.spawn(move || {
      add.start();
      let _ = add.run("sh", &sh("for i in 1 2 3 4 5 6; do echo \"add: working step $i\"; sleep 0.5; done"));
      add.finish_ok(Some("2 + 3 = 5"));
    });
    scope.spawn(move || {
      mul.start();
      let _ =
        mul.run("sh", &sh("for i in 1 2 3 4; do echo \"multiply: scanning $i\"; sleep 0.6; done; echo 'boom' 1>&2"));
      mul.finish_fail(Some("did not produce its result file"));
    });
  });

  ui.finish();
  println!("demo complete — that was the live board (click rows, scroll; Ctrl-C aborts).");
  0
}
