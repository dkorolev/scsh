use super::cast::cast_player_page;
use super::client_js::live_client_js;
use super::escape::esc;
use super::proc::{empty_output_html, empty_output_label};
use super::session::session_page;
use crate::daemon::model::{DaemonMode, ProcKind, ProcRecord, ProcStatus, Session, Store};

/// A one-proc store for the cast player page tests: the proc has a registered cast and
/// the given status.
fn store_with_cast_proc(status: ProcStatus) -> Store {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "castab".into(),
    Session {
      id: "castab".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("default".into()),
      kind: None,
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 1,
      client_connected: true,
      run_pid: None,
      skills: vec![],
      procs: vec![ProcRecord {
        index: 0,
        kind: ProcKind::Skill,
        label: "claude: add".into(),
        status,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: None,
        cast_path: Some("/tmp/x.cast".into()),
        harness: Some("claude".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: None,
        lines: vec![],
      }],
    },
  );
  store
}

fn session_procs_html(html: &str) -> &str {
  let needle = r#"<div class="procs" id="session-procs">"#;
  let start = html.find(needle).expect("session-procs") + needle.len();
  let tail = &html[start..];
  let end = tail.find(r#"<p class="permalink">"#).expect("permalink");
  &tail[..end]
}

#[test]
fn esc_handles_basic_html() {
  assert_eq!(esc("<a>"), "&lt;a&gt;");
}

#[test]
fn browser_player_is_first_party_and_carries_no_third_party_license() {
  // The whole point of scsh-cast-player: the session browser ships NO third-party code.
  // (Exported .html pages still embed asciinema-player via beecast-page — that attribution
  // is pinned separately in tests/cli.rs.)
  let js = super::PLAYER_JS;
  let css = super::PLAYER_CSS;
  assert!(js.contains("ScshCastPlayer"), "the first-party player global must be defined");
  assert!(js.contains("ScshVT"), "the DOM-free core must be bundled first");
  assert!(js.contains("Clean-room implementation"), "the clean-room statement rides in the asset");
  for banned in ["asciinema-player", "AsciinemaPlayer", "@license", "Apache"] {
    assert!(!js.contains(banned), "browser player JS must not carry '{banned}'");
    assert!(!css.contains(banned), "browser player CSS must not carry '{banned}'");
  }
}

/// Run the DOM-free VT core's behavior tests under Node (parsing all three asciicast
/// versions plus the terminal state machine). Skips silently when `node` is not on PATH —
/// the Rust-side structural tests above still gate the asset itself.
#[test]
fn vt_core_node_selftest() {
  if crate::runtime::which("node").is_none() {
    return;
  }
  let dir = std::env::temp_dir().join(format!("scsh-vt-selftest-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let bundle = dir.join("player.js");
  std::fs::write(&bundle, super::PLAYER_JS).unwrap();
  let script = format!(
    r#"
const assert = require('assert');
require({bundle:?});
const VT = globalThis.ScshVT;

// v3: intervals sum; term size from header; resize + marker events survive; # comments skip.
let c = VT.parseCast('{{"version":3,"term":{{"cols":10,"rows":3}}}}\n# note\n[0.5,"o","hi"]\n[0.5,"m","chapter"]\n[1.0,"r","20x5"]\n');
assert.strictEqual(c.cols, 10); assert.strictEqual(c.rows, 3);
assert.strictEqual(c.events.length, 3);
assert.strictEqual(c.duration, 2);
assert.strictEqual(c.events[2].t, 2);

// v2: absolute times.
c = VT.parseCast('{{"version":2,"width":80,"height":24}}\n[0.5,"o","a"]\n[2.0,"o","b"]\n');
assert.strictEqual(c.duration, 2); assert.strictEqual(c.events[1].t, 2);

// v1: one JSON doc, stdout deltas.
c = VT.parseCast('{{"version":1,"width":5,"height":2,"stdout":[[0.1,"x"],[0.2,"y"]]}}');
assert.strictEqual(c.cols, 5); assert.strictEqual(c.events.length, 2);
assert(Math.abs(c.duration - 0.3) < 1e-9);

// Plain text + CR/LF.
let t = new VT.Term(10, 3);
t.write('hello\r\nworld');
assert.deepStrictEqual(t.textLines(), ['hello', 'world', '']);

// CUP + overwrite mid-screen.
t.write('\x1b[1;3Hga');
assert.strictEqual(t.textLines()[0], 'hegao');

// ED 2 clears everything.
t.write('\x1b[2J');
assert.deepStrictEqual(t.textLines(), ['', '', '']);

// SGR runs merge; colors land on cells.
t = new VT.Term(10, 1);
t.write('\x1b[31mred\x1b[0m ok');
const runs = t.snapshot().rows[0];
assert.strictEqual(runs[0].text, 'red'); assert.strictEqual(runs[0].fg, 1);
assert.strictEqual(runs[1].fg, null);

// 256-color + truecolor.
t = new VT.Term(4, 1);
t.write('\x1b[38;5;196mX\x1b[38;2;1;2;3mY');
const r2 = t.snapshot().rows[0];
assert.strictEqual(r2[0].fg, 196);
assert.strictEqual(r2[1].fg, '#010203');
assert.strictEqual(VT.color256(196), '#ff0000');
assert.strictEqual(VT.color256(232), '#080808');

// Deferred wrap: printing in the last column does not wrap until the next char.
t = new VT.Term(3, 2);
t.write('abc');
assert.strictEqual(t.snapshot().cursor.y, 0);
t.write('d');
assert.deepStrictEqual(t.textLines(), ['abc', 'd']);

// Scroll region: LF at the region bottom scrolls only the region.
t = new VT.Term(5, 4);
t.write('aa\r\nbb\r\ncc\r\ndd');
t.write('\x1b[2;3r\x1b[3;1H\n');
const lines = t.textLines();
assert.strictEqual(lines[0], 'aa');
assert.strictEqual(lines[1], 'cc');
assert.strictEqual(lines[3], 'dd');

// Alternate screen: primary content comes back on exit.
t = new VT.Term(5, 2);
t.write('main');
t.write('\x1b[?1049h\x1b[Halt');
assert.strictEqual(t.textLines()[0], 'alt');
t.write('\x1b[?1049l');
assert.strictEqual(t.textLines()[0], 'main');

// DEC special graphics: tmux border characters.
t = new VT.Term(4, 1);
t.write('\x1b(0qqx\x1b(B');
assert.strictEqual(t.textLines()[0], '──│');

// Cursor hide/show.
t = new VT.Term(2, 1);
t.write('\x1b[?25l');
assert.strictEqual(t.snapshot().cursor.visible, false);
t.write('\x1b[?25h');
assert.strictEqual(t.snapshot().cursor.visible, true);

// OSC titles are consumed, never printed.
t = new VT.Term(8, 1);
t.write('\x1b]0;title\x07ok');
assert.strictEqual(t.textLines()[0], 'ok');

console.log('vt selftest OK');
"#,
    bundle = bundle
  );
  let out = std::process::Command::new("node")
    .arg("-")
    .arg("--input-type=commonjs")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .and_then(|mut child| {
      use std::io::Write;
      child.stdin.take().unwrap().write_all(script.as_bytes())?;
      child.wait_with_output()
    })
    .expect("node runs");
  let _ = std::fs::remove_dir_all(&dir);
  assert!(
    out.status.success() && String::from_utf8_lossy(&out.stdout).contains("vt selftest OK"),
    "vt selftest failed:\nstdout: {}\nstderr: {}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
}

#[test]
fn skipped_workflow_step_renders_as_a_dim_slashed_row() {
  let mut store = store_with_cast_proc(ProcStatus::Skipped);
  {
    let p = &mut store.sessions.get_mut("castab").unwrap().procs[0];
    p.cast_path = None; // a skipped step never ran, so it has no recording
    p.detail = Some("skipped — its when: gate is false".into());
    p.note = Some("step 2/2 · needs probe_credentials".into());
  }
  let html = session_page(&store, "castab").expect("session renders");
  let procs = session_procs_html(&html);
  assert!(procs.contains(r#"class="proc skipped""#), "got: {procs}");
  assert!(procs.contains("⊘"), "skipped glyph: {procs}");
  // A skipped step is FINISHED, so its collapsed row shows the outcome (the skip reason),
  // not the transient step note — same rule that puts a finished skill's answer in the row.
  assert!(procs.contains(r#"<span class="note dim">skipped — its when: gate is false</span>"#), "skip reason in the collapsed row: {procs}");
  assert!(!procs.contains("data-proc-stop"), "a skipped step offers no kill button: {procs}");
  // The client knows the glyph too (live updates keep ⊘ when a tick arrives).
  let js = live_client_js();
  assert!(js.contains("skipped:'⊘'"));
}

#[test]
fn start_panel_offers_project_creation_and_the_client_wires_it() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  for id in ["project-name", "project-create"] {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  assert!(html.contains("~/.scsh/projects/"), "the panel explains where projects live");
  let js = live_client_js();
  assert!(js.contains("/api/v1/projects/create"), "client js posts project creation");
  assert!(js.contains("function createProject"), "client js wires the button");
  assert!(js.contains("function handleRepoOpened"), "open and create share the response path");
}

#[test]
fn running_cast_preview_starts_near_the_end() {
  let js = live_client_js();
  // A still-running proc's player opens ~3s before the current tail (autoplaying), not at 0.
  assert!(js.contains("const LIVE_PREVIEW_TAIL_SECS = 3"), "tail preview window constant");
  assert!(js.contains("createCastPlayer(box, 'near-end', true)"), "running casts open near the end");
  assert!(js.contains("stats.duration - LIVE_PREVIEW_TAIL_SECS"), "near-end resolves against the loaded duration");
}

#[test]
fn ui_review_fixes_hold() {
  // 1. Agent-route badges: the chamfer overlay must not swallow the text (the
  //    empty-rectangle bug — .agent-badge's inner span needs the z-index lift too).
  let html = super::index_page(&Store::new(DaemonMode::Persistent, 7274, 1));
  assert!(
    html.contains(".badge > span, .session-status > span, .agent-badge > span"),
    "agent-badge text must sit above the chamfer overlay"
  );
  // 2. Clicking something that renders inputs further down scrolls there.
  let js = live_client_js();
  assert_eq!(js.matches("scrollIntoView").count() >= 2, true, "def form + defs panel scroll into view");
  // 3. A finished proc's collapsed row shows its ANSWER, not the stale run note.
  let mut store = store_with_cast_proc(ProcStatus::Ok);
  {
    let p = &mut store.sessions.get_mut("castab").unwrap().procs[0];
    p.detail = Some("2 + 3 = 5".into());
    p.note = Some("claude run…".into());
  }
  let page = session_page(&store, "castab").expect("session renders");
  assert!(page.contains(r#"<span class="note dim">2 + 3 = 5</span>"#), "the answer rides the collapsed row");
  assert!(!page.contains(r#"<span class="note dim">claude run…</span>"#), "the stale note does not");
  // 4. The meta island is purple and owns the action buttons (top-right corner).
  assert!(page.contains(r#"<div class="card card--accent-left-purple"><div class="session-actions">"#));
  // 5. Proc islands wear their status color.
  assert!(html.contains("details.proc.running summary .glyph, details.proc.running summary .label"));
  // 6. The builtin source badge wears purple.
  assert!(html.contains(".badge--purple"), "purple badge class ships");
  assert!(live_client_js().contains(r#"chamfer badge badge--purple"><span>builtin"#), "builtin badge is purple");
}

#[test]
fn session_header_carries_breadcrumbs_and_honest_kind() {
  // The top island: location path on the left (bold, plain text), daemon status right.
  let mut store = store_with_cast_proc(ProcStatus::Running);
  {
    let s = store.sessions.get_mut("castab").unwrap();
    s.kind = Some("workflow".into());
    s.profile = Some("arith".into());
  }
  let html = session_page(&store, "castab").expect("session renders");
  assert!(
    html.contains(r#"<a href="/">scsh</a><span class="crumb-sep">›</span><a href="/">sessions</a><span class="crumb-sep">›</span><a href="/session/castab">castab</a>"#),
    "breadcrumb permalinks in the top island"
  );
  // The status dot sits at the very RIGHT edge of the island.
  assert!(html.contains(r#"{}<span class="dot" aria-hidden="true"></span></span></div>"#.trim_start_matches("{}")), "dot last in the island");
  assert!(html.contains(r#"<span class="daemon-right">"#), "daemon status keeps the island's right side");
  assert!(!html.contains("<h1>"), "the body no longer duplicates the path as an h1");
  // A workflow session says so — not "profile".
  assert!(html.contains(r#"<p class="subtitle">workflow <strong>arith</strong></p>"#), "got: {html}");
  // A session with no kind (persisted by an older build) still reads as a profile.
  let mut old = store_with_cast_proc(ProcStatus::Running);
  old.sessions.get_mut("castab").unwrap().profile = Some("default".into());
  let html = session_page(&old, "castab").expect("session renders");
  assert!(html.contains(r#"<p class="subtitle">profile <strong>default</strong></p>"#), "got: {html}");
  // The index island shows just "scsh".
  let html = super::index_page(&store);
  assert!(html.contains(r#"<span class="crumbs"><a href="/">scsh</a></span>"#), "got crumbs on index");
}

#[test]
fn stop_strip_and_kill_buttons_ignore_zombie_sessions() {
  // A dead client's session stays un-ended with "running" procs forever. It must get NO
  // stop-all-harness button on the index and NO per-proc kill button on its session page —
  // there is nothing left to stop. (store_with_cast_proc's session was last seen at t=1.)
  let store = store_with_cast_proc(ProcStatus::Running);
  let html = super::index_page(&store);
  // (The embedded client JS always contains the bare attribute selectors, so assert on the
  // rendered `attr="` form, which only a server-side button carries.)
  assert!(!html.contains(r#"data-harness-stop=""#), "zombie sessions must not raise stop-all buttons");
  let page = session_page(&store, "castab").expect("session renders");
  assert!(!page.contains(r#"data-proc-stop=""#), "zombie sessions must not offer per-proc kill");

  // The same session, seen moments ago, gets both.
  let mut live = store_with_cast_proc(ProcStatus::Running);
  live.sessions.get_mut("castab").unwrap().last_seen_at = crate::daemon::paths::now_unix_secs();
  let html = super::index_page(&live);
  assert!(html.contains(r#"data-harness-stop="claude""#), "live sessions raise the stop-all button");
  let page = session_page(&live, "castab").expect("session renders");
  assert!(page.contains(r#"data-proc-stop="0""#), "live sessions offer per-proc kill");
}

#[test]
fn index_page_shows_colored_harness_chips_per_proc() {
  let mut store = store_with_cast_proc(ProcStatus::Running);
  // A second, finished proc on another harness: its chip renders dimmed.
  {
    let session = store.sessions.get_mut("castab").unwrap();
    let mut done = session.procs[0].clone();
    done.index = 1;
    done.status = ProcStatus::Ok;
    done.harness = Some("grok".into());
    done.label = "grok: add".into();
    session.procs.push(done);
    // Build procs never get a chip — only skill runs count.
    let mut build = session.procs[0].clone();
    build.index = 2;
    build.kind = ProcKind::Build;
    build.harness = Some("codex".into());
    session.procs.push(build);
  }
  let html = super::index_page(&store);
  assert!(html.contains(r#"<span class="hchip hchip--claude" title="claude: add (running)">C</span>"#), "got: {html}");
  assert!(html.contains(r#"<span class="hchip hchip--grok hchip--done" title="grok: add (ok)">G</span>"#), "got: {html}");
  assert!(!html.contains(r#"class="hchip hchip--codex"#), "build procs must not render a chip");
  // The stylesheet distinguishes the same letter by harness color, and the client JS
  // mirrors the markup for live re-renders.
  assert!(html.contains(".hchip--claude"));
  assert!(html.contains(".hchip--codex"));
  assert!(html.contains("function harnessChipsHtml"));
}

#[test]
fn index_page_carries_the_images_panel_and_its_client_wiring() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  // The panel skeleton: status table body plus every control the client script binds to.
  for id in ["images-body", "images-build-selected", "images-build-all", "images-rebuild-base", "images-force"] {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  // First paint already lists every known image (§13: no empty limbo while inspect runs).
  assert!(html.contains("checking…"), "skeleton rows start in checking…");
  assert!(html.contains("scsh-base:latest"), "base image row on first paint");
  for tag in ["scsh-opencode:latest", "scsh-claude:latest", "scsh-codex:latest", "scsh-grok:latest", "scsh-cursor:latest"] {
    assert!(html.contains(tag), "harness image {tag} on first paint");
  }
  assert!(html.contains("checking container runtime…"), "note explains the pending inspect");
  // The embedded client script populates the panel from the images API without blanking rows.
  let js = live_client_js();
  assert!(js.contains("/api/v1/images"), "client js should fetch the images API");
  assert!(js.contains("/api/v1/images/build"), "client js should post builds");
  assert!(js.contains("function markImagesChecking"), "refresh keeps rows visible while checking");
  assert!(!js.contains("loading…"), "must not replace the table with a blank loading row");
  // Each image row carries its own [re]build button (base row rebuilds base + everything).
  assert!(js.contains("data-image-build"), "per-row build buttons are rendered");
  assert!(js.contains("function startImageBuildOne"), "per-row build buttons are wired");
  assert!(html.contains("image-action-cell"), "skeleton rows reserve the per-row action cell");
}

#[test]
fn index_page_carries_the_repositories_panel_and_its_client_wiring() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  for id in ["repo-path", "repo-pick", "repo-open", "repo-blockers", "defs-panel", "defs-list", "def-form", "repos-body"] {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  // The four tabs, and their panels.
  for (tab, panel) in [("jobs", "tab-jobs"), ("dirs", "tab-dirs"), ("start", "tab-start"), ("images", "tab-images")] {
    assert!(html.contains(&format!("data-tab=\"{tab}\"")), "index page should have the {tab} tab");
    assert!(html.contains(&format!("id=\"{panel}\"")), "index page should have panel #{panel}");
  }
  let js = live_client_js();
  assert!(js.contains("/api/v1/repos/open"), "client js opens a repo");
  assert!(js.contains("/api/v1/repos/pick"), "client js pops the folder picker");
  assert!(js.contains("/api/v1/jobs/start"), "client js starts a job");
  assert!(js.contains("function renderRepoJobs"), "client js renders jobs by repository");
  assert!(js.contains("OPEN_REPO_RUNNABLE"), "client js gates Start on the repo being runnable");
  assert!(js.contains("function initTabs"), "client js wires the tabs");
}

#[test]
fn empty_output_label_depends_on_proc_status() {
  assert_eq!(empty_output_label(ProcStatus::Running), "No output yet.");
  assert_eq!(empty_output_label(ProcStatus::Waiting), "No output yet.");
  assert_eq!(empty_output_label(ProcStatus::Ok), "No output.");
  assert_eq!(empty_output_label(ProcStatus::Fail), "No output.");
}

#[test]
fn session_proc_html_has_no_stray_backslashes() {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "test".into(),
    Session {
      id: "test".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("default".into()),
      kind: None,
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: crate::daemon::paths::now_unix_secs(), // live: Force stop only renders for running sessions
      client_connected: false,
      run_pid: None,
      skills: vec![],
      procs: vec![ProcRecord {
        index: 0,
        kind: ProcKind::Skill,
        label: "opencode: add".into(),
        status: ProcStatus::Running,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: None,
        cast_path: None,
        harness: Some("opencode".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: None,
        lines: vec![],
      }],
    },
  );
  let html = session_page(&store, "test").expect("session page");
  let procs = session_procs_html(&html);
  assert!(!html.contains("\\\n"), "raw-string line continuations must not leak backslashes");
  assert!(!procs.contains("\\\n"), "autoscroll markup must not leak backslashes");
  assert!(procs.contains(r#"<label class="autoscroll-ctl">"#));
  assert!(procs.contains("Auto-scroll to bottom"));
  assert!(html.contains(r#"<div class="output"><div class="dim">No output yet.</div>"#));
  assert!(html.contains(r#"id="session-stop""#), "running session should offer Force stop");
  assert!(html.contains("Force stop"));
}

#[test]
fn ended_session_hides_force_stop_button() {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "done01".into(),
    Session {
      id: "done01".into(),
      started_at: 1,
      ended_at: Some(10),
      profile: Some("default".into()),
      kind: None,
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 10,
      client_connected: false,
      run_pid: None,
      skills: vec![],
      procs: vec![],
    },
  );
  let html = session_page(&store, "done01").expect("session page");
  assert!(!html.contains(r#"id="session-stop""#), "ended session must not offer Force stop");
  // In its place: the resting lifecycle badge (an ended clean session reads "completed").
  assert!(html.contains(r#"session-status completed"#), "ended session shows the completed badge");
}

#[test]
fn client_js_wires_force_stop() {
  let js = live_client_js();
  assert!(js.contains("/api/v1/session/stop"), "client js posts session stop");
  assert!(js.contains("function forceStopSession"), "client js defines forceStopSession");
}

#[test]
fn recorded_proc_embeds_cast_player_instead_of_text_output() {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "castab".into(),
    Session {
      id: "castab".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("default".into()),
      kind: None,
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 1,
      client_connected: true,
      run_pid: None,
      skills: vec![],
      procs: vec![ProcRecord {
        index: 2,
        kind: ProcKind::Skill,
        label: "claude: add".into(),
        status: ProcStatus::Ok,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: None,
        cast_path: Some("/tmp/x.cast".into()),
        harness: Some("claude".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: Some(3.0),
        lines: vec![],
      }],
    },
  );
  let html = session_page(&store, "castab").expect("session page");
  // The page loads the player assets and embeds a player box wired to the cast endpoint.
  assert!(html.contains(r#"<link rel="stylesheet" href="/assets/scsh-cast-player.css">"#), "player css");
  assert!(html.contains(r#"<script src="/assets/scsh-cast-player.js"></script>"#), "player js");
  let procs = session_procs_html(&html);
  assert!(procs.contains(r#"<div class="cast" data-cast-url="/cast/castab/2""#), "cast embed");
  assert!(procs.contains("data-cast-fs"), "fullscreen button");
  assert!(procs.contains("data-cast-link"), "timestamp deep-link button");
  assert!(procs.contains(r#"<a href="/cast/castab/2?dl=1" download>"#), "download link");
  // A recorded proc shows the player, NOT the text output / autoscroll control.
  assert!(!procs.contains(r#"<div class="output">"#), "no text output for recorded proc");
  assert!(!procs.contains("autoscroll-ctl"), "no autoscroll control for recorded proc");
}

#[test]
fn empty_output_html_has_no_backslash_artifacts() {
  let html = empty_output_html(ProcStatus::Ok);
  assert_eq!(html, "<div class=\"dim\">No output.</div>\n");
  assert!(!html.contains("\\"));
  let running = empty_output_html(ProcStatus::Running);
  assert_eq!(running, "<div class=\"dim\">No output yet.</div>\n");
  assert!(!running.contains("\\"));
}

#[test]
fn session_proc_html_shows_autoscroll_while_running() {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "test".into(),
    Session {
      id: "test".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("default".into()),
      kind: None,
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 1,
      client_connected: true,
      run_pid: None,
      skills: vec![],
      procs: vec![ProcRecord {
        index: 0,
        kind: ProcKind::Skill,
        label: "opencode: add".into(),
        status: ProcStatus::Running,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: None,
        cast_path: None,
        harness: Some("opencode".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: None,
        lines: vec![],
      }],
    },
  );
  let html = session_page(&store, "test").expect("session page");
  let procs = session_procs_html(&html);
  assert!(procs.contains(r#"<label class="autoscroll-ctl">"#));
}

#[test]
fn empty_cast_shows_placeholder_instead_of_player_error() {
  // Both the session-page embed and the standalone player page fetch the cast text first
  // and render a calm placeholder when it has no complete event lines yet, instead of
  // handing the player an empty/404 cast (which errors).
  let js = live_client_js();
  assert!(js.contains("Recording in progress — no frames yet."));
  assert!(js.contains("No recorded frames."));
  assert!(js.contains("cast-placeholder"));
  assert!(js.contains("{ data: text }"), "player mounts over the already-fetched text");
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Running), "castab", 0).expect("player page");
  assert!(page.contains("Recording in progress — no frames yet."));
  assert!(page.contains("cast-placeholder"));
  assert!(page.contains("const LIVE = true;"));
  let done = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(done.contains("const LIVE = false;"));
}

#[test]
fn cast_growth_notifications_drive_the_reload_banner() {
  // The session page routes WS messages by type: cast_growth feeds the banner, everything
  // else stays on the tick path.
  let js = live_client_js();
  assert!(js.contains("if (msg.type === 'cast_growth') { onCastGrowth(msg); return; }"));
  assert!(js.contains("onWsMessage(JSON.parse(ev.data))"));
  assert!(js.contains("Recording grew: +"));
  assert!(js.contains("data-cast-grew"));
  // The standalone player page listens on its own WS connection — but only while the proc
  // runs, and it degrades to the manual reload button when the WS is unavailable.
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Running), "castab", 0).expect("player page");
  assert!(page.contains("'cast_growth'"));
  assert!(page.contains("const SESSION = 'castab';"));
  assert!(page.contains("const PROC = 0;"));
  assert!(page.contains(r#"<button id="grew" hidden></button>"#));
  assert!(page.contains("Recording grew: +"));
  assert!(page.contains("if (!castRunning) return;"), "no WS connect once the proc finished");
}

#[test]
fn live_toggle_renders_only_while_the_proc_runs() {
  // Session-page embed: the Live toggle is in the toolbar, hidden unless the proc runs.
  let running = super::proc::cast_embed_html("castab", &store_with_cast_proc(ProcStatus::Running).sessions["castab"].procs[0]);
  assert!(running.contains(r#"<button type="button" data-cast-live>● Live</button>"#));
  let done = super::proc::cast_embed_html("castab", &store_with_cast_proc(ProcStatus::Ok).sessions["castab"].procs[0]);
  assert!(done.contains(r#"<button type="button" data-cast-live hidden>● Live</button>"#));
  // The client JS follows the tail via cast_growth reloads (see the mechanism comment).
  let js = live_client_js();
  assert!(js.contains("function setCastLive(box, on)"));
  assert!(js.contains("if (box._live) { createCastPlayer(box, box._loadedDuration ?? 'end', true); return; }"));
  // Standalone page: toggle present while running, hidden for finished procs, and the
  // finish notice disables it after the final reload.
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Running), "castab", 0).expect("player page");
  assert!(page.contains(r#"<button id="live-toggle">● Live</button>"#));
  assert!(page.contains("toggle.disabled = true;"));
  let finished = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(finished.contains(r#"<button id="live-toggle" hidden>● Live</button>"#));
}

#[test]
fn export_html_download_renders_on_both_pages_and_hides_without_frames() {
  // Standalone player page: the download link points at the export endpoint, starts
  // hidden, and rides the same no-frames state as the placeholder.
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(page.contains(r#"<a id="dl-html" href="/cast/castab/0/export.html" download hidden>⬇ download .html</a>"#));
  assert!(page.contains("document.getElementById('dl-html').hidden = !stats.events;"));
  // Session-page embed: same link, same hide-until-frames wiring — both in the
  // server-rendered snippet and in the client JS that regenerates it.
  let session = session_page(&store_with_cast_proc(ProcStatus::Ok), "castab").expect("session page");
  let procs = session_procs_html(&session);
  assert!(procs.contains(r#"<a href="/cast/castab/0/export.html" data-cast-export download hidden>⬇ .html</a>"#));
  let js = live_client_js();
  assert!(js.contains("/export.html\" data-cast-export download hidden>⬇ .html</a>"));
  assert!(js.contains("exportLink.hidden = !stats.events;"));
}

#[test]
fn session_page_header_offers_the_session_export_download() {
  // A session with a recorded proc gets the whole-session download button in the header
  // (decided server-side: any proc with a registered cast; the endpoint 404s edge cases).
  let html = session_page(&store_with_cast_proc(ProcStatus::Ok), "castab").expect("session page");
  assert!(
    html.contains(r#"href="/session/castab/export.html" download"#) && html.contains("session-export"),
    "session export button"
  );
  // No recorded proc anywhere → no button (nothing to export; the 404 would only confuse).
  let mut store = store_with_cast_proc(ProcStatus::Ok);
  store.sessions.get_mut("castab").unwrap().procs[0].cast_path = None;
  let bare = session_page(&store, "castab").expect("session page");
  // (The `.session-export` CSS rule is in the shared shell, so match the anchor itself.)
  assert!(!bare.contains("<a class=\"session-export\""), "no export button without any registered cast");
}

#[test]
fn live_client_js_counts_alive_clients_and_shutdown() {
  let js = live_client_js();
  assert!(js.contains("alive_clients"));
  assert!(js.contains("shutting down in"));
}

#[test]
fn live_client_js_skips_index_render_without_sessions() {
  let js = live_client_js();
  assert!(js.contains("if (!body || sessions == null) return"));
  // renderIndex (and the jobs-per-repo view) run only when a snapshot is present.
  assert!(js.contains("if (snapshot) {"));
  assert!(js.contains("renderIndex(snapshot, nowUnix)"));
}

#[test]
fn live_client_js_shows_connecting_on_ws_close() {
  let js = live_client_js();
  assert!(js.contains("setDaemonStatus('connecting', 'connecting…', null)"));
  assert!(!js.contains("daemon unreachable"));
}

#[test]
fn wrap_page_connecting_status_uses_blue() {
  use super::layout::wrap_page;
  let html = wrap_page("scsh sessions", 7274, None, "<p>body</p>");
  assert!(html.contains("class=\"daemon-status connecting\""));
  assert!(html.contains(".daemon-status.connecting .dot { background: var(--cyan);"));
}

#[test]
fn every_daemon_page_carries_the_inline_favicon() {
  use super::layout::wrap_page;
  // A data: URI, so the dashboard and the standalone player page stay request-free.
  let html = wrap_page("scsh sessions", 7274, None, "<p>body</p>");
  assert!(html.contains("<link rel=\"icon\" href=\"data:image/svg+xml,"), "dashboard favicon");
  let player = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(player.contains("<link rel=\"icon\" href=\"data:image/svg+xml,"), "player-page favicon");
}

#[test]
fn wrap_page_serves_valid_css_braces() {
  use super::layout::wrap_page;
  let html = wrap_page("scsh sessions", 7274, None, "<p>body</p>");
  assert!(html.contains(":root {"));
  assert!(!html.contains(":root {{"));
  assert!(html.contains(".daemon-status {"));
}
