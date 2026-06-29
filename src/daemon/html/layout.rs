//! Shared page shell, status bar, and embedded CSS.

use super::client_js::live_client_js;
use super::escape::{esc, quote_js};

const PAGE_CSS: &str = r#"
  :root { color-scheme: dark light; font-family: ui-sans-serif, system-ui, sans-serif; line-height: 1.45; }
  html, body { box-sizing: border-box; width: 100%; margin: 0; }
  body { padding: 1rem 1.25rem; max-width: none; }
  h1 { font-size: 1.25rem; font-weight: 600; }
  .dim { opacity: 0.65; }
  .daemon-status {
    display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap;
    font-size: 0.85rem; margin-bottom: 1rem; padding: 0.35rem 0.6rem;
    border: 1px solid #8884; border-radius: 6px;
  }
  .daemon-status .dot { width: 0.55rem; height: 0.55rem; border-radius: 50%; background: #888; flex-shrink: 0; }
  .daemon-status.live .dot { background: #3a8; }
  .daemon-status.down .dot { background: #e55; }
  .daemon-status.connecting .dot { background: #6af; }
  .daemon-status.connecting { color: #6af; }
  .table-scroll { overflow-x: auto; width: 100%; margin-bottom: 1rem; }
  table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
  thead tr, tbody tr { border-bottom: 1px solid #8884; }
  th, td { text-align: left; padding: 0.35rem 0.5rem; vertical-align: top; }
  .session-status {
    display: inline-block; font-size: 0.75rem; font-weight: 600;
    padding: 0.12rem 0.45rem; border-radius: 4px;
  }
  .session-status.running { color: #6af; background: #6af22; }
  .session-status.completed { color: #3a8; background: #3a822; }
  .session-status.failed { color: #e55; background: #e5522; }
  .session-status.cancelled { color: #ca6; background: #ca622; }
  .session-status.terminated { color: #e55; background: #e5518; }
  a { color: inherit; }
  code { font-size: 0.85em; }
  ul.skills { margin: 0.5rem 0 1rem; padding-left: 1.25rem; font-size: 0.9rem; }
  .procs { margin-top: 1rem; width: 100%; }
  details.proc { border: 1px solid #8884; border-radius: 6px; margin-bottom: 0.5rem; padding: 0.25rem 0.5rem; }
  details.proc[open] { background: #8881; }
  summary { cursor: pointer; list-style: none; display: flex; gap: 0.5rem; align-items: baseline; flex-wrap: wrap; }
  summary::-webkit-details-marker { display: none; }
  summary .triangle {
    flex-shrink: 0; width: 0.85rem; text-align: center; font-size: 0.65rem;
    line-height: 1; opacity: 0.75; align-self: center;
  }
  details.proc:not([open]) summary .triangle::before { content: '▶'; }
  details.proc[open] summary .triangle::before { content: '▼'; }
  .glyph { font-weight: 600; }
  .fail .glyph { color: #e55; }
  .ok .glyph { color: #3a8; }
  .running .glyph { color: #6af; }
  .proc-meta { font-size: 0.85rem; margin: 0.35rem 0 0.5rem; display: flex; flex-wrap: wrap; gap: 0.35rem 0.75rem; }
  .proc-meta strong { font-weight: 600; margin-right: 0.25rem; }
  .proc-stat { font-size: 0.8rem; opacity: 0.75; }
  .proc-stat .idle { opacity: 0.85; }
  .autoscroll-ctl {
    display: block; font-size: 0.8rem; margin: 0.35rem 0 0.25rem;
    cursor: pointer; user-select: none;
  }
  .autoscroll-ctl input { margin-right: 0.35rem; }
  .output {
    font-family: ui-monospace, monospace; font-size: 0.8rem; max-height: 24rem;
    overflow: auto; margin: 0.5rem 0; padding: 0.5rem; background: #0002;
    border-radius: 4px; width: 100%; box-sizing: border-box;
  }
  .line { white-space: pre; }
  .detail, .container { overflow-x: auto; white-space: pre; max-width: 100%; }
  .at { opacity: 0.5; margin-right: 0.35rem; }
  .permalink { margin-top: 1.5rem; font-size: 0.9rem; }
  .session-meta {
    font-size: 0.9rem; margin: 0.75rem 0 1rem; display: grid;
    grid-template-columns: max-content minmax(0, 1fr); gap: 0.2rem 1rem;
    align-items: baseline; width: 100%;
  }
  .session-meta dt { font-weight: 600; opacity: 0.85; }
  .session-meta dd { margin: 0; min-width: 0; }
  .session-meta .repo-path { font-size: 0.85em; white-space: pre; overflow-x: auto; display: block; max-width: 100%; }
  td.repo-path { font-size: 0.85em; white-space: nowrap; overflow-x: auto; max-width: 28rem; }
"#;

fn scsh_version_html() -> String {
  let v = crate::version::pkg_version();
  let git = crate::version::git_stamp();
  if git.is_empty() {
    format!("<span id=\"status-scsh-version\" class=\"dim\">scsh {}</span>", esc(v))
  } else {
    format!("<span id=\"status-scsh-version\" class=\"dim\">scsh {} · <code>{}</code></span>", esc(v), esc(&git))
  }
}

pub(crate) fn wrap_page(title: &str, port: u16, session_id: Option<&str>, body: &str) -> String {
  let session_js = match session_id {
    Some(id) => format!("const SESSION_ID = {};", quote_js(id)),
    None => "const SESSION_ID = null;".to_string(),
  };
  format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{css}</style>
</head>
<body>
<div id="daemon-status" class="daemon-status connecting">
<span class="dot" aria-hidden="true"></span><span id="status-label">connecting…</span>
<span id="status-uptime" class="dim"></span>{scsh_version}</div>
{body}
<script>
const WS_PORT = {port};
{session_js}
{live_js}
</script>
</body>
</html>
"#,
    title = esc(title),
    css = PAGE_CSS,
    scsh_version = scsh_version_html(),
    body = body,
    port = port,
    session_js = session_js,
    live_js = live_client_js()
  )
}
