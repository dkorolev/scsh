use super::client_js::live_client_js;
use super::escape::esc;
use super::proc::{empty_output_html, empty_output_label};
use super::session::session_page;
use crate::daemon::model::{DaemonMode, ProcKind, ProcRecord, ProcStatus, Session, Store};

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
        container_name: None,
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
        container_name: None,
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
fn live_client_js_counts_alive_clients_and_shutdown() {
  let js = live_client_js();
  assert!(js.contains("alive_clients"));
  assert!(js.contains("shutting down in"));
}

#[test]
fn live_client_js_skips_index_render_without_sessions() {
  let js = live_client_js();
  assert!(js.contains("if (!body || sessions == null) return"));
  assert!(js.contains("if (snapshot) renderIndex(snapshot, nowUnix)"));
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
fn wrap_page_serves_valid_css_braces() {
  use super::layout::wrap_page;
  let html = wrap_page("scsh sessions", 7274, None, "<p>body</p>");
  assert!(html.contains(":root {"));
  assert!(!html.contains(":root {{"));
  assert!(html.contains(".daemon-status {"));
}
