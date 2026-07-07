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
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 1,
      client_connected: true,
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
fn index_page_carries_the_images_panel_and_its_client_wiring() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  // The panel skeleton: status table body plus every control the client script binds to.
  for id in ["images-body", "images-build-selected", "images-build-all", "images-rebuild-base", "images-force"] {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  // The embedded client script populates the panel from the images API.
  assert!(live_client_js().contains("/api/v1/images"), "client js should fetch the images API");
  assert!(live_client_js().contains("/api/v1/images/build"), "client js should post builds");
}

#[test]
fn index_page_carries_the_repositories_panel_and_its_client_wiring() {
  let store = Store::new(DaemonMode::Persistent, 7274, 1);
  let html = super::index_page(&store);
  for id in ["repo-path", "repo-open", "defs-panel", "defs-list", "def-form", "repos-body"] {
    assert!(html.contains(&format!("id=\"{id}\"")), "index page should contain #{id}");
  }
  let js = live_client_js();
  assert!(js.contains("/api/v1/repos/open"), "client js opens a repo");
  assert!(js.contains("/api/v1/jobs/start"), "client js starts a job");
  assert!(js.contains("function renderRepoJobs"), "client js renders jobs by repository");
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
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 1,
      client_connected: false,
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
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 1,
      client_connected: true,
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
  assert!(html.contains(r#"<link rel="stylesheet" href="/assets/asciinema-player.css">"#), "player css");
  assert!(html.contains(r#"<script src="/assets/asciinema-player.js"></script>"#), "player js");
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
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      last_seen_at: 1,
      client_connected: true,
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
    html.contains(r#"<a class="session-export" href="/session/castab/export.html" download>⬇ session .html</a>"#),
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
  assert!(html.contains(".daemon-status.connecting .dot { background: #6af; }"));
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
