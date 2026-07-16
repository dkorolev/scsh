use super::cast::cast_player_page;
use super::client_js::live_client_js;
use super::escape::esc;
use super::session::session_page;
use super::session_export::session_export_page;
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
        container_runtime: None,
        cast_path: Some("/tmp/x.cast".into()),
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("claude".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: None,
        lines: vec![],
      }],
      workflow: None,
      parent_session: None,
    },
  );
  store
}

fn session_procs_html(html: &str) -> &str {
  let needle = r#"<div class="procs" id="session-procs">"#;
  let start = html.find(needle).expect("session-procs") + needle.len();
  let tail = &html[start..];
  // The procs div is the body's last element; the page footer is the script block.
  let end = tail.find("<script").expect("script block after procs");
  &tail[..end]
}

#[test]
fn esc_handles_basic_html() {
  assert_eq!(esc("<a>"), "&lt;a&gt;");
}

#[test]
fn running_container_names_its_runtime_without_guessing() {
  let mut store = store_with_cast_proc(ProcStatus::Running);
  let proc = &mut store.sessions.get_mut("castab").unwrap().procs[0];
  proc.container_name = Some("scsh-abcdef-run-add".into());
  proc.container_runtime = Some("container".into());
  let html = session_page(&store, "castab").unwrap();
  assert!(html.contains(r#"class="container-runtime-name">Apple Containers</span> · container: scsh-abcdef-run-add"#));
  assert!(html.contains("function containerRuntimeName"), "live updates must preserve the explicit runtime label");
  assert!(html.contains("Not recorded (legacy run)"), "old stored jobs must stay honest instead of guessing");
}

#[test]
fn browser_player_is_first_party_and_carries_no_third_party_license() {
  // The whole point of the first-party beecast-player: neither the session browser nor
  // the exported pages (same crate family) ship ANY third-party code.
  let js = super::PLAYER_JS;
  let css = super::PLAYER_CSS;
  assert!(js.contains("BeeCastPlayer"), "the first-party player global must be defined");
  assert!(js.contains("BeeCastVT"), "the DOM-free core must be bundled first");
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
const VT = globalThis.BeeCastVT;

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
  assert!(procs.contains(r#"class="chamfer proc skipped""#), "got: {procs}");
  assert!(!procs.contains("class=\"glyph\""), "proc rows no longer carry a status glyph: {procs}");
  assert!(procs.contains(">skipped</span>"), "skipped elapsed phrase: {procs}");
  // A skipped step is FINISHED, so its collapsed row shows the outcome (the skip reason),
  // not the transient step note — same rule that puts a finished skill's answer in the row.
  assert!(
    procs.contains(r#"<span class="note dim">skipped — its when: gate is false</span>"#),
    "skip reason in the collapsed row: {procs}"
  );
  assert!(!procs.contains("data-proc-stop"), "skipped step has no Force stop: {procs}");
  // Live updates speak the same phrases; workflow graph keeps its own icon map.
  let js = live_client_js();
  assert!(js.contains("function elapsedPhrase"));
  assert!(js.contains("'skipped'"));
  assert!(js.contains("function wfDisplayState"), "workflow graph live state");
}

#[test]
fn start_panel_offers_project_creation_and_the_client_wires_it() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  for id in ["project-name", "project-create"] {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  assert!(html.contains("~/.scsh/projects/"), "the panel explains where projects live");
  assert!(html.contains("start-controls"), "Run rows use start-controls for full-width layout");
  assert!(html.contains("start-actions"), "Run action buttons are grouped for right alignment");
  assert!(html.contains(".start-controls"), "start-controls CSS ships");
  assert!(html.contains(".start-actions"), "start-actions CSS ships");
  let js = live_client_js();
  assert!(js.contains("/api/v1/projects/create"), "client js posts project creation");
  assert!(js.contains("function createProject"), "client js wires the button");
  assert!(js.contains("function handleRepoOpened"), "open and create share the response path");
  assert!(js.contains("function showToast"), "invalid-name feedback is a toast");
  assert!(!js.contains("suggestOpenExistingProject"), "existing names open via 200 create-or-open, not a toast");
  assert!(js.contains("projectNameOk"), "client rejects dots/slashes before POST");
  assert!(html.contains("no dots/slashes"), "placeholder documents the name rules");
  assert!(html.contains(".toast"), "toast styles ship with the page");
  assert!(html.contains(r#"class="section-label">Definitions"#), "defs panel is labeled Definitions");
  assert!(js.contains("list.scrollIntoView"), "open/create scrolls the actionable definitions list into view");
  assert!(html.contains("#defs-list { padding-top: 1.25rem; }"), "the first definition keeps a blank top inset");
}

/// Syntax-check the whole live client script under Node. Catches redeclared `const`/`let`,
/// stray braces, dropped function headers, and other parse errors that would abort the
/// inline `<script>` and leave the session browser dead (e.g. `Identifier 'det' has already
/// been declared`). Skips silently when `node` is not on PATH — same pattern as the VT
/// selftest.
#[test]
fn live_client_js_parses_under_node() {
  if crate::runtime::which("node").is_none() {
    return;
  }
  let dir = std::env::temp_dir().join(format!("scsh-live-js-check-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let path = dir.join("live-client.js");
  std::fs::write(&path, live_client_js()).unwrap();
  let out = std::process::Command::new("node").arg("--check").arg(&path).output().expect("spawn node --check");
  assert!(
    out.status.success(),
    "live_client_js must parse: {}\n{}",
    String::from_utf8_lossy(&out.stderr),
    String::from_utf8_lossy(&out.stdout)
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn running_cast_preview_starts_near_the_end() {
  let js = live_client_js();
  // A still-running proc's player opens in DECLARED-LIVE mode: parked at the growing edge,
  // the seek bar pinned full-width in live green (player.setLive) — not a near-end
  // autoplay whose playhead jitters as the duration grows.
  assert!(js.contains("box._live = true; createCastPlayer(box, 'end')"), "running casts open live at the edge");
  assert!(!js.contains("near-end"), "the jittery near-end preview is gone");
  assert!(js.contains("beecast-livechange"), "scsh tracks the player's live state");
}

#[test]
fn fullscreen_cast_refresh_preserves_the_active_player_dom() {
  let js = live_client_js();
  let create = js
    .split("function createCastPlayer")
    .nth(1)
    .and_then(|tail| tail.split("function focusCastPlayer").next())
    .expect("createCastPlayer body");
  let defer_at = create.find("if (castOwnsFullscreen(box))").expect("fullscreen preservation guard");
  let dispose_at = create.find("box._player.dispose()").expect("player replacement path");
  assert!(defer_at < dispose_at, "fullscreen must be checked before the active DOM node is disposed");
  assert!(create.contains("box._deferredPlayerRefresh = { startAt, autoplay }"), "repeated refreshes coalesce");

  let fullscreen_change = js
    .split("function onCastFullscreenChange")
    .nth(1)
    .and_then(|tail| tail.split("function procHtml").next())
    .expect("fullscreenchange body");
  assert!(fullscreen_change.contains("if (!refresh || castOwnsFullscreen(box)) return;"));
  assert!(fullscreen_change.contains("createCastPlayer(box, refresh.startAt, refresh.autoplay)"));
  assert!(js.contains("document.addEventListener('fullscreenchange', onCastFullscreenChange)"));
  assert!(js.contains("document.addEventListener('webkitfullscreenchange', onCastFullscreenChange)"));
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
  assert!(js.matches("scrollIntoView").count() >= 2, "def form + defs panel scroll into view");
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
  assert!(page.contains(r#"<div class="chamfer card card--accent-left-purple"><div class="session-actions">"#));
  // 5. Proc islands wear status on the left accent stripe (and tint the label). The
  //    row is a chamfer: the stripe is painted by the outer layer, wrapping the cut
  //    corners, and the ::before surface insets past it.
  assert!(html.contains("details.proc.ok { --accent: var(--green); }"));
  assert!(html.contains("details.proc.running { --accent: var(--orange); }"));
  assert!(html.contains("var(--proc-border, var(--border)) 0);"));
  assert!(html.contains("details.proc.running summary .label { color: var(--orange); }"));
  assert!(
    html.contains(".wf-node.wf-running { --accent: var(--orange); --node-border: rgba(210, 153, 34, 0.55); }"),
    "graph running matches proc orange"
  );
  assert!(html.contains(".wf-node.wf-stalled { --accent: var(--purple); }"), "abandoned/stalled is purple");
  assert!(html.contains(".wf-node.wf-stopped { --accent: var(--red); }"), "stopped shares fail red");
  assert!(
    html.contains(".wf-node.wf-terminating { --accent: var(--orange);"),
    "terminating shares running orange"
  );
  // Status glyphs speak the diamond language of the chamfered style (same 45° family
  // as the connection dot): hollow diamond waiting, filled diamond running — no circles.
  assert_eq!(super::proc::status_glyph(crate::daemon::model::ProcStatus::Running), "◆");
  assert_eq!(super::proc::status_glyph(crate::daemon::model::ProcStatus::Waiting), "◇");
  assert!(js.contains("running:'◆'"), "the JS graph mirror uses the filled diamond");
  assert!(js.contains("waiting:'◇'"), "the JS graph mirror uses the hollow diamond");
  assert!(!js.contains("◉") && !js.contains("○"), "no circle glyphs remain");
  // The running pulse is a brightness swell — the chamfer clip would swallow a
  // box-shadow halo — and the zoom cluster is chamfered like every other control.
  assert!(html.contains("50% { filter: brightness(1.45); }"), "running nodes pulse via brightness");
  assert!(!html.contains(".wf-node.wf-flash"), "the dead box-shadow flash rule is gone");
  assert!(html.contains(r#"class="chamfer" data-wf-zoom-fit"#), "zoom buttons are chamfered");
  assert!(js.contains("stalled:'Abandoned'"), "legend label is Abandoned");
  assert!(js.contains("done:'Succeeded'"), "successful graph tasks are called Succeeded, not Done");
  assert!(js.contains("failed:'Job failed'"), "graph carries an explicit overall failure verdict");
  {
    let p = &mut store.sessions.get_mut("castab").unwrap().procs[0];
    p.elapsed = Some(18.0);
  }
  let page_with_elapsed = session_page(&store, "castab").expect("session renders");
  assert!(
    page_with_elapsed.contains(r#"data-proc-elapsed="0">done in 18s</span>"#),
    "ok rows say done in N: {page_with_elapsed}"
  );
  // Scoped to the proc markup: the client JS legitimately writes glyph spans (fleet sync).
  assert!(!session_procs_html(&page_with_elapsed).contains(r#"class="glyph""#), "no status glyph on proc rows");
  // 6. The builtin source badge wears purple.
  assert!(html.contains(".badge--purple"), "purple badge class ships");
  assert!(live_client_js().contains(r#"chamfer badge badge--purple"><span>builtin"#), "builtin badge is purple");
}

#[test]
fn cards_are_chamfered_islands() {
  let html = super::index_page(&Store::new(DaemonMode::Persistent, 7274, 1));
  // Every island card is a chamfered surface: the outer layer is the border (or the
  // accent stripe, painted wide enough to wrap the cut corner), the ::before is the fill.
  assert!(html.contains(r#"<div class="chamfer card card--accent-left-green">"#), "Run card is chamfered");
  assert!(html.contains(r#"<div class="chamfer card card--accent-left-cyan">"#), "Jobs card is chamfered");
  assert!(html.contains(".chamfer > * { position: relative; z-index: 1; }"), "children lift above the overlay");
  assert!(html.contains("var(--accent) 0 calc(var(--cut) + var(--accent-w)),"), "accent stripe wraps the chamfer");
  assert!(html.contains(".card--accent-left-cyan { --accent: var(--cyan); }"));
  assert!(!html.contains(".card--accent-left-cyan { border-left"), "no rounded-era accent borders remain");
  // clip-path clips box-shadows: the expanded graph modal must cast a drop-shadow instead.
  assert!(html.contains("filter: drop-shadow(0 24px 80px rgba(0,0,0,0.65));"));
  assert!(!html.contains("box-shadow: 0 24px 80px"));
}

#[test]
fn a_retried_route_is_visibly_a_retry() {
  use crate::daemon::workflow::{WorkflowMeta, WorkflowNodeMeta};
  fn skill(index: usize, name: &str, route: &str, status: ProcStatus, fail: Option<&str>) -> ProcRecord {
    ProcRecord {
      index,
      kind: ProcKind::Skill,
      label: format!("claude: {name}"),
      status,
      note: None,
      detail: Some("done".into()),
      fail_reason: fail.map(Into::into),
      container_name: None,
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: Some("add".into()),
      route: Some(route.into()),
      result_path: None,
      annotate_target: None,
      harness: Some("claude".into()),
      skill_name: Some(name.into()),
      model: None,
      started_at: Some(1),
      elapsed: Some(2.0),
      lines: vec![],
    }
  }
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "retryv".into(),
    Session {
      id: "retryv".into(),
      started_at: 1,
      ended_at: Some(10),
      profile: Some("default".into()),
      kind: Some("profile".into()),
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 10,
      client_connected: false,
      run_pid: None,
      skills: vec![],
      procs: vec![
        skill(0, "add-claude", "claude", ProcStatus::Fail, Some(crate::failure::reason::CONTAINER_TIMEOUT)),
        skill(1, "add-opencode", "opencode", ProcStatus::Ok, None),
        skill(2, "add-claude", "claude", ProcStatus::Ok, None),
      ],
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "add-claude".into(),
            proc_index: Some(2),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "add-opencode".into(),
            proc_index: Some(1),
            order: 1,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    },
  );
  let html = session_page(&store, "retryv").expect("session renders");
  // The failed attempt cross-links its retry; the retry wears an attempt chip.
  assert!(
    html.contains(r##"<a class="proc-retry-link" href="#proc-2""##),
    "failed attempt links its retry: {html}"
  );
  assert!(html.contains("superseded — see attempt 2 ↓"), "link names the retry's ordinal");
  assert!(html.contains(r#"attempt-chip"><span>attempt 2</span>"#), "retry row wears an attempt chip");
  // The graph node (bound to the newest attempt) says it is a second attempt.
  assert!(html.contains(r#"<span class="wf-attempt"> · attempt 2</span>"#), "node state line shows the attempt");
  assert!(html.contains("Attempt 2 of 2 — an earlier attempt failed and was retried"), "tooltip explains");
  // The fleet comparison shows one row per route — the superseded attempt is gone.
  let fleet = html.split(r#"class="chamfer fleet""#).nth(1).and_then(|s| s.split("</section>").next()).expect("fleet");
  assert!(fleet.contains(r#"data-proc="2""#), "the retry represents its route");
  assert!(!fleet.contains(r#"data-proc="0""#), "the superseded attempt is not a fleet row");
  assert!(fleet.contains("2 ok, 0 fail"), "the rollup counts authoritative attempts only: {fleet}");
  // The single-attempt route carries neither chip nor link.
  let procs = session_procs_html(&html);
  assert_eq!(procs.matches("attempt-chip").count(), 1, "chip on the retry row only");
  assert_eq!(procs.matches("proc-retry-link").count(), 1, "link on the superseded row only");
}

#[test]
fn client_lifecycle_ignores_superseded_retry_failures() {
  // The JS fallback derivation mirrors Session::proc_is_superseded: a failed attempt
  // whose route was re-run by a later proc must not turn the job badge red.
  let js = live_client_js();
  assert!(js.contains("function procIsSuperseded"), "supersession helper ships");
  assert!(
    js.contains("p.status === 'fail' && !procIsSuperseded(session, p)"),
    "the failed set excludes superseded attempts"
  );
}

#[test]
fn overlays_are_chamfered() {
  // The floating chrome — confirm dialog, toast, instant tooltip — shares the chamfer
  // language, with drop-shadows (a clip-path swallows box-shadows) for depth.
  let html = super::index_page(&Store::new(DaemonMode::Persistent, 7274, 1));
  let js = live_client_js();
  assert!(js.contains("panel.className = 'chamfer scsh-dialog';"), "confirm dialog is chamfered");
  assert!(js.contains("el.className = 'chamfer toast';"), "toast is chamfered");
  assert!(js.contains("tip.className = 'chamfer ui-tip';"), "tooltip is chamfered");
  assert!(html.contains(".toast::before { background: #1c2128; }"));
  assert!(html.contains(".ui-tip::before { background: #1c2128; }"));
  let session = session_page(&store_with_cast_proc(ProcStatus::Running), "castab").expect("session renders");
  assert!(session.contains(".scsh-dialog::before { background: var(--surface); }"));
  assert!(session.contains("filter: drop-shadow(0 16px 40px rgba(0, 0, 0, 0.55));"));
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
    html.contains(r#"<a href="/">scsh</a><span class="crumb-sep">›</span><a href="/jobs">jobs</a><span class="crumb-sep">›</span><a class="job-id" href="/job/castab">castab</a>"#),
    "breadcrumb permalinks in the top island (the id in a fixed font)"
  );
  // The status dot sits at the very RIGHT edge of the island.
  assert!(
    html.contains(r#"{}<span class="dot" aria-hidden="true"></span></span></div>"#.trim_start_matches("{}")),
    "dot last in the island"
  );
  assert!(html.contains(r#"<span class="daemon-right">"#), "daemon status keeps the island's right side");
  assert!(!html.contains("<h1>"), "the body no longer duplicates the path as an h1");
  // Kind/profile/lifecycle live on the page lede — not repeated in the purple island.
  assert!(html.contains(r#"class="page-lede""#), "got lede: {html}");
  assert!(html.contains("workflow <strong>arith</strong>"), "lede names the workflow: {html}");
  assert!(!html.contains(r#"class="session-kind""#), "purple island no longer repeats kind: {html}");
  assert!(!html.contains(r#"<ul class="skills">"#), "purple island drops skills list: {html}");
  // A session with no kind (persisted by an older build) still reads as a profile.
  let mut old = store_with_cast_proc(ProcStatus::Running);
  old.sessions.get_mut("castab").unwrap().profile = Some("default".into());
  let html = session_page(&old, "castab").expect("session renders");
  assert!(html.contains("profile <strong>default</strong>"), "lede defaults kind to profile: {html}");
  assert!(!html.contains(r#"class="session-kind""#), "no session-kind on default profile: {html}");
  // The Run tab keeps the retained index crumb hidden; the Jobs URL renders it on
  // first paint so returning through the job-page breadcrumb never drops "jobs".
  let html = super::index_page(&store);
  assert!(html.contains(r#"<span id="index-crumb-tail" hidden>"#), "Run hides the retained index crumb: {html}");
  let html = super::index::index_page_for(&store, None, super::index::IndexTab::Jobs);
  assert!(
    html.contains(
      r#"<span class="crumbs"><a href="/">scsh</a><span id="index-crumb-tail"><span class="crumb-sep">›</span><a id="index-crumb" href="/jobs">jobs</a></span></span>"#
    ),
    "Jobs stays present in the top island on /jobs: {html}"
  );
}

#[test]
fn stop_strip_and_kill_buttons_ignore_zombie_sessions() {
  // A dead client's session keeps procs "running" forever. Force stop is omitted —
  // there is nothing left to stop.
  let store = store_with_cast_proc(ProcStatus::Running);
  let html = super::index_page(&store);
  assert!(!html.contains(r#"data-harness-stop=""#), "zombie sessions must not raise stop-all buttons");
  let page = session_page(&store, "castab").expect("session renders");
  assert!(!page.contains(r#"data-proc-stop="0""#), "zombie omits per-proc Force stop: {page}");
  assert!(!page.contains(r#"id="session-stop""#), "zombie omits job Force stop: {page}");

  // The same session, seen moments ago, gets an enabled kill.
  let mut live = store_with_cast_proc(ProcStatus::Running);
  live.sessions.get_mut("castab").unwrap().last_seen_at = crate::daemon::paths::now_unix_secs();
  let html = super::index_page(&live);
  assert!(html.contains(r#"data-harness-stop="claude""#), "live sessions raise the stop-all button");
  let page = session_page(&live, "castab").expect("session renders");
  assert!(page.contains(r#"data-proc-stop="0""#), "live sessions offer per-proc kill");
  assert!(!page.contains(r#"data-proc-stop="0" disabled"#), "live kill is enabled: {page}");
  assert!(page.contains(r#"id="session-stop""#), "live job offers Force stop");

  let proc = &mut live.sessions.get_mut("castab").unwrap().procs[0];
  proc.fail_reason = Some(crate::failure::reason::STOP_REQUESTED.into());
  proc.detail = Some("terminating all claude containers from the session browser".into());
  let html = super::index_page(&live);
  assert!(html.contains("Terminating all claude (1)…"), "harness strip acknowledges teardown: {html}");
  assert!(!html.contains(r#"data-harness-stop="claude""#), "a terminating harness cannot be stopped twice");
  let page = session_page(&live, "castab").expect("terminating session renders");
  assert!(page.contains(r#"class="chamfer proc terminating""#), "proc island turns orange while stopping: {page}");
  assert!(!page.contains(r#"data-proc-stop="0""#), "a terminating proc cannot be stopped twice: {page}");
}

#[test]
fn session_meta_agrees_with_lifecycle_badge() {
  // WEB-UI §6 / ENG §13: channels must not disagree. A job that missed its phase-aware
  // liveness deadline must not say "failed" in the badge and "still running" in Ended.
  let zombie = store_with_cast_proc(ProcStatus::Running);
  let page = session_page(&zombie, "castab").expect("session renders");
  assert!(
    page.contains("· failed"),
    "zombie lifecycle on lede: {page}"
  );
  assert!(!page.contains(r#"class="session-kind""#), "no island status chip: {page}");
  // Ended shows the last-seen timestamp (effective end), not the status phrase.
  assert!(page.contains(r#"data-session-ended>"#), "Ended present: {page}");
  assert!(!page.contains(r#"data-session-ended>still running</dd>"#), "Ended must not contradict: {page}");
  assert!(
    page.contains(r#"data-session-ended>19700101-002001 UTC</dd>"#),
    "a started job uses the 20-minute idle deadline: {page}"
  );
  // Repo above Branch.
  let repo = page.find("<dt>Repo</dt>").expect("Repo");
  let branch = page.find("<dt>Branch</dt>").expect("Branch");
  assert!(repo < branch, "Repo should sit above Branch: {page}");
  assert!(page.contains(r#"data-session-started>"#), "meta is server-rendered on first paint");
  assert!(page.contains(r#"data-last-seen="1""#), "last_seen seeds the client lifecycle");

  let mut live = store_with_cast_proc(ProcStatus::Running);
  live.sessions.get_mut("castab").unwrap().last_seen_at = crate::daemon::paths::now_unix_secs();
  let page = session_page(&live, "castab").expect("session renders");
  assert!(page.contains(r#"data-session-ended>still running</dd>"#), "live Ended: {page}");
  assert!(page.contains(r#"id="session-stop""#), "live Force stop stays available");
}

#[test]
fn index_page_shows_colored_harness_chips_per_proc() {
  let mut store = store_with_cast_proc(ProcStatus::Running);
  let full_repo = "/Users/dima/github/dimacurrentai/a-repository-name-that-is-long-enough-to-be-truncated";
  store.sessions.get_mut("castab").unwrap().repo = full_repo.into();
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
    for index in 3..11 {
      let mut extra = session.procs[1].clone();
      extra.index = index;
      extra.harness = Some("cursor".into());
      session.procs.push(extra);
    }
  }
  let html = super::index_page(&store);
  // A running chip's tip is just `harness · skill`; its start time rides in
  // data-tip-running, from which the tip module ticks a live "running for …" line.
  assert!(
    html.contains(r#"<a class="chamfer hchip hchip--claude" href="/job/castab#task-"#)
      && html.contains(r#"data-tip="claude · add" data-tip-running="1">C</a>"#),
    "got: {html}"
  );
  // A finished chip's tip is two lines: `harness · skill`, then the plain status word.
  assert!(
    html.contains(r#"<a class="chamfer hchip hchip--grok hchip--done" href="/job/castab#task-"#)
      && html.contains("data-tip=\"grok · add\ndone\">G</a>"),
    "got: {html}"
  );
  assert!(!html.contains(r#"class="hchip hchip--codex"#), "build procs must not render a chip");
  assert!(
    html.contains(r#"class="session-procs-cell"><span class="chip-count""#),
    "the total precedes the bounded chip sample"
  );
  assert!(html.contains(r#"<span class="chip-overflow">+ 2</span>"#), "only eight harness chips are shown: {html}");
  assert!(
    html.contains(&format!(r#"class="repo-copy" data-copy-value="{full_repo}" data-tip="{full_repo}""#)),
    "truncated repo path copies and discloses its full, unabridged value: {html}"
  );
  // The stylesheet distinguishes the same letter by harness color, and the client JS
  // mirrors the markup for live re-renders.
  assert!(html.contains(".hchip--claude"));
  assert!(html.contains(".hchip--codex"));
  assert!(html.contains("function harnessChipsHtml"));
  assert!(html.contains("function procRunHref"), "live chips preserve the same deep links");
  assert!(html.contains("function syncProcFromLocation"), "flat-job links open their selected run");
  assert!(html.contains("navigator.clipboard.writeText(value)"), "repo copy uses the Clipboard API");
  assert!(html.contains("'Copied!'"), "successful copies confirm through the tooltip");
  assert!(html.contains("if (!copy._scshCopyTimer)"), "live ticks preserve copy confirmation long enough to read");
}

#[test]
fn index_page_carries_the_setup_panel_and_its_client_wiring() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  assert!(html.contains("data-tab=\"setup\""), "nav label tab is Setup");
  assert!(html.contains(">Setup</button>"), "nav shows Setup, not Containers");
  assert!(!html.contains(">Containers</button>"), "Containers nav label is gone");
  assert!(html.contains("id=\"tab-setup\""), "setup panel id");
  assert!(html.contains("Harness setup"), "harness setup heading");
  assert!(html.contains("id=\"setup-cards\""), "harness cards container");
  assert!(html.contains("Images setup"), "images setup island");
  assert!(html.contains("card--accent-left-purple"), "images island uses purple accent");
  assert!(!html.contains("Advanced image management"), "no advanced disclosure");
  // Advanced still has the image table controls.
  for id in [
    "images-body",
    "images-build-selected",
    "images-build-stale",
    "images-build-all",
    "images-rebuild-base",
    "images-force",
  ] {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  // First paint already lists every harness + known image (§13: no empty limbo).
  assert!(html.contains("checking…"), "skeleton starts in checking…");
  for name in ["Claude", "Codex", "Grok", "Opencode", "Cursor"] {
    assert!(html.contains(name), "harness card {name} on first paint");
  }
  let cursor_card = html.find(">Cursor</strong>").expect("Cursor skeleton card");
  let opencode_card = html.find(">Opencode</strong>").expect("Opencode skeleton card");
  assert!(cursor_card < opencode_card, "OpenCode belongs after every native harness");
  assert!(html.contains("scsh-base:latest"), "base image row on first paint");
  for tag in
    ["scsh-opencode:latest", "scsh-claude:latest", "scsh-codex:latest", "scsh-grok:latest", "scsh-cursor:latest"]
  {
    assert!(html.contains(tag), "harness image {tag} on first paint");
  }
  let js = live_client_js();
  assert!(js.contains("/api/v1/setup"), "client js should fetch the setup API");
  assert!(js.contains("/api/v1/setup/tests"), "client js posts model probes");
  assert!(js.contains("/api/v1/images/build"), "client js should post builds");
  assert!(js.contains("function refreshSetup"), "setup refresh");
  assert!(js.contains("function startSetupTests"), "setup test starter");
  assert!(js.contains("setup-test-all"), "Test all defaults control");
  assert!(js.contains("data-setup-test"), "per-card Test selected");
  assert!(js.contains("setupCustomModels"), "custom models persist in ui prefs");
  assert!(js.contains("function markImagesChecking"), "refresh keeps rows visible while checking");
  assert!(js.contains("id === 'images'"), "images tab id remains a compatibility alias");
  assert!(!js.contains("loading…"), "must not replace the table with a blank loading row");
  assert!(js.contains("data-image-build"), "per-row build buttons are rendered");
  assert!(js.contains("data-setup-build"), "card Build/Update actions");
  assert!(html.contains("id=\"setup-test-all\""), "Test all defaults on the toolbar");
  assert!(js.contains("setup-models-hint"), "models section explains how to test");
  assert!(js.contains("ready to test"), "summary uses ready-to-test wording");
  assert!(js.contains("setupModelStatusHtml"), "model rows hide raw not_tested");
  assert!(js.contains("setup-ready"), "ready-to-test badge styling");
  assert!(js.contains("function startImageBuildOne"), "per-row build buttons are wired");
  assert!(html.contains("btn--orange btn--sm\" id=\"images-build-stale\"><span>Build stale"));
  assert!(html.contains("btn--purple btn--sm\" id=\"images-build-all\"><span>Build all"));
  assert!(js.contains("startImagesBuild('stale')"));
  assert!(js.contains("startImagesBuild('all')"));
  assert!(html.contains("image-action-cell"), "skeleton rows reserve the per-row action cell");
}

#[test]
fn index_page_carries_the_repositories_panel_and_its_client_wiring() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  for id in
    ["repo-path", "repo-pick", "repo-open", "repo-blockers", "defs-panel", "defs-list", "def-form", "repos-body"]
  {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  assert!(html.contains("<span>New Project</span>"), "create button is title-cased");
  assert!(html.contains("#repo-note:empty { display: none; }"), "empty note must not push Open off the shared right edge");
  assert!(html.contains("summary .note { order: 9; flex-basis: 100%;"), "the proc answer always gets its own summary line");
  // The four tabs, and their panels — Run is leftmost and the default landing tab.
  for (tab, panel) in [("run", "tab-run"), ("jobs", "tab-jobs"), ("projects", "tab-projects"), ("setup", "tab-setup")] {
    assert!(html.contains(&format!("data-tab=\"{tab}\"")), "index page should have the {tab} tab");
    assert!(html.contains(&format!("id=\"{panel}\"")), "index page should have panel #{panel}");
  }
  let nav = html.find("<nav class=\"tabs\" role=\"tablist\">").expect("tabs nav");
  let run_btn = html[nav..].find("data-tab=\"run\">Run</button>").expect("Run tab");
  let jobs_btn = html[nav..].find("data-tab=\"jobs\">Jobs</button>").expect("Jobs tab");
  assert!(run_btn < jobs_btn, "Run should be leftmost");
  assert!(
    html.contains("<section class=\"tab-panel active\" id=\"tab-run\" role=\"tabpanel\" aria-labelledby=\"tabbtn-run\">"),
    "Run panel active by default"
  );
  assert!(html.contains("class=\"tab active\" data-tab=\"run\">Run</button>"), "Run tab active by default");
  let js = live_client_js();
  assert!(js.contains("saved || 'run'"), "client default tab is Run");
  assert!(js.contains("pathForTab"), "tabs use path URLs, not #tab= hashes");
  assert!(!js.contains("'/#tab='"), "no hash-based tab navigation");
  assert!(js.contains("'/projects'"), "Projects tab path is /projects");
  assert!(super::index::IndexTab::from_path("/projects") == Some(super::index::IndexTab::Projects));
  assert!(super::index::IndexTab::from_path("/setup") == Some(super::index::IndexTab::Setup));
  assert!(super::index::IndexTab::from_path("/images") == Some(super::index::IndexTab::Setup));
  assert!(super::index::IndexTab::from_path("/jobs") == Some(super::index::IndexTab::Jobs));
  assert!(super::index::IndexTab::from_path("/") == Some(super::index::IndexTab::Run));
  assert!(js.contains("/api/v1/repos/open"), "client js opens a repo");
  assert!(js.contains("/api/v1/repos/pick"), "client js pops the folder picker");
  assert!(js.contains("/api/v1/jobs/start"), "client js starts a job");
  assert!(js.contains("function renderRepoJobs"), "client js renders jobs by repository");
  assert!(js.contains("function renderInternalJobs"), "client js renders Internal section");
  assert!(js.contains("Chapters pending ⬇"), "export label for pending chapters");
  assert!(js.contains("function syncChaptersPending"), "live chapters-pending sync");
  assert!(
    js.contains("SESSION_ID && liveSessions ? liveSessions[SESSION_ID]"),
    "pollForChapters must not index null liveSessions (TypeError reading session id)"
  );
  assert!(!js.contains("SESSION_ID && liveSessions[SESSION_ID]"), "no bare liveSessions[SESSION_ID] without null check");
  assert!(js.contains("const live = life === 'running'"), "Ready only while the job is live");
  assert!(js.contains("OPEN_REPO_RUNNABLE"), "client js gates Start on the repo being runnable");
  assert!(js.contains("p.type === 'text'"), "multiline definition params render distinctly");
  assert!(js.contains("<textarea"), "feature briefs use a text area rather than a one-line input");
  assert!(js.contains("missing.name + ' is required'"), "empty required prose is rejected before start");
  assert!(js.contains("function initTabs"), "client js wires the tabs");
  assert!(js.contains("function syncIndexCrumb"), "tab navigation keeps the top-island crumb in sync");
  assert!(js.contains("tail.hidden = !visible"), "tab navigation retains rather than recreates crumb nodes");
  assert!(js.contains("history.pushState"), "tab clicks push history (WEB-UI §1)");
  assert!(js.contains("popstate"), "Back/Forward restore the active tab");
  assert!(js.contains("function sessionEndedLabel"), "Ended label shares lifecycle with the badge");
  assert!(js.contains("function activateProcPanel"), "fleet and workflow share arrival cues");
  assert!(js.contains("localStorage"), "UI prefs persist (WEB-UI §7)");
  assert!(!js.contains("fonts.googleapis.com"), "no Google Fonts in the live client");
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
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("opencode".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: None,
        lines: vec![crate::daemon::model::OutputLine { at: 0.5, text: "building…".into() }],
      }],
      workflow: None,
      parent_session: None,
    },
  );
  let html = session_page(&store, "test").expect("session page");
  let procs = session_procs_html(&html);
  assert!(!html.contains("\\\n"), "raw-string line continuations must not leak backslashes");
  assert!(!procs.contains("\\\n"), "proc markup must not leak backslashes");
  assert!(!procs.contains("autoscroll-ctl"), "no Auto-scroll checkbox");
  assert!(!procs.contains("Auto-scroll to bottom"), "no Auto-scroll checkbox");
  assert!(html.contains(r#"id="session-stop""#), "running session should offer Force stop");
  assert!(html.contains("Force stop"));
}

#[test]
fn session_page_shows_the_commits_diff_chip_only_when_packed() {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "difjob".into(),
    Session {
      id: "difjob".into(),
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
      procs: vec![
        ProcRecord {
          index: 0,
          kind: ProcKind::Skill,
          label: "opencode: add".into(),
          status: ProcStatus::Ok,
          note: None,
          detail: None,
          fail_reason: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: Some("/tmp/scsh-home/sessions/difjob/diffs/add-p0.html".into()),
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
          harness: Some("opencode".into()),
          skill_name: Some("add".into()),
          model: None,
          started_at: Some(1),
          elapsed: Some(2.0),
          lines: vec![],
        },
        ProcRecord {
          index: 1,
          kind: ProcKind::Skill,
          label: "claude: add".into(),
          status: ProcStatus::Ok,
          note: None,
          detail: None,
          fail_reason: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
          harness: Some("claude".into()),
          skill_name: Some("add".into()),
          model: None,
          started_at: Some(1),
          elapsed: Some(2.0),
          lines: vec![],
        },
      ],
      workflow: None,
      parent_session: None,
    },
  );
  let html = session_page(&store, "difjob").expect("session page");
  assert!(html.contains(r#"href="/diff/difjob/all""#), "the top run island links the end-to-end diff");
  let procs = session_procs_html(&html);
  // The step whose commits were packed links its review page; the other has no chip.
  assert!(procs.contains(r#"href="/diff/difjob/0""#), "packed step links its diff: {procs}");
  assert!(procs.contains("⇄ commits diff"), "the chip is labeled: {procs}");
  assert!(!procs.contains(r#"href="/diff/difjob/1""#), "unpacked step has no diff link: {procs}");
  assert_eq!(procs.matches("data-proc-diff").count(), 1, "exactly one chip: {procs}");
  // Plain click navigates in THIS tab; cmd/ctrl+click keeps its native new-tab meaning.
  assert!(!procs.contains("target="), "no target override on the diff chip: {procs}");
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
      workflow: None,
      parent_session: None,
    },
  );
  let html = session_page(&store, "done01").expect("session page");
  // Settled jobs omit Force stop — no grayed-out stub.
  assert!(!html.contains(r#"id="session-stop""#), "ended session hides Force stop: {html}");
  assert!(html.contains(r#"class="page-lede""#), "session page has a plain-language lede");
  assert!(
    html.contains("· completed ·") || html.contains("completed"),
    "lede carries the completed lifecycle: {html}"
  );
  assert!(
    !html.contains(r#"class="session-kind""#),
    "ended session has no session-kind heading in the island: {html}"
  );
  assert!(
    !html.contains(r#"session-actions"><span class="chamfer session-status"#),
    "no badge in the top-right actions slot"
  );
  assert!(!html.contains(r#"<ul class="skills">"#), "no skills list in purple island");
}

#[test]
fn offline_export_carries_lede_and_full_meta() {
  // Parity with the live job page: the export shows the lede (kind · lifecycle · task
  // count) and the FULL meta — ended and duration included — so the offline copy answers
  // "did it succeed, and how long did it take" without the daemon.
  let session = Session {
    id: "exp01".into(),
    started_at: 1,
    ended_at: Some(10),
    profile: Some("code-review".into()),
    kind: Some("profile".into()),
    repo: "/tmp/repo".into(),
    branch: "main".into(),
    last_seen_at: 10,
    client_connected: false,
    run_pid: None,
    skills: vec![],
    procs: vec![],
    workflow: None,
    parent_session: None,
  };
  let html = session_export_page(&session, &[], 100);
  assert!(html.contains(r#"class="page-lede""#), "export carries the live page's lede: {html}");
  assert!(
    html.contains("profile <strong>code-review</strong> · completed · 0 tasks"),
    "lede shows kind, profile, lifecycle, and task count: {html}"
  );
  assert!(!html.contains("0 tasks."), "task count is not punctuated as a sentence: {html}");
  assert!(html.contains(r#"<dl class="session-meta">"#), "export keeps session-meta: {html}");
  assert!(html.contains("<code>exp01</code>"), "job id in meta: {html}");
  assert!(html.contains("<dt>Ended</dt>"), "export meta shows when the job ended: {html}");
  assert!(html.contains("<dt>Duration</dt><dd>9s</dd>"), "export meta shows how long the job took: {html}");
  assert!(html.contains("accessibility: 'snapshot'"), "export player opts enable a11y snapshot");
  // Offline snapshots must not carry live Force stop chrome (markup or styles).
  assert!(!html.contains("Force stop"), "export must not mention Force stop: {html}");
  assert!(!html.contains("session-stop"), "export must not ship #session-stop: {html}");
  assert!(!html.contains("proc-kill"), "export must not ship .proc-kill: {html}");
  assert!(!html.contains("scsh-dialog"), "export must not ship the Force stop dialog: {html}");
}

#[test]
fn offline_export_advertises_chapter_keys_only_when_chapters_exist() {
  use super::session_export::CastExport;
  let store = store_with_cast_proc(ProcStatus::Ok);
  let session = store.sessions.get("castab").unwrap();
  let no_chapters = [CastExport::Cast {
    ndjson: "{\"version\":3,\"term\":{\"cols\":10,\"rows\":3}}\n[0.1,\"o\",\"hello\"]\n".into(),
    summary: None,
    chapters: vec![],
    diff_html: None,
  }];
  let html = session_export_page(session, &no_chapters, 100);
  assert!(!html.contains("c chapters"), "an empty chapter panel must not be advertised: {html}");

  let with_chapters = [CastExport::Cast {
    ndjson: "{\"version\":3,\"term\":{\"cols\":10,\"rows\":3}}\n[0.1,\"o\",\"hello\"]\n".into(),
    summary: None,
    chapters: vec![(0.0, "Start".into())],
    diff_html: None,
  }];
  let html = session_export_page(session, &with_chapters, 100);
  assert!(html.contains("[/] chapter · c chapters"), "real chapters advertise their keyboard controls: {html}");
}

#[test]
fn offline_export_embeds_commits_diff_when_present() {
  use super::session_export::CastExport;
  let session = Session {
    id: "expdf".into(),
    started_at: 1,
    ended_at: Some(10),
    profile: Some("default".into()),
    kind: Some("profile".into()),
    repo: "/tmp/repo".into(),
    branch: "main".into(),
    last_seen_at: 10,
    client_connected: false,
    run_pid: None,
    skills: vec![],
    procs: vec![ProcRecord {
      index: 0,
      kind: ProcKind::Skill,
      label: "opencode: add".into(),
      status: ProcStatus::Ok,
      note: None,
      detail: Some("ok".into()),
      fail_reason: None,
      container_name: None,
      container_runtime: None,
      cast_path: None,
      diff_path: Some("/tmp/diff.html".into()),
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
      harness: Some("opencode".into()),
      skill_name: Some("add".into()),
      model: None,
      started_at: Some(1),
      elapsed: Some(1.0),
      lines: vec![],
    }],
    workflow: None,
    parent_session: None,
  };
  let hostile = r#"<html><body></script><p>diff</p></body></html>"#;
  let exports = [CastExport::Note { text: "no recording".into(), diff_html: Some(hostile.into()) }];
  let html = session_export_page(&session, &exports, 100);
  assert!(html.contains(r#"<span class="proc-diff""#), "summary carries static commits-diff chip");
  assert!(html.contains(r#"<details class="chamfer proc-diff">"#), "body embeds the packed diff");
  assert!(html.contains("srcdoc="), "diff rides in an iframe srcdoc");
  assert!(
    html.contains(r#"sandbox="allow-scripts allow-same-origin""#),
    "packdiff 0.4.4 needs scripts + same-origin for WASM/localStorage: {html}"
  );
  assert!(html.contains("<\\/"), "hostile </ is broken for srcdoc like CASTS");
  assert!(!html.contains("</script><p>diff"), "raw </script> must not appear unescaped");
}

#[test]
fn offline_export_keeps_text_log_lines() {
  use super::session_export::CastExport;
  use crate::daemon::model::OutputLine;
  // A proc that ran WITHOUT a recording shows full timestamped log lines on the live page;
  // the export must carry the same lines statically (same markup/classes) instead of
  // collapsing them to a one-line "no recording" note.
  let session = Session {
    id: "explog".into(),
    started_at: 1,
    ended_at: Some(10),
    profile: Some("default".into()),
    kind: Some("profile".into()),
    repo: "/tmp/repo".into(),
    branch: "main".into(),
    last_seen_at: 10,
    client_connected: false,
    run_pid: None,
    skills: vec![],
    procs: vec![ProcRecord {
      index: 0,
      kind: ProcKind::Build,
      label: "build: claude".into(),
      status: ProcStatus::Ok,
      note: None,
      detail: None,
      fail_reason: None,
      container_name: None,
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
      harness: Some("claude".into()),
      skill_name: None,
      model: None,
      started_at: Some(1),
      elapsed: Some(9.0),
      lines: vec![
        OutputLine { at: 0.5, text: "Step 1/4 : FROM ubuntu".into() },
        OutputLine { at: 2.0, text: "<hostile> & escaped".into() },
      ],
    }],
    workflow: None,
    parent_session: None,
  };
  let note = "no recording — image build ran without asciinema on PATH (text log only)";
  let exports = [CastExport::Note { text: note.into(), diff_html: None }];
  let html = session_export_page(&session, &exports, 100);
  assert!(html.contains(r#"<div class="chamfer output">"#), "export embeds the text-log output box: {html}");
  assert!(
    html.contains(r#"<div class="line"><span class="at">+0.5s</span> Step 1/4 : FROM ubuntu</div>"#),
    "export keeps the live page's timestamped line markup: {html}"
  );
  assert!(html.contains("&lt;hostile&gt; &amp; escaped"), "log lines are HTML-escaped: {html}");
  assert!(!html.contains(note), "the one-line note gives way to the actual log lines: {html}");
  // A proc with truly no output still gets the explanatory note, not an empty box.
  let mut bare = session.clone();
  bare.procs[0].lines.clear();
  let exports = [CastExport::Note { text: note.into(), diff_html: None }];
  let bare_html = session_export_page(&bare, &exports, 100);
  assert!(bare_html.contains(note), "no lines → the note explains why there is nothing to embed: {bare_html}");
  assert!(!bare_html.contains(r#"<div class="chamfer output">"#), "no lines → no empty output box: {bare_html}");
}

#[test]
fn offline_export_includes_workflow_graph() {
  use super::session_export::CastExport;
  use crate::daemon::workflow::{WorkflowMeta, WorkflowNodeMeta};
  // The live job page renders the workflow DAG above the proc rows; the export must carry
  // the same server-rendered graph (frozen at export time), start/finish terminals and
  // task anchors included, so the offline copy shows how the job was wired.
  let session = Session {
    id: "expwf".into(),
    started_at: 1,
    ended_at: Some(10),
    profile: Some("arith".into()),
    kind: Some("workflow".into()),
    repo: "/tmp/repo".into(),
    branch: "main".into(),
    last_seen_at: 10,
    client_connected: false,
    run_pid: None,
    skills: vec![],
    procs: vec![
      ProcRecord {
        index: 0,
        kind: ProcKind::Skill,
        label: "claude: add".into(),
        status: ProcStatus::Ok,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: Some("add".into()),
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("claude".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: Some(4.0),
        lines: vec![],
      },
      ProcRecord {
        index: 1,
        kind: ProcKind::Skill,
        label: "codex: summarize".into(),
        status: ProcStatus::Ok,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: Some("summarize".into()),
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("codex".into()),
        skill_name: Some("summarize".into()),
        model: None,
        started_at: Some(5),
        elapsed: Some(5.0),
        lines: vec![],
      },
    ],
    workflow: Some(WorkflowMeta {
      nodes: vec![
        WorkflowNodeMeta {
          id: "add".into(),
          proc_index: Some(0),
          order: 0,
          needs: vec![],
          conditional: false,
          when_summary: None,
        },
        WorkflowNodeMeta {
          id: "summarize".into(),
          proc_index: Some(1),
          order: 1,
          needs: vec!["add".into()],
          conditional: false,
          when_summary: None,
        },
      ],
    }),
    parent_session: None,
  };
  let exports = [
    CastExport::Note { text: "no recording".into(), diff_html: None },
    CastExport::Note { text: "no recording".into(), diff_html: None },
  ];
  let html = session_export_page(&session, &exports, 100);
  assert!(html.contains(r#"id="workflow-graph""#), "export carries the workflow card: {html}");
  assert!(html.contains(r#"class="chamfer wf-bookend wf-start""#), "graph keeps its start bookend");
  assert!(html.contains(r#"class="chamfer wf-bookend wf-finish""#), "graph keeps its finish bookend");
  assert!(html.contains(r#"data-workflow-step="add""#) && html.contains(r#"data-workflow-step="summarize""#));
  // The graph's jump links resolve offline: proc rows carry the same task anchors as live.
  assert!(html.contains("href=\"#task-add\""), "node links target task anchors");
  assert!(html.contains(r#"<details open class="chamfer proc ok" data-index="0" id="task-add""#), "proc row anchors: {html}");
  // The graph CSS rides in the shared stylesheet the export inlines.
  assert!(html.contains(".wf-bookend"), "bookend CSS is inlined in the export");
}

/// The standalone play page accepts BOTH deep-link forms: '#t=' (primary — what its copy
/// button writes) and '?t=' (what beecast-generated offline pages link with), so links
/// work across surfaces. Also parse-gates the page's inline script under Node, same
/// pattern as `live_client_js_parses_under_node`.
#[test]
fn cast_play_page_accepts_query_time_deep_links() {
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(page.contains("location.hash.match(/^#t=([0-9:.]+)$/)"), "hash form stays primary");
  assert!(
    page.contains("new URLSearchParams(location.search).get('t')"),
    "query form is parsed from location.search: {page}"
  );
  assert!(page.contains("/^[0-9:.]+$/.test(q)"), "query values pass the same seconds/mm:ss shape check");
  assert!(page.contains("'#t=' + t"), "the copy button still writes the hash form");
  if crate::runtime::which("node").is_none() {
    return;
  }
  let script = page.rsplit("<script>").next().expect("inline boot script");
  let script = script.split("</script>").next().expect("script end");
  let dir = std::env::temp_dir().join(format!("scsh-cast-js-check-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let path = dir.join("cast-page.js");
  std::fs::write(&path, script).unwrap();
  let out = std::process::Command::new("node").arg("--check").arg(&path).output().expect("spawn node --check");
  assert!(
    out.status.success(),
    "cast page inline script must parse: {}\n{}",
    String::from_utf8_lossy(&out.stderr),
    String::from_utf8_lossy(&out.stdout)
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_page_renders_fleet_comparison_for_shared_skill_source() {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "fleet1".into(),
    Session {
      id: "fleet1".into(),
      started_at: 1,
      ended_at: Some(10),
      profile: Some("default".into()),
      kind: Some("profile".into()),
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 10,
      client_connected: false,
      run_pid: None,
      skills: vec![],
      procs: vec![
        ProcRecord {
          index: 0,
          kind: ProcKind::Skill,
          label: "opencode: add-opencode".into(),
          status: ProcStatus::Ok,
          note: None,
          detail: Some("2 + 3 = 5".into()),
          fail_reason: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: Some("opencode".into()),
          result_path: None,
          annotate_target: None,
          harness: Some("opencode".into()),
          skill_name: Some("add-opencode".into()),
          model: None,
          started_at: Some(1),
          elapsed: Some(1.0),
          lines: vec![],
        },
        ProcRecord {
          index: 1,
          kind: ProcKind::Skill,
          label: "claude: add-claude".into(),
          status: ProcStatus::Ok,
          note: None,
          detail: Some("2 + 3 = 5".into()),
          fail_reason: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: Some("claude".into()),
          result_path: None,
          annotate_target: None,
          harness: Some("claude".into()),
          skill_name: Some("add-claude".into()),
          model: None,
          started_at: Some(1),
          elapsed: Some(1.2),
          lines: vec![],
        },
      ],
      workflow: None,
      parent_session: None,
    },
  );
  let html = session_page(&store, "fleet1").expect("session page");
  assert!(html.contains(r#"class="fleets""#), "fleet section present: {html}");
  assert!(html.contains(r#"class="fleet-compare""#), "comparison table present");
  assert!(html.contains(r#"data-skill-source="add""#), "grouped by skill_source");
  assert!(html.contains(r#"class="chamfer fleet-jump" data-proc="0""#), "jump to first route");
  assert!(html.contains(r#"class="chamfer fleet-jump" data-proc="1""#), "jump to second route");
  assert!(html.contains("· 2 routes"), "true route matrices keep route terminology");
  assert!(html.contains("<th>Route</th>"), "true route matrices keep the Route column");
  // The table is live: tick snapshots re-sync each row's status/glyph/duration/result and
  // the rollup line in place (server-rendered grade/issue text is richer and is kept).
  let js = live_client_js();
  assert!(js.contains("function syncFleetSections"), "fleet sections sync from ticks");
  assert!(js.contains("syncFleetSections(session, nowUnix);"), "renderSession keeps fleets current");
  assert!(js.contains("running:'◆',waiting:'◇'"), "fleet glyphs share the diamond language");
  assert!(js.contains(".fleet-grade, .fleet-issues"), "richer server-rendered results are preserved");
  let fleets_at = html.find(r#"class="fleets""#).expect("fleets");
  let first_proc_at = html.find(r#"data-index="0""#).expect("first proc");
  let last_proc_at = html.find(r#"data-index="1""#).expect("last proc");
  assert!(first_proc_at < last_proc_at && last_proc_at < fleets_at, "comparison follows the work it summarizes");

  // Repeated workflow steps are cycles, not model/harness routes.
  {
    let session = store.sessions.get_mut("fleet1").unwrap();
    for (i, proc) in session.procs.iter_mut().enumerate() {
      proc.skill_source = Some("review_docs".into());
      proc.skill_name = Some(format!("review_docs-while-decide-{}", i + 1));
      proc.route = proc.skill_name.clone();
    }
  }
  let cycles = session_page(&store, "fleet1").expect("cycle page");
  assert!(cycles.contains("· 2 cycle iterations"), "loop repetitions are named as cycle iterations");
  assert!(cycles.contains("<th>Cycle iteration</th>"), "cycle table names its first column honestly");
  assert!(cycles.contains("all cycle iterations agree"), "cycle rollup avoids route terminology");
  assert!(cycles.contains("rgba(88,166,255,0.09)"), "comparison islands carry a light-blue tint");
  assert_eq!(
    cycles.matches("<strong>skill</strong> <code>review_docs</code>").count(),
    2,
    "run metadata shows the authored action, not generated while-loop wiring"
  );
  assert!(
    live_client_js().contains("skillName = p.skill_source"),
    "live-added loop rows use the same human-facing action name"
  );
}

#[test]
fn fleet_routes_stack_completed_before_running_before_waiting() {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "fleet2".into(),
    Session {
      id: "fleet2".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("default".into()),
      kind: Some("profile".into()),
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: crate::daemon::paths::now_unix_secs(),
      client_connected: true,
      run_pid: Some(1),
      skills: vec![],
      procs: vec![
        ProcRecord {
          index: 0,
          kind: ProcKind::Skill,
          label: "claude: add-waiting".into(),
          status: ProcStatus::Waiting,
          note: None,
          detail: None,
          fail_reason: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: Some("waiting-route".into()),
          result_path: None,
          annotate_target: None,
          harness: Some("claude".into()),
          skill_name: Some("add-waiting-route".into()),
          model: None,
          started_at: None,
          elapsed: None,
          lines: vec![],
        },
        ProcRecord {
          index: 1,
          kind: ProcKind::Skill,
          label: "claude: add-done".into(),
          status: ProcStatus::Ok,
          note: None,
          detail: Some("ok".into()),
          fail_reason: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: Some("done-route".into()),
          result_path: None,
          annotate_target: None,
          harness: Some("claude".into()),
          skill_name: Some("add-done-route".into()),
          model: None,
          started_at: Some(1),
          elapsed: Some(1.0),
          lines: vec![],
        },
        ProcRecord {
          index: 2,
          kind: ProcKind::Skill,
          label: "claude: add-running".into(),
          status: ProcStatus::Running,
          note: None,
          detail: None,
          fail_reason: None,
          container_name: None,
          container_runtime: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: Some("running-route".into()),
          result_path: None,
          annotate_target: None,
          harness: Some("claude".into()),
          skill_name: Some("add-running-route".into()),
          model: None,
          started_at: Some(1),
          elapsed: None,
          lines: vec![],
        },
      ],
      workflow: None,
      parent_session: None,
    },
  );
  let html = session_page(&store, "fleet2").expect("fleet page");
  let done_at = html.find("done-route").expect("done route");
  let running_at = html.find("running-route").expect("running route");
  let waiting_at = html.find("waiting-route").expect("waiting route");
  assert!(done_at < running_at && running_at < waiting_at, "Completed → Running → Waiting: {html}");
}

#[test]
fn client_js_wires_fleet_jumps_and_accessibility_snapshot() {
  let js = live_client_js();
  assert!(js.contains("function parseIndexFilter"), "client parses /project and /repo");
  assert!(js.contains("function repoFilterHref"), "client builds filter hrefs");
  assert!(js.contains("repo-filter-link"), "live Projects rows stay clickable");
}

#[test]
fn client_js_wires_force_stop() {
  let js = live_client_js();
  assert!(js.contains("/api/v1/session/stop"), "client js posts session stop");
  assert!(js.contains("function forceStopSession"), "client js defines forceStopSession");
  assert!(js.contains("function scshConfirm"), "Force stop uses an in-app confirm dialog");
  assert!(js.contains("scsh-dialog"), "dialog markup class ships");
  assert!(!js.contains("confirm("), "no browser confirm() for Force stop");
  assert!(!js.contains("alert("), "Force stop errors use toast, not alert()");
  assert!(js.contains("Terminating all ' + harness"), "stop-all acknowledges the accepted request in its button");
  assert!(js.contains("Stop requested for all ' + harness"), "stop-all immediately confirms through the live toast");
  assert!(js.contains("Stopped ' + n + ' ' + harness + ' task"), "stop-all reports its final stopped count");
  assert!(js.contains("p.fail_reason === 'stop_requested'"), "live tasks expose the terminating transition");
}

/// Accessibility hardening: confirm-dialog focus management, the full ARIA tab pattern,
/// live-region copy feedback, reduced-motion-gated scrolling and micro-transitions, and
/// breadcrumb truncation on narrow viewports.
#[test]
fn dashboard_a11y_contracts_hold() {
  let js = live_client_js();
  // 1. The confirm dialog remembers the previously focused element, restores it on
  //    close, and traps Tab inside the panel while it is open.
  assert!(js.contains("const prevFocus = document.activeElement;"), "dialog records the opener's focus");
  assert!(js.contains("prevFocus.focus()"), "dialog restores focus on close");
  assert!(js.contains("ev.key === 'Tab'"), "dialog traps Tab inside the modal");
  // 2. Tabs carry the complete ARIA pattern: a tablist container, labelled tabpanels,
  //    arrow-key navigation, and a roving tabindex.
  let html = super::index_page(&Store::new(DaemonMode::Persistent, 7274, 1));
  assert!(html.contains(r#"<nav class="tabs" role="tablist">"#), "the tabs nav is a tablist");
  assert_eq!(html.matches(r#"role="tabpanel""#).count(), 4, "every panel is a tabpanel");
  assert!(html.contains(r#"aria-labelledby="tabbtn-run""#), "panels are labelled by their tabs");
  assert!(html.contains(r#"aria-selected="true""#), "the active tab is selected server-side");
  assert!(js.contains("'ArrowRight'"), "arrow keys walk the tablist");
  assert!(js.contains("x.tabIndex = on ? 0 : -1;"), "roving tabindex follows the active tab");
  // 3. Every scroll respects prefers-reduced-motion; none is hardcoded smooth.
  assert!(js.contains("list.scrollIntoView"), "open/create still scrolls Definitions into view");
  assert!(!js.contains("behavior: 'smooth'"), "no hardcoded smooth scrolling");
  // 4. The cast page announces "copied" to screen readers, like the dashboard toast.
  let cast = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(cast.contains(r#"<span id="copied" role="status">"#), "copy feedback is a live region");
  // 5. Long breadcrumbs truncate inside the fixed-height sticky status bar instead of
  //    overflowing phone-width viewports.
  assert!(
    html.contains("min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"),
    "crumbs truncate with an ellipsis"
  );
  // 6. Decorative micro-transitions (buttons, chips, the toast) sit behind the same
  //    reduced-motion gate as the workflow pulse.
  let gate = html.find("@media (prefers-reduced-motion: no-preference)").expect("reduced-motion gate ships");
  for rule in [".btn, button.btn { transition:", ".hchip { transition:", ".toast { transition:"] {
    assert_eq!(html.matches(rule).count(), 1, "{rule} appears exactly once");
    assert!(html.find(rule).unwrap() > gate, "{rule} is gated on reduced motion");
  }
}

#[test]
fn client_js_mirrors_the_commits_diff_chip() {
  // Integration (and the packdiff pack) happens after a step finished, so the chip usually
  // arrives on a live tick: the client must render the same markup session.rs serves.
  let js = live_client_js();
  assert!(js.contains("function procDiffBtnHtml"), "client js builds the diff chip");
  assert!(js.contains("p.diff_path"), "client js keys the chip on the proc's diff_path");
  assert!(js.contains("⇄ commits diff"), "the live chip carries the same label");
  assert!(js.contains("data-job-diff"), "the live run island gains the whole-job diff button");
  assert!(js.contains("actions = document.createElement('div')"), "a closed slim row gains its action island live");
  assert!(js.contains("initProcDiffs"), "chips present at page render are wired too");
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
        container_runtime: None,
        cast_path: Some("/tmp/x.cast".into()),
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("claude".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: Some(3.0),
        lines: vec![],
      }],
      workflow: None,
      parent_session: None,
    },
  );
  let html = session_page(&store, "castab").expect("session page");
  // The page loads the player assets and embeds a player box wired to the cast endpoint.
  assert!(html.contains(r#"<link rel="stylesheet" href="/assets/scsh-cast-player.css">"#), "player css");
  assert!(html.contains(r#"<script src="/assets/scsh-cast-player.js"></script>"#), "player js");
  let procs = session_procs_html(&html);
  assert!(procs.contains(r#"<div class="cast" data-cast-url="/cast/castab/2""#), "cast embed");
  // Fullscreen lives in the player's own control bar now (⛶ + the f key) — the page
  // toolbar carries no fullscreen button of its own. Opening a
  // section focuses its player, so space and f work with no click first.
  assert!(!procs.contains("data-cast-fs"), "no page-side fullscreen button");
  assert!(procs.contains("f fullscreen"), "the keys hint teaches f");
  // Streaming drives itself (WS growth appends + the finish reload), so there is no manual
  // Reload button; chapters are the player's own chrome (☰ panel + c key + seek ticks) —
  // no scsh-side chip row or fullscreen sidebar. The external hint starts empty and is
  // populated only after the sidecar proves chapters exist.
  assert!(!procs.contains("data-cast-reload"), "no manual reload in a streaming toolbar");
  assert!(!procs.contains("c chapters"), "do not advertise chapters before markers exist");
  assert!(procs.contains("data-chapter-keys"), "the live client owns the conditional chapter hint");
  let js = live_client_js();
  assert!(!js.contains("data-cast-reload"), "client js builds no reload button");
  assert!(!js.contains("cast-chapters"), "no scsh-side chapter chips");
  assert!(!js.contains("cast-fs-chapters"), "no scsh-side fullscreen chapters sidebar");
  assert!(js.contains("markers"), "chapters reach the player as markers");
  assert!(js.contains("function setChapterKeys") && js.contains("c chapters"), "the hint appears with real markers");
  assert!(js.contains("function focusCastPlayer"), "open sections hand the player the keyboard");
  assert!(js.contains("if (det.open) focusCastPlayer(box)"), "focus follows the section toggle");
  // Run snapshot sits in the proc island's top-right (above Force stop), cyan chamfer —
  // not inside the cast toolbar (toolbar keeps only `.cast` download + keys hint).
  assert!(!procs.contains("data-cast-link"), "no link-at-time in the inline toolbar");
  assert!(procs.contains(r#"class="chamfer btn btn--cyan btn--sm proc-snapshot""#), "run snapshot link");
  assert!(procs.contains(r#"href="/cast/castab/2/export.html" data-cast-export"#), "run snapshot href");
  assert!(
    !procs.contains(r#"cast-toolbar"><a href="/cast/castab/2/export.html"#),
    "snapshot is outside the cast toolbar"
  );
  assert!(procs.contains(r#"<a href="/cast/castab/2?dl=1" download>"#), "download link");
  // A recorded proc shows the player, NOT the text output.
  assert!(!procs.contains(r#"<div class="chamfer output">"#), "no text output for recorded proc");
  assert!(!procs.contains("autoscroll-ctl"), "no autoscroll control for recorded proc");
}

#[test]
fn session_proc_html_has_no_autoscroll_checkbox() {
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
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("opencode".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: None,
        // The auto-scroll control belongs to a text-output box, so the proc must have
        // streamed at least one line for the control to render.
        lines: vec![crate::daemon::model::OutputLine { at: 0.2, text: "cloning…".into() }],
      }],
      workflow: None,
      parent_session: None,
    },
  );
  let html = session_page(&store, "test").expect("session page");
  let procs = session_procs_html(&html);
  assert!(procs.contains(r#"<div class="chamfer output">"#), "text fallback output remains");
  assert!(!procs.contains("autoscroll-ctl"), "Auto-scroll checkbox removed");
  assert!(!procs.contains("Auto-scroll to bottom"), "Auto-scroll checkbox removed");
  let js = live_client_js();
  assert!(!js.contains("Auto-scroll to bottom"), "client JS has no Auto-scroll checkbox");
  assert!(js.contains("followOutput"), "sticky follow without a checkbox");
}

/// A one-proc store whose proc is an `Annotate` row: no recording, no log lines — the
/// shape the post-run annotation pass registers while it summarizes a cast.
fn store_with_annotate_proc(status: ProcStatus) -> Store {
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "annjob".into(),
    Session {
      id: "annjob".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("annotate".into()),
      kind: Some("annotate".into()),
      repo: "(internal)".into(),
      branch: "".into(),
      // Fresh heartbeat: per-proc Force stop renders only for sessions still alive.
      last_seen_at: crate::daemon::paths::now_unix_secs(),
      client_connected: true,
      run_pid: None,
      skills: vec![],
      procs: vec![ProcRecord {
        index: 0,
        kind: ProcKind::Annotate,
        label: "annotate · add-20260711-114749-utc-ufakca".into(),
        status,
        note: Some("summarizing…".into()),
        detail: None,
        fail_reason: None,
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: Some("/tmp/casts/add-20260711-114749-utc-ufakca.cast".into()),
        harness: Some("cursor".into()),
        skill_name: None,
        model: Some("composer".into()),
        started_at: Some(1),
        elapsed: None,
        lines: vec![],
      }],
      workflow: None,
      parent_session: None,
    },
  );
  store
}

#[test]
fn annotate_rows_render_slim_without_the_retired_terminal_chrome() {
  // An annotate proc without a recording has no streamed lines. Its row keeps the
  // summary line and status, but must NOT wear the recorded-terminal chrome — no
  // auto-scroll checkbox and no empty "No output." box. Force stop appears only while
  // the row is live (finished rows drop the button entirely).
  for status in [ProcStatus::Running, ProcStatus::Ok, ProcStatus::Fail] {
    let html = session_page(&store_with_annotate_proc(status), "annjob").expect("session page");
    let procs = session_procs_html(&html);
    assert!(procs.contains("annotate · add-20260711-114749-utc-ufakca"), "row renders: {procs}");
    if status == ProcStatus::Running {
      assert!(procs.contains("Stop annotation"), "annotation has its own stop action while live");
    } else {
      assert!(!procs.contains("Stop annotation"), "no stop action on a settled row ({status:?})");
    }
    assert!(!procs.contains("autoscroll-ctl"), "no auto-scroll control on a slim row ({status:?}): {procs}");
    assert!(!procs.contains(r#"<div class="chamfer output">"#), "no output box on a slim row ({status:?}): {procs}");
    assert!(!procs.contains("No output"), "no empty-output placeholder ({status:?}): {procs}");
  }
  // The live-update path renders the same slim shape, so a WS tick never grows the
  // chrome back: the client builds the output box only once log lines exist.
  let js = live_client_js();
  assert!(
    js.contains("lines.length ? '<div class=\"chamfer output\">'"),
    "procHtml keeps line-less procs slim"
  );
  assert!(js.contains("if (!lines.length || hasCast(p)) return;"), "syncProcOutput never creates an empty box");
  assert!(!js.contains("No output yet."), "the retired empty-output placeholder is gone from the client");
}

#[test]
fn annotation_control_links_to_the_job_and_persists_its_state() {
  let js = live_client_js();
  assert!(js.contains("meta.annotation_job"), "the player reads the annotation job id");
  assert!(js.contains("#proc-' + Number(meta.annotation_proc"), "the link targets the annotating run");
  assert!(js.contains("annotation-link--' + status"), "running/ok/fail each retain a status class");
  assert!(js.contains("annotation-dots"), "running annotation gets animated dots");
  assert!(js.contains("SESSION_START_TIMEOUT_SECS = 30"), "startup has one short deadline");
  assert!(
    js.contains("SESSION_IDLE_TIMEOUT_SECS = 20 * 60"),
    "started work gets a 20-minute idle allowance"
  );
  assert_eq!(
    crate::daemon::model::SESSION_IDLE_TIMEOUT_SECS,
    crate::config::DEFAULT_INACTIVITY_TIMEOUT_SECS,
    "browser and harnesses must agree on running-idle timeout"
  );
  assert!(js.contains("session.lifecycle && session.lifecycle !== 'running'"), "terminal lifecycle comes from the daemon");
  assert!(
    js.contains("sessionLifecycle(candidate, Date.now() / 1000).class === 'running'"),
    "annotation rendering consumes the shared job lifecycle"
  );
  assert!(js.contains("CHAPTERS_WAIT_SECS"), "the poll window is still bounded");
  assert!(js.contains("renderAnnotationLink(box, meta)"), "a late-registering job links up mid-poll");
}

#[test]
fn annotation_child_sessions_belong_to_the_parent_job_not_job_lists() {
  let mut store = store_with_cast_proc(ProcStatus::Ok);
  let mut annotation = store.sessions["castab"].clone();
  annotation.id = "annjob".into();
  annotation.profile = Some("annotate-cast".into());
  annotation.parent_session = Some("castab".into());
  annotation.procs[0].kind = ProcKind::Annotate;
  annotation.procs[0].cast_path = None;
  annotation.procs[0].annotate_target = Some("/tmp/x.cast".into());
  store.sessions.insert(annotation.id.clone(), annotation);

  let html = super::index_page(&store);
  assert!(html.contains(r#"href="/job/castab""#), "parent remains a listed job");
  assert!(!html.contains(r#"href="/job/annjob""#), "annotation child is absent from Jobs and Projects");

  let js = live_client_js();
  assert!(js.contains("if (!s || s.parent_session) return;"), "live Jobs updates omit annotation children");
  assert!(js.contains("s.parent_session || !s.repo"), "live Projects updates omit annotation children");
  assert!(
    js.contains("jobId !== session.id && candidate.parent_session !== session.id"),
    "the hidden child still supplies annotation state to its parent job"
  );
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
fn cast_growth_notifications_append_in_place() {
  // The session page routes WS messages by type: cast_growth appends the newly recorded
  // suffix to the mounted player IN PLACE (no re-creation, no seek, no reload banner) —
  // smooth live following. Everything else stays on the tick path.
  let js = live_client_js();
  assert!(js.contains("if (msg.type === 'cast_growth') { onCastGrowth(msg); return; }"));
  assert!(js.contains("onWsMessage(JSON.parse(ev.data))"));
  assert!(js.contains("function followCastGrowth"));
  assert!(js.contains("box._player.append(text.slice(prev))"));
  assert!(!js.contains("Recording grew: +"), "the reload banner is gone — growth is invisible and smooth");
  // The standalone player page listens on its own WS connection — but only while the proc
  // runs — and follows growth the same way.
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Running), "castab", 0).expect("player page");
  assert!(page.contains("'cast_growth'"));
  assert!(page.contains("const SESSION = 'castab';"));
  assert!(page.contains("const PROC = 0;"));
  assert!(page.contains("player.append(text.slice(loadedChars))"));
  assert!(!page.contains("Recording grew: +"));
  assert!(page.contains("if (!castRunning) return;"), "no WS connect once the proc finished");
  // The player bundle itself carries the live-follow API the pages rely on.
  assert!(super::PLAYER_JS.contains("Player.prototype.append"), "the vendored player must have append");
  assert!(super::PLAYER_JS.contains("appendCast"), "the DOM-free core must parse appends");
}

#[test]
fn live_follows_from_player_toolbar_not_scsh_chrome() {
  // Session-page embed: no external Live button — the player owns ● Live when running.
  let running =
    super::proc::cast_embed_html("castab", &store_with_cast_proc(ProcStatus::Running).sessions["castab"].procs[0]);
  assert!(!running.contains("data-cast-live"), "session embed has no scsh Live button");
  let done = super::proc::cast_embed_html("castab", &store_with_cast_proc(ProcStatus::Ok).sessions["castab"].procs[0]);
  assert!(!done.contains("data-cast-live"));
  let js = live_client_js();
  assert!(js.contains("function setCastLive(box, on)"));
  assert!(js.contains("box._player.setLive(true)"));
  assert!(js.contains("controls: running ? { live: true } : true"), "running casts enable player Live control");
  assert!(js.contains("live: !!(box._live || running)"), "running casts start declared-live");
  // Standalone page: likewise no page-chrome Live toggle.
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Running), "castab", 0).expect("player page");
  assert!(!page.contains("live-toggle"), "standalone page has no external Live button");
  assert!(page.contains("controls: wantLive ? { live: true } : true"));
  let finished = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(!finished.contains("live-toggle"));
}

#[test]
fn export_html_download_renders_on_both_pages_and_hides_without_frames() {
  // Standalone player page: the download link points at the export endpoint, starts
  // hidden, and rides the same no-frames state as the placeholder.
  let page = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(page.contains(r#"<a id="dl-html" href="/cast/castab/0/export.html" download hidden>⬇ download .html</a>"#));
  assert!(page.contains("document.getElementById('dl-html').hidden = !stats.events;"));
  // Session-page embed: run snapshot lives in `.proc-actions` (hidden until frames);
  // client JS unhides it when the cast has events.
  let session = session_page(&store_with_cast_proc(ProcStatus::Ok), "castab").expect("session page");
  let procs = session_procs_html(&session);
  assert!(
    procs.contains(r#"class="chamfer btn btn--cyan btn--sm proc-snapshot""#)
      && procs.contains(r#"href="/cast/castab/0/export.html" data-cast-export download hidden"#)
      && procs.contains("<span>Run snapshot ⬇</span>"),
    "run snapshot button: {procs}"
  );
  let js = live_client_js();
  assert!(js.contains("ensureProcSnapshot"));
  assert!(js.contains("exportLink.hidden = !stats.events;"));
  assert!(js.contains("Incomplete run ⬇"), "live cast export says incomplete run while running");
  assert!(js.contains("Run snapshot ⬇"), "finished cast export says run snapshot");
}

#[test]
fn session_page_header_offers_the_session_export_download() {
  // Every session gets a whole-job download: cast-less procs remain useful note rows.
  let html = session_page(&store_with_cast_proc(ProcStatus::Ok), "castab").expect("session page");
  assert!(
    html.contains(r#"href="/job/castab/export.html" download"#) && html.contains("session-export"),
    "session export button"
  );
  // No recorded proc anywhere still retains the button and exports the job metadata.
  let mut store = store_with_cast_proc(ProcStatus::Ok);
  store.sessions.get_mut("castab").unwrap().procs[0].cast_path = None;
  let bare = session_page(&store, "castab").expect("session page");
  // (The `.session-export` CSS rule is in the shared shell, so match the anchor itself.)
  assert!(bare.contains("session-export"), "job snapshot remains available without a recording");
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
  let html = wrap_page("scsh sessions", 7274, None, None, "", "<p>body</p>");
  assert!(html.contains("class=\"chamfer daemon-status connecting\""));
  assert!(html.contains(".daemon-status.connecting .dot { background: var(--cyan);"));
  assert!(!html.contains("fonts.googleapis.com"), "offline-first: no CDN fonts (WEB-UI §5)");
  assert!(html.contains("position: sticky"), "status chrome is pinned");
  assert!(html.contains("--daemon-status-height"), "tabs stick flush under a shared status height");
  assert!(!html.contains("top: 3.1rem"), "no hard-coded sticky gap above the tabs");
  assert!(html.contains("width: 100%"), "status chrome spans the viewport");
  assert!(html.contains(r#"class="page-shell""#), "content sits in a centered column under the bar");
  // The bar is a chamfered island in a sticky full-width backdrop; the connection dot
  // is a 45°-rotated square (diamond), not a circle.
  assert!(html.contains(r#"class="daemon-status-wrap""#), "island sits in the sticky wrap");
  assert!(html.contains(".daemon-status .dot {"));
  assert!(html.contains("transform: rotate(45deg);"), "diamond dot");
  assert!(!html.contains("border-radius: 50%"), "no round dots remain");
}

#[test]
fn every_daemon_page_carries_the_inline_favicon() {
  use super::layout::wrap_page;
  // A data: URI, so the dashboard and the standalone player page stay request-free.
  let html = wrap_page("scsh sessions", 7274, None, None, "", "<p>body</p>");
  assert!(html.contains("<link rel=\"icon\" href=\"data:image/svg+xml,"), "dashboard favicon");
  let player = cast_player_page(&store_with_cast_proc(ProcStatus::Ok), "castab", 0).expect("player page");
  assert!(player.contains("<link rel=\"icon\" href=\"data:image/svg+xml,"), "player-page favicon");
  assert!(!player.contains("fonts.googleapis.com"), "cast player page is offline-first too");
}

#[test]
fn wrap_page_serves_valid_css_braces() {
  use super::layout::wrap_page;
  let html = wrap_page("scsh sessions", 7274, None, None, "Hello lede", "<p>body</p>");
  assert!(html.contains(":root {"));
  assert!(!html.contains(":root {{"));
  assert!(html.contains(".daemon-status {"));
  assert!(html.contains(r#"class="page-lede""#), "lede renders in the content column");
  assert!(html.contains("Hello lede"));
  // Status bar is the first body child so it pins full-width at the top; lede follows under it.
  let status_at = html.find(r#"id="daemon-status""#).expect("status bar");
  let shell_at = html.find(r#"class="page-shell""#).expect("page shell");
  let lede_at = html.find(r#"class="page-lede""#).expect("lede");
  assert!(status_at < shell_at && shell_at < lede_at, "chrome, then shell, then lede");
}

#[test]
fn review_round_four_fixes_hold() {
  use crate::daemon::model::OpenRepo;
  // (1) The Projects tab is populated server-side — jobs grouped by repository, plus a
  // "no jobs yet" row for repos opened with none — so it shows on first paint instead of
  // waiting for a full WebSocket snapshot a quiet daemon never sends.
  let mut store = store_with_cast_proc(ProcStatus::Ok);
  store.sessions.get_mut("castab").unwrap().ended_at = Some(5);
  store.open_repo(OpenRepo { path: "/work/empty".into(), opened_at: 9, clean: true });
  let html = super::index_page(&store);
  assert!(html.contains(r#"class="repo-filter-link""#), "project/repo names are filter links");
  assert!(html.contains(r#"title="/tmp/repo""#) || html.contains(r#"title="/tmp/repo">"#), "got: {html}");
  // Prefer a stable substring: the filter href for a non-project path.
  assert!(html.contains("/repo/tmp/repo") || html.contains("href=\"/repo/tmp/repo\""), "repo filter href: {html}");
  // Jobs are grouped by the task they ran, with a compact age stamp per job (the exact
  // age depends on the wall clock, so pin up to the stamp). The link — color and
  // underline — covers EXACTLY the six-letter id, in a fixed font (.job-id): never the
  // badge or the age stamp.
  assert!(
    html.contains(
      r#"<div class="repo-jobgroup"><span class="repo-jobgroup-name">default</span><div class="repo-job"><span class="chamfer session-status completed"><span>completed</span></span> <a class="job-id" href="/job/castab">castab</a> <span class="dim">"#
    ),
    "got: {html}"
  );
  assert!(
    html.contains(r#"title="/work/empty""#)
      && html.contains(r#"href="/repo/work/empty""#)
      && html.contains(r#"no jobs yet"#),
    "got: {html}"
  );
  // (2) Chips and counts carry instant data-tip tooltips, served by the shared floating tip
  // (native title tooltips were reset by every live table re-render).
  assert!(html.contains(r#"<span class="chip-count" data-tip="1 run in this job">1</span>"#), "got: {html}");
  assert!(html.contains(".ui-tip"), "tooltip CSS ships");
  assert!(html.contains("initTips"), "tooltip delegation ships");
  assert!(!super::index_page(&store).contains(r#"hchip--claude hchip--done" title="#), "chips use data-tip, not title");
  // (3) The UI speaks "jobs": table header, breadcrumb, empty states.
  assert!(html.contains("<th>Job</th>"), "got: {html}");
  assert!(!html.contains("<th>Session</th>"));
  // (4) A finished recording advertises WHEN it ended, and the chapters poll is bounded by
  // it — no more eternal "summarizing…" on casts that will never gain chapters.
  let mut ended = store_with_cast_proc(ProcStatus::Ok);
  ended.sessions.get_mut("castab").unwrap().procs[0].elapsed = Some(30.0);
  let shtml = session_page(&ended, "castab").expect("session renders");
  assert!(shtml.contains(r#" data-status="ok" data-kind="skill" data-ended="31">"#), "got: {shtml}");
  assert!(shtml.contains("CHAPTERS_WAIT_SECS"), "bounded summarizing window ships");
  // A still-running recording has no end yet (the session-meta dl has its own unrelated
  // data-ended, so pin the cast box's tag specifically).
  let running = session_page(&store_with_cast_proc(ProcStatus::Running), "castab").expect("session renders");
  assert!(running.contains(r#" data-status="running" data-kind="skill">"#), "got: {running}");
  assert!(!running.contains(r#" data-status="running" data-ended"#), "got: {running}");
  // (5) The per-container button reads "Force stop", not "kill" and without a leading ✕.
  let mut live = store_with_cast_proc(ProcStatus::Running);
  live.sessions.get_mut("castab").unwrap().last_seen_at = crate::daemon::paths::now_unix_secs();
  let shtml = session_page(&live, "castab").expect("session renders");
  assert!(shtml.contains(">Force stop</button>") || shtml.contains("<span>Force stop</span>"), "got: {shtml}");
  assert!(!shtml.contains("✕ Force stop"), "no leading ✕ on Force stop");
  assert!(!shtml.contains("✕ kill"));
  assert!(shtml.contains("<span>Incomplete job ⬇</span>"), "running job export says incomplete job: {shtml}");
  assert!(shtml.contains(r#"class="chamfer btn btn--cyan btn--sm session-export""#), "export matches Force stop size");
  assert!(
    shtml.find("session-export").unwrap() < shtml.find("id=\"session-stop\"").unwrap(),
    "job snapshot sits above Force stop in the actions stack"
  );
  assert!(shtml.contains("Job snapshot ⬇") || shtml.contains("Incomplete job ⬇"), "snapshot wording is explicit");
  // Finished job with a cast but no chapters sidecar → chapters pending (not incomplete).
  let done = session_page(&store_with_cast_proc(ProcStatus::Ok), "castab").expect("done session");
  assert!(
    done.contains("<span>Chapters pending ⬇</span>"),
    "finished job missing sidecar uses chapters pending: {done}"
  );
  assert!(done.contains("1 cast finalizing chapters"), "pending counter on job page: {done}");
  assert!(!done.contains("<span>Incomplete job ⬇</span>"), "finished job meta export is not incomplete");
  assert!(!done.contains("<span>Incomplete run ⬇</span>"), "finished job has no incomplete-run label");
  assert!(!done.contains(r#"<ul class="skills">"#), "no skills list on job page island");
  // Settled: cast + sidecar on disk → job snapshot.
  let dir = std::env::temp_dir().join(format!("scsh-chap-ready-{}", crate::runtime::random_nonce_6()));
  std::fs::create_dir_all(&dir).unwrap();
  let cast = dir.join("ready.cast");
  std::fs::write(&cast, "{\"version\":3}\n").unwrap();
  std::fs::write(dir.join("ready.chapters.json"), r#"{"summary":"ok","chapters":[]}"#).unwrap();
  let mut settled = store_with_cast_proc(ProcStatus::Ok);
  {
    let s = settled.sessions.get_mut("castab").unwrap();
    s.ended_at = Some(10);
    s.procs[0].cast_path = Some(cast.to_string_lossy().into_owned());
  }
  let settled_html = session_page(&settled, "castab").expect("settled session");
  assert!(
    settled_html.contains("<span>Job snapshot ⬇</span>"),
    "settled job export label: {settled_html}"
  );
  assert!(
    !settled_html.contains(r#"id="chapters-pending""#),
    "no pending line when sidecar exists: {settled_html}"
  );
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn review_round_five_fixes_hold() {
  // Projects: running jobs sort above completed ones, grouped by the task that ran, each
  // line stamped with a compact age; the launch tab reads "Run".
  let now = crate::daemon::paths::now_unix_secs();
  let mut store = store_with_cast_proc(ProcStatus::Ok);
  store.sessions.get_mut("castab").unwrap().ended_at = Some(5);
  {
    let done = store.sessions.get("castab").unwrap().clone();
    let mut live = done.clone();
    live.id = "livejb".into();
    live.ended_at = None;
    live.last_seen_at = now;
    live.profile = Some("arith".into());
    live.procs[0].status = ProcStatus::Running;
    store.sessions.insert("livejb".into(), live);
  }
  let html = super::index_page(&store);
  let arith = html.find(r#"<span class="repo-jobgroup-name">arith</span>"#).expect("arith group");
  let default = html.find(r#"<span class="repo-jobgroup-name">default</span>"#).expect("default group");
  assert!(arith < default, "the group with a running job sorts above the finished one: {html}");
  assert!(html.contains(r#"<span class="chamfer session-status running"><span>running</span></span> <a class="job-id" href="/job/livejb">livejb</a> <span class="dim">"#), "got: {html}");
  assert!(html.contains(r#"data-tab="run">Run</button>"#), "got: {html}");
  assert!(!html.contains("New job"));
  assert!(!html.contains("Start a job"));
  // Image-build sessions land under Projects → Internal, not the main Projects table.
  {
    let mut img = store_with_cast_proc(ProcStatus::Ok);
    img.sessions.get_mut("castab").unwrap().repo = crate::daemon::server::IMAGE_BUILDS_REPO.into();
    img.sessions.get_mut("castab").unwrap().profile = Some("build-images".into());
    img.sessions.get_mut("castab").unwrap().ended_at = Some(5);
    let html = super::index_page(&img);
    assert!(html.contains(r#"id="internal-jobs-card""#), "Internal section present: {html}");
    assert!(html.contains(r#"<p class="section-label">Internal</p>"#), "Internal label: {html}");
    assert!(html.contains(r#"<span class="repo-jobgroup-name">build-images</span>"#), "grouped by profile: {html}");
    assert!(html.contains(r#"href="/job/castab""#), "job link in Internal: {html}");
    assert!(
      !html.contains(r#"data-repo="(image builds)""#),
      "image builds excluded from Projects table: {html}"
    );
  }
  // Short ages are single-unit; both renderers ship the same helper and group markup.
  assert_eq!(super::format::format_short_age(45), "45s");
  assert_eq!(super::format::format_short_age(200), "3m");
  assert_eq!(super::format::format_short_age(7300), "2h");
  assert_eq!(super::format::format_short_age(200_000), "2d");
  assert!(html.contains("function formatShortAge"), "JS mirror ships");
  assert!(html.contains(".repo-jobgroup"), "group CSS ships");
  // The inline player pane has NO forced height — the player sizes its own box to the
  // recording's aspect at full width, so the pane is exactly as tall as the terminal wants
  // (the page-side sizeCastPane workaround is gone).
  let shtml = session_page(&store, "castab").expect("session renders");
  assert!(!shtml.contains("sizeCastPane"), "the pane-sizing workaround must stay gone");
  assert!(!shtml.contains(".cast-player { width: 100%; height:"), "no forced pane height");
  assert!(
    shtml.contains(r#".cast-player [part~="screen-box"]"#)
      && shtml.contains("width: 100%; min-width: 0; max-width: 100%;"),
    "wide terminals must be budgeted to the visible player pane: {shtml}"
  );
  assert!(!shtml.contains("fullscreenEl: box"), "fullscreen must contain only the player, not the scsh cast card");
  assert!(!shtml.contains(".cast:fullscreen"), "the outer cast card is never the fullscreen element");
  assert!(
    shtml.contains("button.proc-kill") && shtml.contains("color: var(--text); background: var(--red); border: none;"),
    "Force stop must keep its red chamfered border despite button.btn specificity: {shtml}"
  );
}

#[test]
fn project_and_repo_filter_urls_normalize_extra_slashes() {
  use super::index::{parse_index_filter, IndexFilter};
  assert_eq!(parse_index_filter("/project//demo-1/"), Some(IndexFilter::Project("demo-1".into())));
  assert_eq!(parse_index_filter("/project/demo-1"), Some(IndexFilter::Project("demo-1".into())));
  assert_eq!(parse_index_filter("/project/"), None);
  assert_eq!(parse_index_filter("/project"), None);
  assert_eq!(parse_index_filter("/repo///Users/dima/foo/"), Some(IndexFilter::Repo("/Users/dima/foo".into())));
  assert_eq!(parse_index_filter("/repo/Users/dima/foo"), Some(IndexFilter::Repo("/Users/dima/foo".into())));
  assert_eq!(parse_index_filter("/repo/tmp/my%20repo"), Some(IndexFilter::Repo("/tmp/my repo".into())));
  assert_eq!(parse_index_filter("/repo/"), None);
}

#[test]
fn filtered_index_page_shows_only_matching_repo_and_opens_projects_tab() {
  use super::index::{index_page_with_filter, IndexFilter};
  let mut store = store_with_cast_proc(ProcStatus::Ok);
  store.sessions.get_mut("castab").unwrap().repo = "/tmp/repo".into();
  store.sessions.get_mut("castab").unwrap().ended_at = Some(5);
  {
    let mut other = store.sessions.get("castab").unwrap().clone();
    other.id = "other1".into();
    other.repo = "/tmp/other".into();
    store.sessions.insert("other1".into(), other);
  }
  let html = index_page_with_filter(&store, Some(IndexFilter::Repo("/tmp/repo".into())));
  assert!(html.contains(r#"class="tab active" data-tab="projects""#), "Projects tab active when filtered: {html}");
  assert!(html.contains("filter-banner"), "filter banner present");
  assert!(html.contains("Show all"), "clear-filter link");
  assert!(html.contains("href=\"/projects\""), "Show all clears to /projects");
  assert!(html.contains("castab"), "matching job shown");
  assert!(!html.contains(">other1<") && !html.contains("/job/other1"), "other repo's job hidden");
  assert!(html.contains(r#"class="repo-filter-link""#));
}

#[test]
fn review_round_six_fixes_hold() {
  let store = store_with_cast_proc(ProcStatus::Ok);
  let html = super::index_page(&store);
  // Durations can never render backwards: stale tick frames are dropped, and a superseded
  // WebSocket is fully retired before a reconnect (the "oscillating Duration" bug).
  assert!(html.contains("lastTickSecs"), "monotonic tick guard ships");
  assert!(html.contains("Retire any superseded socket"), "socket retirement ships");
  // The runtime switcher is a segmented control above the images table, not loose buttons
  // in the action strip; tips are multi-line and can tick a live running-for line.
  assert!(html.contains(r#"<div id="images-runtimes" class="images-runtimes"></div>"#), "got: {html}");
  assert!(html.contains(".seg-opt"), "segmented-control CSS ships");
  // The group is a chamfered ring: the outer layer shows through the padding/gaps, and
  // the corner options clip their own inner chamfer so the ring survives the diagonals.
  assert!(html.contains(r#"'<span class="chamfer seg" data-tip="#), "runtime switcher group is chamfered");
  assert!(html.contains("--seg-inner: calc(var(--cut) - var(--bw) * 0.5858);"));
  assert!(html.contains(".seg-opt:first-child {"), "corner options carry their own clip");
  // Active tabs are underlined by a trapezoid (45° ends), not a plain border.
  assert!(html.contains(".tab.active::after {"));
  assert!(html.contains("clip-path: polygon(3px 0%, calc(100% - 3px) 0%, 100% 100%, 0% 100%);"));
  assert!(!html.contains("border-bottom: 2px solid transparent"), "no rounded-era tab underline");
  assert!(html.contains("data-tip-running"), "live-ticking tip support ships");
  assert!(html.contains("white-space: pre-line"), "multi-line tip CSS ships");
  // Both JS chip-count writers share one renderer, so live re-syncs keep the tooltip.
  assert!(html.contains("function chipCountHtml"), "shared chip-count renderer ships");
}

#[test]
fn workflow_graph_renders_builtin_shapes() {
  use crate::daemon::workflow::{WorkflowMeta, WorkflowNodeMeta};
  fn skill_proc(index: usize, id: &str, harness: &str, status: ProcStatus) -> ProcRecord {
    ProcRecord {
      index,
      kind: ProcKind::Skill,
      label: format!("{harness}: {id}"),
      status,
      note: None,
      detail: None,
      fail_reason: None,
      container_name: None,
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: Some(id.into()),
      route: None,
      result_path: None,
      annotate_target: None,
      harness: Some(harness.into()),
      skill_name: Some(id.into()),
      model: None,
      started_at: Some(1),
      elapsed: Some(1.0),
      lines: vec![],
    }
  }
  // arith: add + multiply → summarize
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "arith1".into(),
    Session {
      id: "arith1".into(),
      started_at: 1,
      ended_at: Some(10),
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        skill_proc(0, "add", "claude", ProcStatus::Ok),
        skill_proc(1, "multiply", "codex", ProcStatus::Ok),
        skill_proc(2, "summarize", "grok", ProcStatus::Ok),
      ],
      last_seen_at: 10,
      client_connected: false,
      run_pid: None,
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "add".into(),
            proc_index: Some(0),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "multiply".into(),
            proc_index: Some(1),
            order: 1,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "summarize".into(),
            proc_index: Some(2),
            order: 2,
            needs: vec!["add".into(), "multiply".into()],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    },
  );
  store.sessions.get_mut("arith1").unwrap().procs[2].elapsed = Some(198.0);
  let html = session_page(&store, "arith1").expect("page");
  assert!(html.contains(r#"id="workflow-graph""#), "workflow card present");
  assert!(html.contains("3 tasks · "), "summary starts with task count");
  assert!(html.contains(r#">3 succeeded</a>"#), "summary counts successful tasks unambiguously");
  assert!(html.contains(r#"class="wf-jump""#), "status counters are jump links");
  assert!(html.contains("Jump to first succeeded task"), "success counter links to a successful node");
  assert!(
    html.contains("href=\"#task-add\"") || html.contains("href=\"#task-multiply\""),
    "done jump targets a real node"
  );
  assert!(!html.contains("dependencies</p>"), "summary must not say N dependencies");
  assert!(!html.contains(r#"class="workflow-summary dim">3 tasks · 2 dependencies"#));
  assert!(html.contains(r#"data-workflow-step="add""#));
  assert!(html.contains(r#"data-workflow-step="multiply""#));
  assert!(html.contains(r#"data-workflow-step="summarize""#));
  assert!(html.contains(r#"id="task-add""#));
  assert!(html.contains("href=\"#task-summarize\""));
  assert!(html.contains(r#"class="chamfer wf-bookend wf-start""#), "Start bookend on multi-node graphs too");
  assert!(html.contains(r#"class="chamfer wf-bookend wf-finish""#), "Finish bookend");
  // Two DAG fan-in edges + Start→add + Start→multiply + summarize→Finish.
  let graph = html.split(r#"id="workflow-graph""#).nth(1).expect("graph card");
  let graph = graph.split("</svg>").next().expect("svg");
  assert_eq!(graph.matches("marker-end=\"url(#wf-arrow)\"").count(), 5);
  assert!(html.contains(r#"class="wf-arrowhead""#), "open chevron arrowheads, not filled triangles");
  let curved = graph
    .split(r#"class="wf-edge" d=""#)
    .filter_map(|part| part.split('"').next())
    .find(|path| path.contains(" C"))
    .expect("fan-in graph has a curved cross-row edge");
  assert!(curved.starts_with('M') && curved.contains(" L") && curved.rsplit_once(" L").is_some(), "{curved}");
  assert!(curved.split(" C").nth(1).unwrap_or("").contains(" L"), "arrow has a horizontal entry runway: {curved}");
  // Fan-in ports land at distinct y on summarize (not a single shared tip).
  let mut ends: Vec<(String, String)> = Vec::new();
  for part in graph.split(r#"class="wf-edge" d=""#) {
    if !part.contains(r#"marker-end="url(#wf-arrow)""#) {
      continue;
    }
    let Some(d) = part.split('"').next() else {
      continue;
    };
    // Path ends `… x2,y2` — last comma-separated pair.
    let Some((x, y)) = d.rsplit_once(',') else {
      continue;
    };
    let x = x.rsplit([' ', 'C']).next().unwrap_or(x);
    ends.push((x.to_string(), y.to_string()));
  }
  assert_eq!(ends.len(), 5, "expected five edges, got {ends:?}");
  // The two edges into summarize share an end x and differ in y.
  let mut by_x: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
  for (x, y) in ends {
    by_x.entry(x).or_default().push(y);
  }
  let fan_in = by_x.values().find(|ys| ys.len() == 2).expect("summarize fan-in pair missing");
  assert_ne!(fan_in[0], fan_in[1], "fan-in edges must enter at different heights: {fan_in:?}");

  // All-successful graph: show one overall verdict and only the Succeeded legend state.
  assert!(html.contains(r#"workflow-outcome--completed"#));
  assert!(html.contains(">Job succeeded</span>"));
  assert!(html.contains("> Succeeded</li>"));
  assert!(!html.contains("> Done</li>"));
  assert!(html.contains(r#"<li class="wf-leg wf-leg-done""#));
  assert!(!html.contains(r#"<li class="wf-leg wf-leg-running""#));
  assert!(!html.contains(r#"<li class="wf-leg wf-leg-waiting""#));
  assert!(!html.contains(r#"<li class="wf-leg wf-leg-failed""#));
  assert!(!html.contains(r#"<li class="wf-leg wf-leg-stalled""#));
  assert!(!html.contains(r#"<li class="wf-leg wf-leg-skipped""#));
  assert!(
    html.contains(r#"<span class="wf-state-label">Succeeded</span><span class="wf-state-elapsed"> · 3m18s</span>"#),
    "duration sits beside status in compact clock form"
  );
  let graph_card = html.split(r#"id="workflow-graph""#).nth(1).unwrap();
  let graph_head = graph_card.split(r#"<div class="workflow-visual">"#).next().unwrap();
  let graph_visual = graph_card.split(r#"<div class="workflow-visual">"#).nth(1).unwrap();
  assert!(!graph_head.contains("workflow-legend"), "task legend must not read as part of the job-level header");
  assert!(
    graph_visual.find("workflow-legend") < graph_visual.find("workflow-scroll"),
    "legend overlays the visual before its scroll viewport"
  );

  {
    let session = store.sessions.get_mut("arith1").unwrap();
    session.ended_at = None;
    session.client_connected = true;
    session.last_seen_at = crate::daemon::paths::now_unix_secs();
  }
  let finalizing = session_page(&store, "arith1").expect("finalizing page");
  assert!(finalizing.contains(">Finalizing recordings</span>"), "all tasks done while casts settle is explicit");

  // fruits fan-out — live session so Waiting→Ready (deps met) is not collapsed to Stalled
  let now = crate::daemon::paths::now_unix_secs();
  store.sessions.insert(
    "fruit1".into(),
    Session {
      id: "fruit1".into(),
      started_at: now.saturating_sub(5),
      ended_at: None,
      profile: Some("fruits".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        skill_proc(0, "categorize", "claude", ProcStatus::Ok),
        skill_proc(1, "sort_fruits", "claude", ProcStatus::Waiting),
        skill_proc(2, "sort_vegetables", "claude", ProcStatus::Waiting),
      ],
      last_seen_at: now,
      client_connected: true,
      run_pid: Some(1),
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "categorize".into(),
            proc_index: Some(0),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "sort_fruits".into(),
            proc_index: Some(1),
            order: 1,
            needs: vec!["categorize".into()],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "sort_vegetables".into(),
            proc_index: Some(2),
            order: 2,
            needs: vec!["categorize".into()],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    },
  );
  let fruits = session_page(&store, "fruit1").expect("fruits");
  assert!(
    fruits.contains(r#">1 succeeded</a>"#) && fruits.contains(r#">2 ready</a>"#),
    "ready stays separate from waiting in the headline"
  );
  assert!(fruits.contains("data-tip="), "nodes carry instant tooltips");
  assert!(fruits.contains("Ready — dependencies finished; not started yet"), "ready tip explains why the node is idle");
  // 2 dependency edges + start → categorize + both sorts → finish.
  assert_eq!(
    fruits
      .split(r#"id="workflow-graph""#)
      .nth(1)
      .unwrap()
      .split("</svg>")
      .next()
      .unwrap()
      .matches("marker-end=\"url(#wf-arrow)\"")
      .count(),
    5 // categorize→2 sorts + Start→categorize + 2 sinks→Finish
  );
  assert!(fruits.contains(r#"data-workflow-step="categorize""#));

  // code-review conditional gate
  store.sessions.insert(
    "rev001".into(),
    Session {
      id: "rev001".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("code-review".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        skill_proc(0, "probe_credentials", "claude", ProcStatus::Ok),
        skill_proc(1, "review", "claude", ProcStatus::Skipped),
      ],
      last_seen_at: 1,
      client_connected: true,
      run_pid: Some(1),
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "probe_credentials".into(),
            proc_index: Some(0),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "review".into(),
            proc_index: Some(1),
            order: 1,
            needs: vec!["probe_credentials".into()],
            conditional: true,
            when_summary: Some("Runs only if probe_credentials.ok = true".into()),
          },
        ],
      }),
      parent_session: None,
    },
  );
  let review = session_page(&store, "rev001").expect("review");
  assert!(review.contains(r#"class="chamfer wf-gate""#), "gate marker");
  assert!(review.contains(">when</span>"), "gate label is the word when, not a diamond");
  assert!(
    review.contains("Runs only when its gate passes"),
    "gate tooltip is generic — no raw gate literals in the browser"
  );
  // Node ids may appear; gate *expressions* must not.
  assert!(!review.contains("probe_credentials.ok"), "no gate operand leakage");
  assert!(!review.contains("Runs only if"), "no authored when_summary in the page");
  assert!(!review.contains("Conditional task"), "no cryptic Conditional task label");
  // (Waiting/ready node icons ARE diamonds now; the gate itself is pinned to the word
  // "when" above, so no separate no-diamond check on the page.)
  assert!(review.contains(r#"wf-skipped"#));
  // 1 dependency edge + start → probe_credentials + review → finish.
  assert_eq!(
    review
      .split(r#"id="workflow-graph""#)
      .nth(1)
      .unwrap()
      .split("</svg>")
      .next()
      .unwrap()
      .matches("marker-end=\"url(#wf-arrow)\"")
      .count(),
    3 // Start→root + one DAG edge + sink→Finish
  );

  // Waiting tip names the blocker (WEB-UI §4 disclosure; not a bare "1 waiting on").
  let now = crate::daemon::paths::now_unix_secs();
  store.sessions.insert(
    "wait1".into(),
    Session {
      id: "wait1".into(),
      started_at: now.saturating_sub(5),
      ended_at: None,
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        skill_proc(0, "add", "claude", ProcStatus::Running),
        skill_proc(1, "summarize", "grok", ProcStatus::Waiting),
      ],
      last_seen_at: now,
      client_connected: true,
      run_pid: Some(1),
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "add".into(),
            proc_index: Some(0),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "summarize".into(),
            proc_index: Some(1),
            order: 1,
            needs: vec!["add".into()],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    },
  );
  let waiting = session_page(&store, "wait1").expect("waiting");
  assert!(waiting.contains("Waiting on:"), "waiting tip explains blockers");
  assert!(waiting.contains("waiting on add"), "meta line names the blocker");
  assert!(waiting.contains("margin: auto"), "graph stage centers on both axes when it fits");

  // Force-stopped is distinct from a natural failure (✕ vs ✗) but shares the fail/red accent.
  store.sessions.insert(
    "stop1".into(),
    Session {
      id: "stop1".into(),
      started_at: now.saturating_sub(30),
      ended_at: Some(now),
      profile: Some("demo-pr".into()),
      kind: Some("definition".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        {
          let mut p = skill_proc(0, "cursor-build", "cursor", ProcStatus::Fail);
          p.fail_reason = Some(crate::failure::reason::FORCE_STOPPED.into());
          p.detail = Some("stopped from the session browser".into());
          p
        },
        {
          let mut p = skill_proc(1, "claude-run", "claude", ProcStatus::Fail);
          p.fail_reason = Some(crate::failure::reason::HARNESS_NONZERO.into());
          p
        },
      ],
      last_seen_at: now,
      client_connected: false,
      run_pid: None,
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "cursor-build".into(),
            proc_index: Some(0),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "claude-run".into(),
            proc_index: Some(1),
            order: 1,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    },
  );
  let stopped = session_page(&store, "stop1").expect("stopped page");
  let summary =
    stopped.split(r#"class="workflow-summary dim">"#).nth(1).and_then(|s| s.split("</p>").next()).unwrap_or("?");
  assert!(stopped.contains(r#"wf-stopped"#), "stopped node class; summary={summary}");
  assert!(stopped.contains("Stopped"), "stopped label; summary={summary}");
  assert!(summary.contains("stopped"), "summary counts stopped separately: {summary}");
  assert!(summary.contains("failed"), "natural failure stays failed: {summary}");
  assert!(stopped.contains("wf-leg-stopped"), "legend lists stopped");
  assert!(stopped.contains(r#"workflow-outcome--failed"#), "mixed terminal states have one failed job verdict");
  assert!(stopped.contains(">Job failed</span>"), "overall failure is explicit above the task legend");

  {
    let session = store.sessions.get_mut("stop1").unwrap();
    session.ended_at = None;
    session.client_connected = true;
    session.procs[0].status = ProcStatus::Running;
    session.procs[0].fail_reason = Some(crate::failure::reason::STOP_REQUESTED.into());
    session.procs[0].detail = Some("terminating container from the session browser".into());
  }
  let terminating = session_page(&store, "stop1").expect("terminating page");
  assert!(terminating.contains(r#"wf-terminating"#), "graph task turns orange while teardown runs");
  assert!(terminating.contains("Terminating"), "graph task names the intermediate state");

  // Flat skill session (no authored DAG): still gets a job graph from its skill proc,
  // bookended Start → task → Finish so even a single-run job shows arrows.
  let flat = store_with_cast_proc(ProcStatus::Ok);
  let flat_html = session_page(&flat, "castab").expect("flat");
  assert!(flat_html.contains("· 1 task</p>"), "single-task lede has no terminal punctuation");
  assert!(!flat_html.contains("· 1 task.</p>"), "single-task lede is not punctuated as a sentence");
  assert!(flat_html.contains(r#"id="workflow-graph""#), "every job with skills gets a graph");
  assert!(flat_html.contains("Job graph"), "card title is Job graph");
  assert!(flat_html.contains("card--accent-left-orange workflow-card"), "graph island uses the orange accent");
  assert!(!flat_html.contains("card--accent-left-cyan workflow-card"), "graph no longer competes with cyan summaries");
  assert!(flat_html.contains(r#"data-workflow-step="add""#) || flat_html.contains("wf-node"), "skill node present");
  assert!(flat_html.contains(r#"class="chamfer wf-bookend wf-start""#), "Start bookend");
  assert!(flat_html.contains(r#"class="chamfer wf-bookend wf-finish""#), "Finish bookend");
  assert!(flat_html.contains("wf-start-play"), "play-triangle start glyph");
  assert!(flat_html.contains("wf-finish-flag"), "checkered finish flag");
  assert!(flat_html.contains("scrollbar-width: none"), "graph remains scrollable without visible scrollbar chrome");
  assert!(flat_html.contains(".workflow-scroll::-webkit-scrollbar"), "WebKit scrollbar chrome is hidden too");
  assert!(flat_html.contains("data-wf-zoom-fit>Fit</button>"), "server-rendered graph includes Fit");
  assert!(flat_html.contains("data-wf-expand"), "server-rendered graph includes the large-view control");
  assert!(flat_html.contains(">Full screen</button>"), "large-view control has a clear label");
  assert!(flat_html.contains("height: 29rem"), "normal graph viewport is about 60% of its former 48rem height");
  assert!(flat_html.contains(".workflow-card.wf-expanded"), "large view is an inset card, not the browser Fullscreen API");
  assert!(!flat_html.contains("wf-selected"), "task links do not leave a persistent graph-side selection state");
  assert!(!flat_html.contains("wf-start-line"), "dashed race-line start glyph is gone");
  assert!(!flat_html.contains("wf-bookend-label"), "bookends are icon-only");
  let edge_count = flat_html.matches(r#"class="wf-edge""#).count();
  assert!(edge_count >= 2, "single-run job still has Start→task and task→Finish edges: {edge_count}");
  // Same-row bookend edges must be straight horizontals (not S-curve cubics).
  let flat_graph = flat_html.split(r#"id="workflow-graph""#).nth(1).expect("flat graph");
  let flat_graph = flat_graph.split("</svg>").next().expect("flat svg");
  let flat_edges: Vec<&str> = flat_graph
    .split(r#"class="wf-edge" d=""#)
    .skip(1)
    .filter_map(|p| p.split('"').next())
    .collect();
  assert!(flat_edges.len() >= 2, "flat edges: {flat_edges:?}");
  for d in &flat_edges {
    assert!(d.contains(" L"), "same-row edge must be horizontal L, got {d}");
    assert!(!d.contains(" C"), "same-row edge must not be a cubic S-curve, got {d}");
  }

  // Client wiring
  let js = live_client_js();
  assert!(js.contains("function updateWorkflowGraph"));
  assert!(js.contains("function activateWorkflowTask"));
  assert!(js.contains("function initWorkflowGraph"));
  assert!(js.contains("function wfLegendHtml"));
  assert!(js.contains("function wfBuildGraphHtml"), "late graph creation without reload");
  assert!(js.contains("function wfLayoutWithBookends"), "live graph mirrors Start/Finish");
  assert!(js.contains("function wfLoopIslandsHtml"), "dynamic repeat iterations share a loop island");
  assert!(js.contains("function wfLoopProgressText"), "loop islands explain whether more iterations can follow");
  assert!(js.contains("session.workflow_loops"), "live loop copy reads authored iteration bounds");
  assert!(js.contains("may continue · up to "), "agent-decided loops stay visibly open-ended while running");
  assert!(js.contains("wf-loop-progress"), "loop continuation is a distinct visual element");
  assert!(js.contains("repeat|while-"), "repeat and do-while iterations share the dash loop id scheme");
  assert!(!js.contains("__repeat") && !js.contains("__while"), "the double-underscore id scheme is gone");
  assert!(js.contains("'do-while · '"), "do-while islands are labeled as do-while, not repeat");
  assert!(js.contains("' → '"), "a multi-step do-while island is named for its whole body (first → final)");
  assert!(js.contains("data-wf-zoom-in"), "graph has explicit zoom controls");
  assert!(js.contains("data-wf-zoom-fit"), "graph has a Fit control");
  assert!(js.contains("function wfFitZoom"), "Fit has one shared two-axis calculation");
  assert!(js.contains("return Math.min(widthZoom, heightZoom);"), "Fit uses the tighter bound and scales up as well as down");
  assert!(js.contains("Math.max(minimum, Math.min(maximum, next))"), "zoom-out stops at the fitted lower bound");
  assert!(js.contains("const minimum = Math.min(1, fit);"), "the zoom-out floor never exceeds 100%");
  // Fit fits the viewport AS IT IS NOW: the manual 2x ceiling lifts when the live fit
  // factor exceeds it (window resized, full screen toggled) — never a remembered size.
  assert!(js.contains("const maximum = Math.max(2, fit);"), "the ceiling follows the live fit factor");
  assert!(js.contains("const fit = wfFitZoom(scroller, stage);"), "bounds recompute from the live viewport");
  assert!(js.contains("zoomOut.disabled"), "the zoom-out control advertises when Fit is the lower bound");
  assert!(js.contains("scroller.scrollTop = 0"), "Fit resets both scroll axes before CSS centers the graph");
  assert!(flat_html.contains("display: flex"), "the graph viewport can center spare space on either axis");
  assert!(flat_html.contains("margin: auto"), "the stage centers only along axes with spare room");
  assert!(
    !flat_html.contains(".workflow-stage { position: relative; flex: 0 0 auto; min-height:"),
    "the stage height follows the actual graph so a small graph is vertically centered"
  );
  assert!(js.contains("data-wf-expand"), "graph has a large-view control");
  assert!(js.contains("let workflowExpanded = false"), "large view survives dynamic graph remounts");
  assert!(js.contains("aria-modal"), "large graph view exposes modal semantics");
  assert!(js.contains("current.contains(ev.target)"), "clicks inside the large graph keep it open");
  assert!(
    js.contains("current.__scshApplyWorkflowExpanded(false, false)"),
    "a click on the modal backdrop closes the large graph without stealing focus"
  );
  assert!(js.contains("ev.key !== 'Escape'"), "Escape closes the large graph view");
  assert!(js.contains("ev.key === 'Tab'"), "keyboard focus stays inside the large graph view");
  assert_eq!(
    js.matches("if (workflowExpanded) applyExpanded(false, false);").count(),
    2,
    "both graph run links and status-summary run links close large view before navigation"
  );
  assert!(!js.contains("requestFullscreen"), "large view deliberately avoids the browser Fullscreen API");
  assert!(js.contains("stage.style.zoom"), "zoom changes the graph without changing its topology");
  assert!(js.contains("let workflowZoom = 1"), "zoom survives dynamic graph remounts");
  assert!(!js.contains("window.scrollBy"), "the page viewport never moves except on direct human input");
  assert!(!js.contains("scroller.style.height"), "zoom scales inside the fixed viewport, never resizing the card");
  assert!(js.contains("scroll !== false"), "data-driven panel activation opens without scrolling the page");
  assert!(js.contains("function wfNodeTip"), "useful node tooltips");
  assert!(js.contains("function wfSummaryHtml"), "status counters are jump links");
  assert!(js.contains("a.wf-jump"), "summary jump click wiring");
  assert!(js.contains("history.pushState"), "task clicks push history");
  assert!(js.contains("pendingWorkflowStep"), "pre-registration pending selection");
  assert!(js.contains("Task details are not available yet"), "pending status copy");
}

#[test]
fn workflow_loop_island_advertises_future_iterations() {
  use crate::daemon::workflow::workflow_meta_from_def;
  use crate::harness_def::{builtin_defs, validate, DefSource};

  let session_for = |profile: &str| {
    let (_, src) = builtin_defs().into_iter().find(|(name, _)| *name == profile).expect("builtin loop");
    let def = validate(profile, src, DefSource::Builtin).expect("valid builtin loop");
    let now = crate::daemon::paths::now_unix_secs();
    Session {
      id: "loopmore".into(),
      started_at: now.saturating_sub(5),
      ended_at: None,
      profile: Some(profile.into()),
      kind: Some("workflow".into()),
      repo: "/tmp/loop-more".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![],
      last_seen_at: now,
      client_connected: true,
      run_pid: Some(1),
      workflow: workflow_meta_from_def(&def),
      parent_session: None,
    }
  };

  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  let do_while = session_for("demo-loop-do-while");
  let api = crate::daemon::jsonio::session_json_api(&do_while);
  assert!(api.contains(r#""workflow_loops": [{ "id": "compare", "max_iterations": 25, "exact": false }]"#));
  store.sessions.insert("loopmore".into(), do_while);
  let open_ended = session_page(&store, "loopmore").expect("do-while page");
  assert!(open_ended.contains(r#"class="chamfer wf-loop-progress""#));
  assert!(open_ended.contains("↻ may continue · up to 24 more"));

  let mut fixed = session_for("demo-loop-repeat");
  fixed.id = "fixedlp".into();
  store.sessions.insert("fixedlp".into(), fixed);
  let exact = session_page(&store, "fixedlp").expect("repeat page");
  assert!(exact.contains("↻ 2 more iterations planned"));
}

#[test]
fn workflow_graph_bookends_runs_with_start_and_finish_terminals() {
  use crate::daemon::workflow::{WorkflowMeta, WorkflowNodeMeta};
  // A job with a single run reads start → run → finish: exactly two arrows.
  let mut store = Store::new(DaemonMode::Persistent, 7274, 1);
  store.sessions.insert(
    "solo1".into(),
    Session {
      id: "solo1".into(),
      started_at: 1,
      ended_at: Some(10),
      profile: Some("solo".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![ProcRecord {
        index: 0,
        kind: ProcKind::Skill,
        label: "claude: add".into(),
        status: ProcStatus::Ok,
        note: None,
        detail: None,
        fail_reason: None,
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: Some("add".into()),
        route: None,
        result_path: None,
        annotate_target: None,
        harness: Some("claude".into()),
        skill_name: Some("add".into()),
        model: None,
        started_at: Some(1),
        elapsed: Some(1.0),
        lines: vec![],
      }],
      last_seen_at: 10,
      client_connected: false,
      run_pid: None,
      workflow: Some(WorkflowMeta {
        nodes: vec![WorkflowNodeMeta {
          id: "add".into(),
          proc_index: Some(0),
          order: 0,
          needs: vec![],
          conditional: false,
          when_summary: None,
        }],
      }),
      parent_session: None,
    },
  );
  let html = session_page(&store, "solo1").expect("page");
  let graph = html.split(r#"id="workflow-graph""#).nth(1).expect("graph card");
  let svg = graph.split("</svg>").next().expect("svg");

  // Play-triangle Start and checkered-flag Finish bookends are present and decorative.
  assert!(graph.contains(r#"class="chamfer wf-bookend wf-start""#), "start bookend markup");
  assert!(graph.contains(r#"class="chamfer wf-bookend wf-finish""#), "finish bookend markup");
  assert!(graph.contains("wf-start-play"), "play-triangle start glyph");
  assert!(graph.contains("wf-finish-flag"), "checkered finish flag");
  // Bookends are divs, not links — they must never read (or click) as runs.
  assert!(!graph.contains(r#"wf-bookend wf-start" href"#), "start is not a link");
  assert!(!graph.contains(r#"wf-bookend wf-finish" href"#), "finish is not a link");
  assert!(!graph.contains(r#"wf-bookend wf-start" tabindex"#), "start never steals focus");

  // Exactly two arrows for a single-run job: start → add, add → finish. Same-row edges
  // draw as straight horizontals, not S-curve cubics (skip the arrowhead in <defs>).
  let edges = svg.split("</defs>").last().expect("edge paths");
  assert_eq!(edges.matches("marker-end=\"url(#wf-arrow)\"").count(), 2, "start → run → finish is two arrows");
  assert_eq!(edges.matches(" L").count(), 2, "same-row bookend edges are horizontal lines");
  assert_eq!(edges.matches(" C").count(), 0, "no cubic S-curves in a one-run job");

  // Styles ship with the page: not-interactive bookends, checkered flag at the finish.
  assert!(html.contains(".wf-bookend"), "bookend styles shipped");
  assert!(html.contains(".wf-start-play"), "play-triangle styles shipped");
  assert!(html.contains(".wf-finish-flag"), "finish flag styles shipped");

  // Client-side rebuild draws the same bookends so live updates stay consistent.
  let js = live_client_js();
  assert!(js.contains("function wfBookendHtml"), "client JS re-render draws bookends");
  assert!(js.contains("function wfLayoutWithBookends"), "client layout mirrors the bookends");
  assert!(js.contains("wf-start-play"), "client start glyph class");
  assert!(js.contains("wf-finish-flag"), "client finish flag class");
}
