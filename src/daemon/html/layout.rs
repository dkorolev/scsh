//! Shared page shell, status bar, and embedded CSS.

use super::client_js::live_client_js;
use super::escape::{esc, quote_js};

/// Page chrome for the session browser — tokens and components from the prnui stylebook
/// (Ubuntu, dark surfaces, chamfered buttons/badges/inputs).
pub(crate) const PAGE_CSS: &str = r#"
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
  .card { position: relative; }
  .card--accent-left-cyan { border-left: 3px solid var(--cyan); }
  .card--accent-left-purple { border-left: 3px solid var(--purple); }
  .card--accent-left-green { border-left: 3px solid var(--green); }
  .card--accent-left-orange { border-left: 3px solid var(--orange); }
  .card--accent-top-magenta { border-top: 3px solid var(--magenta); }
  .card--accent-left-magenta { border-left: 3px solid var(--magenta); }

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
  /* Chamfer badges paint their fill, then a ::before surface overlay; the inner text must
     sit ABOVE that overlay or it is invisible (the empty-rectangle bug). */
  .badge > span, .session-status > span, .agent-badge > span { position: relative; z-index: 1; }
  .session-status.running, .badge--cyan {
    background: var(--cyan); color: var(--cyan);
  }
  .session-status.running::before, .badge--cyan::before { background: var(--surface); }
  .session-status.completed {
    background: var(--green); color: var(--green);
  }
  .session-status.completed::before { background: var(--surface); }
  .badge--green {
    background: var(--green); color: var(--green);
  }
  .badge--green::before { background: var(--surface); }
  .badge--purple {
    background: var(--purple); color: var(--purple);
  }
  .badge--purple::before { background: var(--surface); }
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
  .daemon-status .crumbs { font-weight: 700; font-size: 1rem; }
  .crumbs a { color: inherit; text-decoration: none; }
  .crumbs a:hover { color: var(--cyan); }
  .crumb-sep { color: var(--text-muted); margin: 0 0.4rem; font-weight: 400; }
  .daemon-right { margin-left: auto; display: flex; gap: 0.55rem; align-items: center; flex-wrap: wrap; }
  .session-kind { font-size: 1.05rem; margin: 0 0 0.75rem; color: var(--text-muted); }
  .session-kind strong { color: var(--text); }
  /* The resting lifecycle badge follows the heading (the kind/name stays flush-left with
     the meta labels below) — align it with the heading text it now sits beside. */
  .session-kind .session-status { margin-left: 0.5rem; vertical-align: 0.12em; }
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
  /* Fleet comparison tables (multi-route skill_source groups) sit above #session-procs. */
  .fleets { margin: 1rem 0 0.25rem; width: 100%; }
  .fleet {
    background: var(--surface); border: 1px solid var(--border); border-radius: 6px;
    margin-bottom: 0.75rem; padding: 0.65rem 0.85rem;
  }
  .fleet-title { margin: 0 0 0.35rem; font-size: 0.95rem; font-weight: 600; }
  .fleet-summary { margin: 0 0 0.55rem; font-size: 0.85rem; }
  .fleet-compare {
    width: 100%; border-collapse: collapse; font-size: 0.85rem;
  }
  .fleet-compare th, .fleet-compare td {
    text-align: left; padding: 0.35rem 0.5rem; border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  .fleet-compare th { color: var(--text-muted); font-weight: 600; font-size: 0.75rem; }
  .fleet-compare tr:last-child td { border-bottom: 0; }
  .fleet-row.ok .glyph { color: var(--green); }
  .fleet-row.fail .glyph { color: var(--red); }
  .fleet-harness { font-size: 0.75rem; margin-top: 0.1rem; }
  .fleet-jump {
    font: inherit; font-size: 0.8rem; line-height: 1.3; cursor: pointer;
    color: var(--cyan); background: transparent; border: 1px solid var(--cyan);
    border-radius: 4px; padding: 0 0.4rem; opacity: 0.85;
  }
  .fleet-jump:hover { opacity: 1; background: rgba(88, 166, 255, 0.12); }
  .fleet-jump-cell { width: 2.5rem; text-align: right; }
  details.proc {
    background: var(--surface); border: 1px solid var(--border); border-left: 3px solid var(--border);
    border-radius: 6px; margin-bottom: 0.6rem; padding: 0.35rem 0.65rem;
  }
  details.proc[open] {
    border-top-color: #3a4558; border-right-color: #3a4558; border-bottom-color: #3a4558;
  }
  /* Status lives on the left accent bar (same language as the purple meta card). */
  details.proc.ok { border-left-color: var(--green); }
  details.proc.fail { border-left-color: var(--red); }
  details.proc.running { border-left-color: var(--orange); }
  details.proc.waiting { border-left-color: var(--cyan); }
  details.proc.skipped { border-left-color: var(--text-muted); }
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
  /* Label (and fleet glyphs) still tint with status; the row bar carries the primary cue. */
  details.proc.fail summary .label { color: var(--red); }
  details.proc.ok summary .label { color: var(--green); }
  details.proc.running summary .label { color: var(--orange); }
  details.proc.waiting summary .label { color: var(--cyan); }
  .fail .glyph { color: var(--red); }
  .ok .glyph { color: var(--green); }
  .running .glyph { color: var(--cyan); }
  .skipped .glyph, details.proc.skipped summary .label { color: var(--text-muted); }
  .proc-meta {
    font-size: 0.85rem; margin: 0.35rem 0 0.5rem; display: flex; flex-wrap: wrap; gap: 0.35rem 0.75rem;
    color: var(--text-muted);
  }
  .proc-meta strong { font-weight: 600; margin-right: 0.25rem; color: var(--text); }
  .proc-stat { font-size: 0.8rem; color: var(--text-muted); }
  summary .meta {
    font-weight: 600; font-variant-numeric: tabular-nums; white-space: nowrap;
    color: var(--text);
  }
  summary .note code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.85em; opacity: 0.85; }
  .harness-stops { display: flex; flex-wrap: wrap; gap: 0.5rem; margin: 0 0 0.75rem; }
  /* single-letter harness chips: same letter, different hue (claude vs codex vs cursor) */
  .session-procs-cell { white-space: nowrap; }
  #sessions-body td, #sessions-body th { white-space: nowrap; }
  /* Long repo paths keep their TAIL visible (rtl flips the ellipsis to the front). */
  td.repo-path {
    max-width: 34ch; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; direction: rtl; text-align: left;
  }
  /* Containers tab: the runtime switcher is a segmented control — a view toggle between
     the (separate) Apple Containers and docker/podman image stores, not an action. */
  .images-runtimes { display: block; margin: 0 0 0.75rem; }
  .seg {
    display: inline-flex; border: 1px solid var(--border); border-radius: 8px;
    overflow: hidden; background: var(--surface);
  }
  .seg-opt {
    font: inherit; font-size: 0.85rem; padding: 0.3rem 0.9rem;
    background: none; border: 0; color: var(--text-muted); cursor: pointer;
  }
  .seg-opt + .seg-opt { border-left: 1px solid var(--border); }
  .seg-opt.active { background: var(--purple); color: #fff; font-weight: 600; }
  .seg-opt:hover:not(.active) { color: var(--text); }
  /* Projects tab: jobs grouped by the task they ran, one line per job, with breathing
     room between the badge+link lines. */
  .repo-jobgroup { margin: 0.3rem 0 0.8rem; }
  .repo-jobgroup:last-child { margin-bottom: 0.3rem; }
  .repo-jobgroup-name { display: block; font-weight: 700; font-size: 0.85rem; margin-bottom: 0.15rem; }
  .repo-job { margin: 0.3rem 0; }
  /* Six-letter job ids are identifiers: fixed font everywhere, and the link (color +
     underline) covers exactly those six letters — never the badge or the age stamp. */
  .job-id { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
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
  .hchip { transition: transform 0.1s ease, opacity 0.1s ease; }
  .hchip:hover { transform: scale(1.3); opacity: 1; }
  .ui-tip {
    position: fixed; z-index: 100; pointer-events: none;
    background: #1c2128; border: 1px solid var(--border); border-radius: 6px;
    color: var(--text); font-size: 0.75rem; line-height: 1.35;
    padding: 0.3rem 0.55rem; width: max-content; max-width: 44ch;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.45);
    white-space: pre-line;
  }
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
  /* The "⇄ commits diff" chip shares the kill button's right-edge slot: the diff appears
     only after a step finished, the kill button only while it runs — never both. */
  a.proc-diff, span.proc-diff {
    margin-left: auto; align-self: center; flex-shrink: 0; white-space: nowrap;
    font-size: 0.75rem; line-height: 1.4; text-decoration: none;
    color: var(--cyan); background: transparent; border: 1px solid var(--cyan);
    border-radius: 4px; padding: 0 0.45rem; opacity: 0.85;
  }
  a.proc-diff:hover { opacity: 1; background: rgba(88, 166, 255, 0.12); text-decoration: none; }
  /* Offline export: packed commits-diff embedded as a sandboxed iframe (not the live chip). */
  details.proc-diff {
    margin: 0.5rem 0; border: 1px solid var(--border); border-radius: 6px;
    background: var(--surface); padding: 0.25rem 0.55rem;
  }
  details.proc-diff > summary {
    cursor: pointer; list-style: none; color: var(--cyan); font-size: 0.85rem;
  }
  details.proc-diff iframe {
    width: 100%; min-height: 28rem; border: 1px solid var(--border); border-radius: 4px;
    margin: 0.4rem 0 0.25rem; background: #fff;
  }
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
  /* The snapshot download keeps its cyan identity but at the SAME size and shape as its
     toolbar siblings — a chamfered .btn in this row read as a misfit. */
  .cast-toolbar a[data-cast-export] { border-color: var(--cyan); color: var(--cyan); }
  .cast-toolbar a[data-cast-export]:hover { background: rgba(88, 166, 255, 0.12); }
  /* The job-level snapshot download wears the SAME style — one download family. */
  .dl-snap {
    display: inline-block; font: inherit; font-size: 0.8rem; line-height: 1.5;
    color: var(--cyan); background: none; border: 1px solid var(--cyan);
    border-radius: 4px; padding: 0.15rem 0.55rem; text-decoration: none; cursor: pointer;
  }
  .dl-snap:hover { background: rgba(88, 166, 255, 0.12); }
  .cast-keys { margin-left: auto; font-size: 0.72rem; color: var(--text-muted); }
  .cast-summary {
    padding: 0.45rem 0.65rem; font-size: 0.9rem; line-height: 1.4;
    background: #111820; border-bottom: 1px solid var(--border);
  }
  /* No fixed height: the player sizes its own box to the recording's aspect at full
     width (fit never scales up), so the pane is exactly as tall as the terminal wants.
     Chapters, the big play button, the speed menu — all player chrome (beecast-player). */
  .cast-player { width: 100%; }
  .cast-placeholder { padding: 1.5rem 1rem; color: var(--text-muted); }
  .cast:fullscreen {
    display: grid; background: #000;
    grid-template-columns: 1fr; grid-template-rows: auto 1fr;
  }
  .cast:fullscreen .cast-toolbar { grid-column: 1; }
  .cast:fullscreen .cast-summary { display: none; }
  /* !important: whatever height the pane carries inline, fullscreen must override it and
     let the grid row size the player. */
  .cast:fullscreen .cast-player { grid-column: 1; grid-row: 2; height: auto !important; max-height: none !important; min-height: 0; }
  .cast .ap-player { width: 100%; height: 100%; }

  .permalink { margin-top: 1.5rem; font-size: 0.9rem; color: var(--text-muted); }
  /* Pinned to the meta island's top-right corner (the card is the positioning context). */
  .session-actions { position: absolute; top: 0.7rem; right: 0.85rem; display: flex; gap: 0.6rem; align-items: center; margin: 0; z-index: 2; }
  .session-meta {
    font-size: 0.9rem; margin: 0.75rem 0 1rem; display: grid;
    grid-template-columns: max-content minmax(0, 1fr); gap: 0.25rem 1rem;
    align-items: baseline; width: 100%;
  }
  .session-meta dt { font-weight: 600; color: var(--text-muted); font-size: 0.75rem;
    text-transform: uppercase; letter-spacing: 0.05em; }
  .session-meta dd { margin: 0; min-width: 0; }
"#;

/// The location path shown bold on the LEFT of the top island — `scsh` on the index,
/// `scsh › jobs › <id>` on a job page. Every segment is a permalink (the id links
/// to its own job page); the daemon status cluster keeps the island's right side.
fn crumbs_html(session_id: Option<&str>) -> String {
  match session_id {
    Some(id) => format!(
      "<a href=\"/\">scsh</a><span class=\"crumb-sep\">›</span><a href=\"/\">jobs</a>\
<span class=\"crumb-sep\">›</span><a class=\"job-id\" href=\"/session/{id}\">{id}</a>",
      id = crate::daemon::html::escape::esc(id)
    ),
    None => "<a href=\"/\">scsh</a>".to_string(),
  }
}

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
<span class="crumbs">{crumbs}</span>
<span class="daemon-right"><span id="status-label">connecting…</span>
<span id="status-uptime" class="dim"></span>{scsh_version}<span class="dot" aria-hidden="true"></span></span></div>
{body}
{player_js}
<script>
const WS_PORT = {port};
const PROJECTS_DIR = {projects_dir};
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
    crumbs = crumbs_html(session_id),
    projects_dir = quote_js(&crate::daemon::paths::projects_dir().to_string_lossy()),
    body = body,
    player_js = player_js,
    port = port,
    session_js = session_js,
    live_js = live_client_js()
  )
}
