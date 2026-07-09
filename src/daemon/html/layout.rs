//! Shared page shell, status bar, and embedded CSS.

use super::client_js::live_client_js;
use super::escape::{esc, quote_js};

/// Page chrome for the session browser — tokens and components from the prnui stylebook
/// (Ubuntu, dark surfaces, chamfered buttons/badges/inputs).
const PAGE_CSS: &str = r#"
  *,*::before,*::after{box-sizing:border-box}
  :root {
    --bg: #0d1117;
    --surface: #161b22;
    --border: #2a3140;
    --text: #e6edf3;
    --text-muted: #7d8590;
    --cyan: #58a6ff;
    --orange: #d29922;
    --green: #3fb950;
    --yellow: #d4a72c;
    --red: #f85149;
    --magenta: #bc4dff;
    --purple: #8957e5;
    color-scheme: dark;
  }
  html, body { width: 100%; margin: 0; }
  body {
    font-family: 'Ubuntu', ui-sans-serif, system-ui, sans-serif;
    background: var(--bg);
    color: var(--text);
    padding: 28px 24px 48px;
    max-width: 1100px;
    margin: 0 auto;
    line-height: 1.5;
  }
  h1 { font-size: 1.75rem; font-weight: 600; margin: 0 0 4px; }
  h1 a { color: inherit; text-decoration: none; }
  h1 a:hover { color: var(--cyan); }
  h2 { font-size: 1.1rem; font-weight: 600; margin: 0 0 12px; }
  h4 { font-size: 1rem; font-weight: 600; margin: 0 0 10px; }
  .subtitle, .dim { color: var(--text-muted); }
  .dim { opacity: 1; }
  a { color: var(--cyan); }
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.85em;
    background: rgba(255,255,255,0.06);
    padding: 1px 6px;
    border-radius: 3px;
  }
  .section-label {
    font-size: 0.7rem; font-weight: 600; letter-spacing: 0.08em;
    text-transform: uppercase; color: var(--text-muted); margin: 0 0 14px;
  }

  /* ── chamfer (octagon clip) ── */
  .chamfer {
    --cut: 5px; --bw: 2px;
    position: relative;
    clip-path: polygon(
      var(--cut) 0%, calc(100% - var(--cut)) 0%,
      100% var(--cut), 100% calc(100% - var(--cut)),
      calc(100% - var(--cut)) 100%, var(--cut) 100%,
      0% calc(100% - var(--cut)), 0% var(--cut)
    );
  }
  .chamfer::before {
    content: '';
    position: absolute;
    inset: var(--bw);
    --inner: calc(var(--cut) - var(--bw) * 0.5858);
    clip-path: polygon(
      var(--inner) 0%, calc(100% - var(--inner)) 0%,
      100% var(--inner), 100% calc(100% - var(--inner)),
      calc(100% - var(--inner)) 100%, var(--inner) 100%,
      0% calc(100% - var(--inner)), 0% var(--inner)
    );
    z-index: 0;
  }

  /* ── cards ── */
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 20px 22px;
    margin-bottom: 20px;
  }
  .card--accent-left-cyan { border-left: 3px solid var(--cyan); }
  .card--accent-left-green { border-left: 3px solid var(--green); }
  .card--accent-left-orange { border-left: 3px solid var(--orange); }
  .card--accent-top-magenta { border-top: 3px solid var(--magenta); }

  /* ── buttons ── */
  .btn, button.btn {
    --cut: 5px; --bw: 2px;
    display: inline-flex; align-items: center; justify-content: center;
    min-width: 96px; padding: 8px 22px;
    font-size: 0.9rem; font-weight: 500; font-family: inherit;
    color: var(--text); border: none; cursor: pointer;
    transition: transform 0.1s;
    text-decoration: none;
  }
  .btn::before { transition: background 0.2s; }
  .btn:not(:disabled):not(.btn--disabled):hover::before { background: var(--btn-fill) !important; }
  .btn:not(:disabled):not(.btn--disabled):active { transform: scale(0.97); }
  .btn:not(:disabled):not(.btn--disabled):active::before { filter: brightness(0.85); }
  .btn > span { position: relative; z-index: 1; }
  .btn--cyan { --btn-fill: rgba(88,166,255,0.18); background: var(--cyan); }
  .btn--cyan::before { background: var(--surface); }
  .btn--orange { --btn-fill: rgba(210,153,34,0.18); background: var(--orange); }
  .btn--orange::before { background: var(--surface); }
  .btn--purple { --btn-fill: rgba(137,87,229,0.22); background: var(--purple); }
  .btn--purple::before { background: var(--surface); }
  .btn--red { --btn-fill: rgba(248,81,73,0.2); background: var(--red); }
  .btn--red::before { background: var(--surface); }
  .btn--green { --btn-fill: rgba(63,185,80,0.2); background: var(--green); }
  .btn--green::before { background: var(--surface); }
  .btn--disabled, .btn:disabled {
    background: var(--border); color: var(--text-muted); cursor: not-allowed;
    transform: none;
  }
  .btn--disabled::before, .btn:disabled::before { background: var(--surface); }
  .btn--sm { min-width: 0; padding: 5px 14px; font-size: 0.8rem; --cut: 4px; }
  .btn-row { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; margin-bottom: 10px; }
  .btn-row:last-child { margin-bottom: 0; }

  /* ── inputs ── */
  .input-wrap {
    --cut: 8px; --bw: 1.5px;
    background: var(--border); display: block; flex: 1; min-width: 12rem;
  }
  .input-wrap::before { background: rgba(255,255,255,0.06); transition: background 0.2s; }
  .input-wrap:focus-within { background: var(--purple); }
  .input-wrap:focus-within::before { background: rgba(137,87,229,0.4); }
  .input, .input-wrap .input, #repo-path, .param-row input[type=text],
  .param-row input[type=number], .param-row select {
    display: block; width: 100%; padding: 10px 16px;
    font-size: 0.95rem; font-family: inherit; color: var(--text);
    background: transparent; border: none; outline: none;
    position: relative; z-index: 1;
  }
  .input::placeholder, #repo-path::placeholder { color: var(--text-muted); }
  .param-row input[type=text], .param-row input[type=number], .param-row select {
    background: rgba(255,255,255,0.06); border: 1px solid var(--border);
    border-radius: 4px; padding: 6px 10px; width: auto; min-width: 8rem;
  }
  .param-row input:focus, .param-row select:focus { border-color: var(--purple); outline: none; }

  /* ── badges / status ── */
  .badge, .session-status, .agent-badge {
    --cut: 3px; --bw: 1.5px;
    display: inline-block; padding: 3px 12px;
    font-size: 0.72rem; font-weight: 700; letter-spacing: 0.04em;
    text-transform: uppercase; position: relative;
  }
  .badge > span, .session-status > span { position: relative; z-index: 1; }
  .session-status.running, .badge--cyan {
    background: var(--cyan); color: var(--cyan);
  }
  .session-status.running::before, .badge--cyan::before { background: var(--surface); }
  .session-status.completed, .badge--green {
    background: var(--green); color: var(--green);
  }
  .session-status.completed::before, .badge--green::before { background: var(--surface); }
  .session-status.failed, .session-status.terminated, .badge--red {
    background: var(--red); color: var(--red);
  }
  .session-status.failed::before, .session-status.terminated::before, .badge--red::before {
    background: var(--surface);
  }
  .session-status.cancelled, .badge--yellow {
    background: var(--yellow); color: var(--yellow);
  }
  .session-status.cancelled::before, .badge--yellow::before { background: var(--surface); }
  /* Pending runtime inspect — never looks like "missing" / empty (§13: no limbo). */
  .session-status.checking {
    background: var(--border); color: var(--text-muted);
    text-transform: none; font-weight: 500; letter-spacing: 0;
  }
  .session-status.checking::before { background: var(--surface); }
  .agent-badge {
    background: var(--border); color: var(--text-muted); text-transform: none;
    font-weight: 500; letter-spacing: 0; padding: 2px 8px;
  }
  .agent-badge::before { background: var(--surface); }
  .form-title { font-size: 1.05rem; font-weight: 600; margin: 0 0 10px; }

  /* ── status bar ── */
  .daemon-status {
    display: flex; gap: 0.55rem; align-items: center; flex-wrap: wrap;
    font-size: 0.85rem; margin-bottom: 1.25rem; padding: 0.55rem 0.85rem;
    background: var(--surface); border: 1px solid var(--border); border-radius: 6px;
  }
  .daemon-status .dot {
    width: 0.55rem; height: 0.55rem; border-radius: 50%; background: var(--text-muted); flex-shrink: 0;
  }
  .daemon-status.live .dot { background: var(--green); }
  .daemon-status.down .dot { background: var(--red); }
  .daemon-status.connecting .dot { background: var(--cyan); box-shadow: 0 0 0 3px rgba(88,166,255,0.25); }
  .daemon-status.connecting { color: var(--cyan); }

  /* ── tabs ── */
  .tabs {
    display: flex; gap: 0.15rem; border-bottom: 1px solid var(--border);
    margin-bottom: 1.1rem; flex-wrap: wrap;
  }
  .tab {
    font: inherit; color: var(--text-muted); background: none; border: none;
    border-bottom: 2px solid transparent; padding: 0.5rem 0.9rem; cursor: pointer;
  }
  .tab:hover { color: var(--text); }
  .tab.active {
    color: var(--cyan); border-bottom-color: var(--cyan); font-weight: 600;
  }
  .tab-panel { display: none; }
  .tab-panel.active { display: block; }

  /* ── tables ── */
  .table-scroll { overflow-x: auto; width: 100%; margin-bottom: 0.5rem; }
  table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
  thead tr { border-bottom: 1px solid var(--border); }
  tbody tr { border-bottom: 1px solid rgba(42,49,64,0.7); }
  tbody tr:hover { background: rgba(255,255,255,0.02); }
  th, td { text-align: left; padding: 0.45rem 0.55rem; vertical-align: top; }
  th {
    font-size: 0.7rem; font-weight: 600; letter-spacing: 0.06em;
    text-transform: uppercase; color: var(--text-muted);
  }
  td.repo-path, .session-meta .repo-path {
    font-size: 0.85em; white-space: nowrap; overflow-x: auto; max-width: 28rem;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }

  /* ── panels / forms ── */
  .images-controls, .controls-row {
    display: flex; gap: 0.75rem; align-items: center; flex-wrap: wrap;
    margin: 0.75rem 0 0; font-size: 0.85rem;
  }
  .images-controls label { cursor: pointer; user-select: none; color: var(--text-muted); }
  .image-select-cell { width: 1.5rem; }
  .blockers {
    border: 1px solid var(--red); border-radius: 6px; padding: 0.65rem 0.9rem;
    margin: 0.75rem 0 1rem; background: rgba(248,81,73,0.08); color: var(--text);
  }
  .blockers ul { margin: 0.3rem 0 0; padding-left: 1.2rem; }
  .def-card {
    background: var(--surface); border: 1px solid var(--border); border-radius: 6px;
    padding: 0.75rem 0.9rem; margin-bottom: 0.6rem;
  }
  .def-agents { margin-top: 0.45rem; display: flex; gap: 0.4rem; flex-wrap: wrap; }
  #def-form { margin: 0.75rem 0 1.25rem; }
  .param-row {
    display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap;
    margin-bottom: 0.5rem; font-size: 0.85rem;
  }
  .param-row label { min-width: 6rem; font-weight: 600; }
  .param-req { color: var(--red); }
  ul.skills { margin: 0.5rem 0 1rem; padding-left: 1.25rem; font-size: 0.9rem; }

  /* ── session procs ── */
  .procs { margin-top: 1rem; width: 100%; }
  details.proc {
    background: var(--surface); border: 1px solid var(--border); border-radius: 6px;
    margin-bottom: 0.6rem; padding: 0.35rem 0.65rem;
  }
  details.proc[open] { border-color: #3a4558; }
  summary {
    cursor: pointer; list-style: none; display: flex; gap: 0.5rem;
    align-items: baseline; flex-wrap: wrap; padding: 0.25rem 0;
  }
  summary::-webkit-details-marker { display: none; }
  summary .triangle {
    flex-shrink: 0; width: 0.85rem; text-align: center; font-size: 0.65rem;
    line-height: 1; opacity: 0.75; align-self: center; color: var(--text-muted);
  }
  details.proc:not([open]) summary .triangle::before { content: '▶'; }
  details.proc[open] summary .triangle::before { content: '▼'; }
  .glyph { font-weight: 600; }
  .fail .glyph { color: var(--red); }
  .ok .glyph { color: var(--green); }
  .running .glyph { color: var(--cyan); }
  .skipped .glyph, .skipped .label { color: var(--text-muted); }
  .proc-meta {
    font-size: 0.85rem; margin: 0.35rem 0 0.5rem; display: flex; flex-wrap: wrap; gap: 0.35rem 0.75rem;
    color: var(--text-muted);
  }
  .proc-meta strong { font-weight: 600; margin-right: 0.25rem; color: var(--text); }
  .proc-stat { font-size: 0.8rem; color: var(--text-muted); }
  .harness-stops { display: flex; flex-wrap: wrap; gap: 0.5rem; margin: 0 0 0.75rem; }
  /* single-letter harness chips: same letter, different hue (claude vs codex vs cursor) */
  .session-procs-cell { white-space: nowrap; }
  .hchip {
    display: inline-flex; align-items: center; justify-content: center;
    width: 1.15rem; height: 1.15rem; border-radius: 4px; margin-right: 0.2rem;
    font-size: 0.7rem; font-weight: 700; color: #0d1117; background: var(--hchip-bg, #8b949e);
    vertical-align: middle; cursor: default;
  }
  .hchip--opencode { --hchip-bg: #58a6ff; }
  .hchip--claude { --hchip-bg: #d97757; }
  .hchip--codex { --hchip-bg: #3fb950; }
  .hchip--grok { --hchip-bg: #d4a72c; }
  .hchip--cursor { --hchip-bg: #a371f7; }
  .hchip--done { opacity: 0.35; }
  .chip-count { color: var(--text-muted); margin-left: 0.25rem; font-size: 0.85rem; }
  .image-build-btn {
    font: inherit; font-size: 0.75rem; line-height: 1.5; cursor: pointer; white-space: nowrap;
    color: var(--cyan); background: transparent; border: 1px solid var(--cyan);
    border-radius: 4px; padding: 0 0.5rem;
  }
  .image-build-btn:hover:not(:disabled) { background: rgba(88, 166, 255, 0.12); }
  .image-build-btn:disabled { cursor: default; opacity: 0.55; }
  .proc-kill {
    margin-left: auto; align-self: center; flex-shrink: 0;
    font: inherit; font-size: 0.75rem; line-height: 1.4; cursor: pointer;
    color: var(--red); background: transparent; border: 1px solid var(--red);
    border-radius: 4px; padding: 0 0.45rem; opacity: 0.85;
  }
  .proc-kill:hover:not(:disabled) { opacity: 1; background: rgba(224, 82, 82, 0.12); }
  .proc-kill:disabled { cursor: default; opacity: 0.55; }
  .autoscroll-ctl {
    display: block; font-size: 0.8rem; margin: 0.35rem 0 0.25rem;
    cursor: pointer; user-select: none; color: var(--text-muted);
  }
  .autoscroll-ctl input { margin-right: 0.35rem; }
  .output {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.8rem;
    max-height: 24rem; overflow: auto; margin: 0.5rem 0; padding: 0.65rem;
    background: #0a0e14; border: 1px solid var(--border); border-radius: 4px;
    width: 100%; box-sizing: border-box;
  }
  .line { white-space: pre; }
  .detail, .container { overflow-x: auto; white-space: pre; max-width: 100%; color: var(--text-muted); }
  .at { opacity: 0.5; margin-right: 0.35rem; }

  /* ── cast player embed ── */
  .cast {
    position: relative; margin: 0.5rem 0; border: 1px solid var(--border);
    border-radius: 6px; overflow: hidden; background: #000;
  }
  .cast-toolbar {
    display: flex; gap: 0.4rem; align-items: center; flex-wrap: wrap;
    padding: 0.4rem 0.55rem; background: var(--surface); font-size: 0.8rem;
    border-bottom: 1px solid var(--border);
  }
  .cast-toolbar button, .cast-toolbar a {
    font: inherit; color: var(--text); background: none; border: 1px solid var(--border);
    border-radius: 4px; padding: 0.15rem 0.55rem; cursor: pointer; text-decoration: none;
  }
  .cast-toolbar button:hover, .cast-toolbar a:hover { border-color: var(--cyan); color: var(--cyan); }
  .cast-toolbar button.on { border-color: var(--red); color: var(--red); }
  .cast-copied { color: var(--green); visibility: hidden; }
  .cast-keys { margin-left: auto; font-size: 0.72rem; color: var(--text-muted); }
  .cast-summary {
    padding: 0.45rem 0.65rem; font-size: 0.9rem; line-height: 1.4;
    background: #111820; border-bottom: 1px solid var(--border);
  }
  .cast-chapters {
    display: flex; flex-wrap: wrap; gap: 0.35rem; padding: 0.4rem 0.55rem;
    background: #111820; border-bottom: 1px solid var(--border);
  }
  .cast-chapters button {
    font: inherit; font-size: 0.78rem; color: var(--text); background: var(--surface);
    border: 1px solid var(--border); border-radius: 4px; padding: 0.1rem 0.5rem; cursor: pointer;
  }
  .cast-chapters button:hover { border-color: var(--cyan); color: var(--cyan); }
  .cast-player { width: 100%; height: 42vh; max-height: 460px; }
  .cast-placeholder { padding: 1.5rem 1rem; color: var(--text-muted); }
  .cast-grew {
    display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap;
    padding: 0.35rem 0.55rem; background: #111820; font-size: 0.8rem; color: var(--text-muted);
  }
  .cast-grew button {
    font: inherit; color: var(--cyan); background: none; border: 1px solid var(--border);
    border-radius: 4px; padding: 0.1rem 0.5rem; cursor: pointer;
  }
  .cast-grew button:hover { border-color: var(--cyan); }
  .cast-toast {
    position: absolute; left: 50%; bottom: 12%; transform: translateX(-50%);
    max-width: 80%; padding: 0.4rem 0.9rem; border-radius: 7px; z-index: 5;
    background: #000c; color: #fff; font-size: 1rem; white-space: nowrap;
    overflow: hidden; text-overflow: ellipsis; pointer-events: none;
    opacity: 0; transition: opacity 0.6s ease;
  }
  .cast-toast.show { opacity: 1; transition: opacity 0.12s ease; }
  .cast:fullscreen {
    display: grid; background: #000;
    grid-template-columns: 1fr 0; grid-template-rows: auto 1fr;
  }
  .cast:fullscreen.has-side { grid-template-columns: 1fr var(--side-w, 360px); }
  .cast:fullscreen .cast-toolbar { grid-column: 1 / -1; }
  .cast:fullscreen .cast-summary, .cast:fullscreen .cast-chapters { display: none; }
  .cast:fullscreen .cast-player { grid-column: 1; grid-row: 2; height: auto; max-height: none; min-height: 0; }
  .cast-fs-chapters { display: none; }
  .cast:fullscreen.has-side .cast-fs-chapters {
    display: block; grid-column: 2; grid-row: 2; overflow-y: auto;
    background: var(--surface); border-left: 1px solid var(--border); padding: 0.6rem;
  }
  .cast-fs-chapters .fs-summary { font-size: 0.9rem; line-height: 1.45; margin-bottom: 0.8rem; opacity: 0.9; }
  .cast-fs-chapters .fs-head {
    font-size: 0.7rem; text-transform: uppercase; letter-spacing: 0.08em;
    color: var(--text-muted); margin-bottom: 0.4rem;
  }
  .cast-fs-chapters button {
    display: block; width: 100%; text-align: left; font: inherit; font-size: 0.85rem;
    color: var(--text); background: none; border: 0; border-radius: 4px;
    padding: 0.35rem 0.5rem; cursor: pointer;
  }
  .cast-fs-chapters button:hover { background: rgba(88,166,255,0.12); color: var(--cyan); }
  .cast-fs-chapters .fs-t { color: var(--cyan); margin-right: 0.4rem; font-variant-numeric: tabular-nums; }
  .cast .ap-player { width: 100%; height: 100%; }

  .permalink { margin-top: 1.5rem; font-size: 0.9rem; color: var(--text-muted); }
  .session-actions { display: flex; gap: 0.6rem; flex-wrap: wrap; align-items: center; margin: 0 0 0.85rem; }
  .session-meta {
    font-size: 0.9rem; margin: 0.75rem 0 1rem; display: grid;
    grid-template-columns: max-content minmax(0, 1fr); gap: 0.25rem 1rem;
    align-items: baseline; width: 100%;
  }
  .session-meta dt { font-weight: 600; color: var(--text-muted); font-size: 0.75rem;
    text-transform: uppercase; letter-spacing: 0.05em; }
  .session-meta dd { margin: 0; min-width: 0; }
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

/// Inline SVG favicon — a `❯` prompt chevron on a dark rounded tile, as a `data:` URI so
/// every page (the live dashboard AND the downloaded offline exports) stays request-free.
pub(crate) const FAVICON_LINK: &str = "<link rel=\"icon\" href=\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Crect width='16' height='16' rx='3' fill='%230d1117'/%3E%3Ctext x='3.5' y='12.5' font-size='11' fill='%2358a6ff'%3E%E2%9D%AF%3C/text%3E%3C/svg%3E\">";

pub(crate) fn wrap_page(title: &str, port: u16, session_id: Option<&str>, body: &str) -> String {
  let session_js = match session_id {
    Some(id) => format!("const SESSION_ID = {};", quote_js(id)),
    None => "const SESSION_ID = null;".to_string(),
  };
  // The session page embeds an asciinema player per proc; the index page does not need it.
  let (player_css, player_js) = if session_id.is_some() {
    (
      "<link rel=\"stylesheet\" href=\"/assets/scsh-cast-player.css\">",
      "<script src=\"/assets/scsh-cast-player.js\"></script>",
    )
  } else {
    ("", "")
  };
  format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
{favicon}
<title>{title}</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Ubuntu:wght@400;500;700&display=swap" rel="stylesheet">
{player_css}
<style>{css}</style>
</head>
<body>
<div id="daemon-status" class="daemon-status connecting">
<span class="dot" aria-hidden="true"></span><span id="status-label">connecting…</span>
<span id="status-uptime" class="dim"></span>{scsh_version}</div>
{body}
{player_js}
<script>
const WS_PORT = {port};
{session_js}
{live_js}
</script>
</body>
</html>
"#,
    title = esc(title),
    favicon = FAVICON_LINK,
    player_css = player_css,
    css = PAGE_CSS,
    scsh_version = scsh_version_html(),
    body = body,
    player_js = player_js,
    port = port,
    session_js = session_js,
    live_js = live_client_js()
  )
}
