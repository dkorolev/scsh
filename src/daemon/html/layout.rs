//! Shared page shell, status bar, and embedded CSS.

use super::client_js::live_client_js;
use super::escape::{esc, quote_js};

/// Page chrome for the session browser — tokens and components from the prnui stylebook
/// (system UI stack, dark surfaces, chamfered buttons/badges/inputs — no CDN fonts; WEB-UI §5).
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
    /* Status bar padding (0.55*2) + crumbs line-box (1rem * 1.5) — tabs stick flush under it. */
    --daemon-status-height: calc(1.1rem + 1.5rem);
    color-scheme: dark;
  }
  html, body { width: 100%; margin: 0; }
  body {
    font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    background: var(--bg);
    color: var(--text);
    padding: 0;
    line-height: 1.5;
  }
  /* Content column — the status bar sits outside this so it can span the full viewport. */
  .page-shell {
    max-width: 1100px;
    margin: 0 auto;
    padding: 28px 24px 48px;
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
  .card > p.dim { margin: 0 0 0.85rem; max-width: 62rem; line-height: 1.45; }
  .card > p.dim + p.dim { margin-top: -0.35rem; }

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
    text-decoration: none;
  }
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
  .btn--muted { --btn-fill: rgba(125,133,144,0.2); background: var(--border); }
  .btn--muted::before { background: var(--surface); }
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
  .param-row input[type=number], .param-row select, .param-row textarea {
    display: block; width: 100%; padding: 10px 16px;
    font-size: 0.95rem; font-family: inherit; color: var(--text);
    background: transparent; border: none; outline: none;
    position: relative; z-index: 1;
  }
  .input::placeholder, #repo-path::placeholder { color: var(--text-muted); }
  .param-row input[type=text], .param-row input[type=number], .param-row select, .param-row textarea {
    background: rgba(255,255,255,0.06); border: 1px solid var(--border);
    border-radius: 4px; padding: 6px 10px; flex: 1 1 16rem; min-width: 16rem; max-width: 100%;
  }
  .param-row textarea { min-height: 12rem; resize: vertical; line-height: 1.45; }
  .param-row input:focus, .param-row select:focus, .param-row textarea:focus { border-color: var(--purple); outline: none; }

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

  /* ── status bar (full-width pinned chrome — WEB-UI §1 / §2) ── */
  /* Tabs use --daemon-status-height so they stick flush under this bar with no
     see-through gap (scrolled rows must not peek between the two sticky layers). */
  .page-lede {
    margin: 0 0 0.85rem; font-size: 1.02rem; line-height: 1.45; color: var(--text);
    max-width: 52rem;
  }
  .page-lede .dim { color: var(--text-muted); }
  .daemon-status {
    position: sticky; top: 0; z-index: 40;
    width: 100%; box-sizing: border-box;
    display: flex; gap: 0.55rem; align-items: center; flex-wrap: nowrap;
    height: var(--daemon-status-height);
    font-size: 0.85rem; margin: 0; padding: 0.55rem 24px;
    background: var(--surface);
    border: none; border-bottom: 1px solid var(--border); border-radius: 0;
  }
  /* The bar is a fixed-height single row, so long breadcrumb paths must truncate with
     an ellipsis rather than push the status dot off a phone-width viewport (min-width: 0
     lets the flex child actually shrink below its content size). */
  .daemon-status .crumbs {
    font-weight: 700; font-size: 1rem; line-height: 1.5;
    min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .crumbs a { color: inherit; text-decoration: none; }
  .crumbs a:hover { color: var(--cyan); }
  .crumb-sep { color: var(--text-muted); margin: 0 0.4rem; font-weight: 400; }
  .daemon-right { margin-left: auto; display: flex; gap: 0.55rem; align-items: center; flex-wrap: nowrap; }
  .session-kind { font-size: 1.05rem; margin: 0 0 0.75rem; color: var(--text-muted); }
  .session-kind strong { color: var(--text); }
  .chapters-pending { margin: 0.65rem 0 0; font-size: 0.9rem; }
  .daemon-status .dot {
    width: 0.55rem; height: 0.55rem; border-radius: 50%; background: var(--text-muted); flex-shrink: 0;
  }
  .daemon-status.live .dot { background: var(--green); }
  .daemon-status.down .dot { background: var(--red); }
  .daemon-status.connecting .dot { background: var(--cyan); box-shadow: 0 0 0 3px rgba(88,166,255,0.25); }
  .daemon-status.connecting { color: var(--cyan); }

  /* ── tabs (pinned flush under the status island — WEB-UI §1) ── */
  .tabs {
    position: sticky; top: var(--daemon-status-height); z-index: 19;
    display: flex; gap: 0.15rem; border-bottom: 1px solid var(--border);
    margin: 0 0 1.1rem; flex-wrap: wrap;
    background: var(--bg); padding-top: 0.35rem;
    /* Opaque seal into the status bar so subpixel gaps never show scrolled content. */
    box-shadow: 0 -0.5rem 0 0 var(--bg);
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
  /* Run tab: input grows; action cluster pinned to the right edge of the card. */
  .start-controls {
    display: flex; width: 100%; gap: 0.75rem; align-items: center; flex-wrap: wrap;
    margin: 0.75rem 0 0; font-size: 0.85rem;
  }
  .start-controls .input-wrap { flex: 1 1 16rem; min-width: 0; }
  .start-actions {
    display: flex; gap: 0.75rem; margin-left: auto; flex-shrink: 0;
  }
  /* An empty trailing note must not consume a flex gap, or the Open/Pick… buttons
     end one gap short of the New Project button's right edge on the row below. */
  #repo-note:empty { display: none; }
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
  /* Auto-scroll lands on this box, not the preceding explanatory copy. Its padding is
     deliberate blank breathing room above the first actionable definition control. */
  #defs-list { padding-top: 1.25rem; }
  .def-agents { margin-top: 0.45rem; display: flex; gap: 0.4rem; flex-wrap: wrap; }
  #def-form { margin: 0.75rem 0 1.25rem; }
  .param-row {
    display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap;
    margin-bottom: 0.5rem; font-size: 0.85rem;
  }
  .param-row label { min-width: 6rem; font-weight: 600; }
  .param-row--text { align-items: flex-start; }
  .param-row--text label { flex: 0 0 100%; }
  .param-row--text textarea { flex-basis: 100%; min-width: 0; }
  .param-req { color: var(--red); }
  ul.skills { margin: 0.5rem 0 1rem; padding-left: 1.25rem; font-size: 0.9rem; }

  /* ── session procs ── */
  .procs { margin-top: 1rem; width: 100%; }
  /* ── workflow dependency graph ── */
  .workflow-card { margin: 1rem 0; }
  .workflow-head { display: flex; flex-wrap: wrap; gap: 0.5rem 1.25rem; align-items: baseline; margin-bottom: 0.75rem; }
  .workflow-title { margin: 0; font-size: 1.05rem; }
  .workflow-outcome {
    display: inline-flex; align-items: center; min-height: 1.55rem; padding: 0.05rem 0.55rem;
    border: 1px solid currentColor; border-radius: 999px; font-size: 0.75rem; font-weight: 700;
    letter-spacing: 0.01em;
  }
  .workflow-outcome--running { color: var(--orange); background: rgba(210,153,34,0.09); }
  .workflow-outcome--completed { color: var(--green); background: rgba(63,185,80,0.09); }
  .workflow-outcome--failed, .workflow-outcome--cancelled { color: var(--red); background: rgba(248,81,73,0.09); }
  .workflow-outcome--terminated { color: var(--purple); background: rgba(163,113,247,0.09); }
  .workflow-summary { margin: 0; }
  .workflow-summary a.wf-jump {
    color: inherit; text-decoration: underline; text-decoration-color: transparent;
    text-underline-offset: 0.15em; border-radius: 2px;
  }
  .workflow-summary a.wf-jump:hover {
    color: var(--cyan); text-decoration-color: var(--cyan);
  }
  .workflow-summary a.wf-jump:focus-visible { outline: 2px solid var(--cyan); outline-offset: 2px; }
  .workflow-visual { position: relative; min-height: 0; }
  .workflow-legend {
    position: absolute; z-index: 4; top: 0.75rem; right: 0.75rem;
    list-style: none; margin: 0; padding: 0.35rem 0.55rem; display: flex; flex-wrap: wrap;
    justify-content: flex-end; gap: 0.35rem 0.85rem; max-width: calc(100% - 1.5rem);
    color: var(--text-muted); background: rgba(22,27,34,0.92); border: 1px solid var(--border);
    border-radius: 5px; font-size: 0.78rem;
  }
  .workflow-legend .wf-ico { margin-right: 0.2rem; }
  .wf-leg-running { color: var(--orange); }
  .wf-leg-terminating { color: var(--orange); }
  .wf-leg-done { color: var(--green); }
  .wf-leg-graceful { color: var(--orange); }
  .wf-leg-failed { color: var(--red); }
  .wf-leg-stopped { color: var(--red); }
  .wf-leg-stalled { color: var(--purple); }
  .wf-leg-waiting, .wf-leg-ready, .wf-leg-skipped { color: var(--text-muted); }
  .workflow-scroll {
    overflow: auto; max-width: 100%; height: 29rem; padding: 0.5rem;
    display: flex;
    overscroll-behavior: contain; touch-action: pan-x pan-y;
    scrollbar-width: none;
    border-radius: 6px; /* same as the enclosing .card island */
    /* Visible overflow cue when the graph is wider than the viewport (overlay scrollbars). */
    background:
      linear-gradient(90deg, var(--bg) 30%, transparent) left center / 1.25rem 100% no-repeat,
      linear-gradient(270deg, var(--bg) 30%, transparent) right center / 1.25rem 100% no-repeat,
      radial-gradient(farthest-side at 0 50%, rgba(0,0,0,0.35), transparent) left center / 0.75rem 100% no-repeat local,
      radial-gradient(farthest-side at 100% 50%, rgba(0,0,0,0.35), transparent) right center / 0.75rem 100% no-repeat local,
      var(--bg);
    background-attachment: local, local, scroll, scroll, local;
  }
  .workflow-scroll::-webkit-scrollbar { display: none; }
  .workflow-scroll:focus-visible { outline: 2px solid var(--cyan); outline-offset: 2px; }
  /* Auto margins center each axis independently, and collapse to zero on an overflowing axis.
     The stage must remain exactly as tall as its graph: a synthetic minimum would center the
     stage while leaving a small graph visibly above the viewport's vertical midpoint. */
  .workflow-stage { position: relative; flex: 0 0 auto; margin: auto; }
  .workflow-zoom { margin-left: auto; display: inline-flex; flex: 0 0 auto; gap: 0.25rem; }
  .workflow-zoom button {
    min-width: 2.2rem; min-height: 2rem; padding: 0.2rem 0.5rem; color: var(--text-muted);
    background: transparent; border: 1px solid var(--border); border-radius: 4px; cursor: pointer;
  }
  .workflow-zoom button:hover { color: var(--text); border-color: #3a4558; }
  .workflow-zoom button:disabled { color: #545d69; border-color: var(--border); cursor: default; opacity: 0.62; }
  .workflow-zoom button:focus-visible { outline: 2px solid var(--cyan); outline-offset: 2px; }
  .workflow-zoom [data-wf-zoom-reset] { width: 4.6rem; }
  .workflow-zoom [data-wf-zoom-fit] { width: 3rem; }
  .workflow-zoom [data-wf-expand] { width: 6.4rem; }
  body.wf-modal-open { overflow: hidden; }
  body.wf-modal-open::before {
    content: ""; position: fixed; inset: 0; z-index: 999; background: rgba(1,4,9,0.76);
  }
  .workflow-card.wf-expanded {
    position: fixed; inset: clamp(0.75rem, 2.5vw, 2rem); z-index: 1000; margin: 0;
    display: flex; flex-direction: column; max-width: none; min-height: 0;
    background: var(--surface); box-shadow: 0 24px 80px rgba(0,0,0,0.65);
  }
  .workflow-card.wf-expanded .workflow-visual { flex: 1 1 auto; display: flex; min-height: 0; }
  .workflow-card.wf-expanded .workflow-scroll { flex: 1 1 auto; height: auto; min-height: 0; }
  .wf-loop-island {
    position: absolute; z-index: 0; box-sizing: border-box; pointer-events: none;
    border: 1px solid rgba(139, 148, 158, 0.42); border-radius: 9px;
    background: rgba(139, 148, 158, 0.07);
  }
  .wf-loop-island > .wf-loop-title {
    position: absolute; top: 4px; left: 10px; color: var(--text-muted);
    font-size: 0.72rem; font-weight: 600; letter-spacing: 0.03em;
  }
  .wf-loop-island > .wf-loop-progress {
    position: absolute; right: 10px; bottom: 7px; padding: 0.14rem 0.42rem;
    color: var(--cyan); border: 1px dashed rgba(88,166,255,0.52); border-radius: 999px;
    background: rgba(88,166,255,0.08); font-size: 0.68rem; font-weight: 600;
    letter-spacing: 0.02em; white-space: nowrap;
  }
  .workflow-edges { position: absolute; inset: 0; color: #8b949e; pointer-events: none; }
  .wf-edge {
    fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round;
    opacity: 0.92;
  }
  .wf-arrowhead { stroke: currentColor; }
  .workflow-nodes { position: relative; z-index: 1; width: 100%; height: 100%; }
  .wf-node {
    position: absolute; box-sizing: border-box; display: flex; flex-direction: column; justify-content: center;
    gap: 0.15rem; padding: 0.45rem 0.65rem; min-width: 44px; min-height: 44px;
    text-decoration: none; color: var(--text); background: var(--bg); border: 1px solid var(--border);
    border-left: 3px solid var(--border); border-radius: 6px; font-size: 0.85rem; line-height: 1.25;
  }
  .wf-node:hover { border-color: #3a4558; background: var(--surface); }
  .wf-node:focus-visible { outline: 2px solid var(--cyan); outline-offset: 2px; }
  .wf-node.wf-flash { box-shadow: 0 0 0 2px var(--cyan); }
  .wf-state { display: flex; align-items: center; gap: 0.35rem; font-size: 0.72rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; }
  .wf-state-elapsed { color: var(--text-muted); font-weight: 500; letter-spacing: 0; text-transform: none; }
  .wf-state-elapsed:empty { display: none; }
  .wf-id { font-weight: 600; font-size: 0.95rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .wf-meta { font-size: 0.75rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .wf-gate {
    margin-left: 0.4rem; padding: 0.05rem 0.35rem; border: 1px solid rgba(137, 87, 229, 0.55);
    border-radius: 3px; color: var(--purple); font-weight: 600; font-size: 0.68rem;
    letter-spacing: 0.02em; vertical-align: middle; white-space: nowrap;
  }
  .wf-node.wf-running { border-left-color: var(--orange); }
  .wf-node.wf-running .wf-state { color: var(--orange); }
  .wf-node.wf-running { box-shadow: 0 0 0 1px rgba(210, 153, 34, 0.45); }
  .wf-node.wf-terminating { border-left-color: var(--orange); box-shadow: 0 0 0 1px rgba(210, 153, 34, 0.45); }
  .wf-node.wf-terminating .wf-state, .wf-node.wf-terminating .wf-id { color: var(--orange); }
  @media (prefers-reduced-motion: no-preference) {
    .wf-node.wf-running, .wf-node.wf-terminating { animation: wf-pulse 1.6s ease-in-out infinite; }
    @keyframes wf-pulse {
      0%, 100% { box-shadow: 0 0 0 1px rgba(210, 153, 34, 0.35); }
      50% { box-shadow: 0 0 0 3px rgba(210, 153, 34, 0.55); }
    }
    /* Decorative micro-motion lives behind the same gate: button press/hover, chip
       pop, and the toast slide keep their end states but stop animating when the
       user asked the OS for reduced motion. */
    .btn, button.btn { transition: transform 0.1s; }
    .btn::before { transition: background 0.2s; }
    .hchip { transition: transform 0.1s ease, opacity 0.1s ease; }
    .toast { transition: opacity 0.22s ease, transform 0.22s ease; }
  }
  .wf-node.wf-done { border-left-color: var(--green); }
  .wf-node.wf-graceful { border-left-color: var(--orange); }
  .wf-node.wf-graceful .wf-state { color: var(--orange); }
  .wf-node.wf-done .wf-state, .wf-node.wf-done .wf-id { color: var(--green); }
  .wf-node.wf-failed { border-left-color: var(--red); }
  .wf-node.wf-failed .wf-state, .wf-node.wf-failed .wf-id { color: var(--red); }
  .wf-node.wf-stopped { border-left-color: var(--red); }
  .wf-node.wf-stopped .wf-state, .wf-node.wf-stopped .wf-id { color: var(--red); }
  .wf-node.wf-stalled { border-left-color: var(--purple); }
  .wf-node.wf-stalled .wf-state, .wf-node.wf-stalled .wf-id { color: var(--purple); }
  .wf-node.wf-waiting, .wf-node.wf-ready { border-left-color: var(--text-muted); }
  .wf-node.wf-waiting .wf-state, .wf-node.wf-ready .wf-state { color: var(--text-muted); }
  .wf-node.wf-skipped { border-left-color: var(--text-muted); opacity: 0.75; }
  .wf-node.wf-skipped .wf-state, .wf-node.wf-skipped .wf-id { color: var(--text-muted); }
  .wf-node.wf-build { border-left-color: var(--cyan); }
  .wf-node.wf-build .wf-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.88rem; }
  /* Start / Finish bookends — small icon cards (play / checkered flag), not task cards. */
  .wf-bookend {
    position: absolute; box-sizing: border-box;
    display: flex; align-items: center; justify-content: center;
    background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
    pointer-events: none; user-select: none;
  }
  /* Solid play triangle — matches the start marker in the preferred graph mock. */
  .wf-start-play {
    display: block; width: 0; height: 0;
    border-style: solid;
    border-width: 0.5rem 0 0.5rem 0.85rem;
    border-color: transparent transparent transparent #fff;
    margin-left: 0.2rem; /* optical center in the square */
  }
  /* Checkered flag on a pole. */
  .wf-finish-flag {
    position: relative;
    display: block; width: 1.15rem; height: 0.9rem;
    margin-left: 0.2rem;
    border: 1px solid var(--text-muted);
    background-color: var(--bg);
    background-image:
      linear-gradient(45deg, var(--text-muted) 25%, transparent 25%),
      linear-gradient(-45deg, var(--text-muted) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, var(--text-muted) 75%),
      linear-gradient(-45deg, transparent 75%, var(--text-muted) 75%);
    background-size: 6px 6px;
    background-position: 0 0, 0 3px, 3px -3px, 3px 0;
  }
  .wf-finish-flag::before {
    content: '';
    position: absolute; left: -4px; top: -1px; bottom: -5px;
    width: 2px; border-radius: 1px;
    background: var(--text-muted);
  }
  /* Fleet/cycle summaries sit immediately after the last proc they summarize. */
  .fleets { margin: 1rem 0 0.25rem; width: 100%; }
  .fleet {
    background: linear-gradient(90deg, rgba(88,166,255,0.09), rgba(88,166,255,0.025) 42%, var(--surface));
    border: 1px solid rgba(88,166,255,0.42); border-left: 3px solid var(--cyan); border-radius: 6px;
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
  .fleet-row.graceful .glyph { color: var(--orange); }
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
    position: relative;
    background: var(--surface); border: 1px solid var(--border); border-left: 3px solid var(--border);
    border-radius: 6px; margin-bottom: 0.6rem; padding: 0.35rem 0.65rem;
  }
  details.proc[open] {
    border-top-color: #3a4558; border-right-color: #3a4558; border-bottom-color: #3a4558;
  }
  /* Status lives on the left accent bar (same language as the purple meta card). */
  details.proc.ok { border-left-color: var(--green); }
  details.proc.graceful { border-left-color: var(--orange); }
  details.proc.fail { border-left-color: var(--red); }
  details.proc.running { border-left-color: var(--orange); }
  details.proc.terminating { border-left-color: var(--orange); }
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
  details.proc.graceful summary .label { color: var(--orange); }
  details.proc.running summary .label { color: var(--orange); }
  details.proc.terminating summary .label { color: var(--orange); }
  details.proc.waiting summary .label { color: var(--cyan); }
  .fail .glyph { color: var(--red); }
  .ok .glyph { color: var(--green); }
  .graceful .glyph { color: var(--orange); }
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
  .session-procs-cell .chip-count { display: inline-block; min-width: 1.7rem; margin-right: 0.4rem; text-align: right; }
  .chip-overflow { margin-left: 0.35rem; color: var(--text-muted); font-variant-numeric: tabular-nums; }
  #sessions-body td, #sessions-body th { white-space: nowrap; }
  /* Long repo paths keep their TAIL visible (rtl flips the ellipsis to the front). */
  td.repo-path {
    max-width: 34ch; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; direction: rtl; text-align: left;
  }
  td.session-repo-path { direction: ltr; }
  .repo-copy {
    display: block; width: 34ch; max-width: 100%; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; direction: rtl; text-align: left; padding: 0; border: 0;
    color: inherit; background: transparent; font: inherit; cursor: copy;
  }
  .repo-copy:hover, .repo-copy:focus-visible { color: var(--cyan); }
  .repo-copy:focus-visible { outline: 1px solid var(--cyan); outline-offset: 2px; }
  a.repo-filter-link {
    color: inherit; text-decoration: none; direction: rtl;
  }
  a.repo-filter-link:hover { color: var(--cyan); text-decoration: underline; }
  .filter-banner {
    margin: 0 0 0.65rem; padding: 0.45rem 0.65rem;
    background: rgba(88, 166, 255, 0.08); border: 1px solid rgba(88, 166, 255, 0.35);
    border-radius: 6px; font-size: 0.9rem;
  }
  .filter-banner a.filter-clear { color: var(--cyan); }
  /* Setup tab: runtime switcher + harness readiness cards; image inventory is a separate island. */
  .images-runtimes { display: block; margin: 0; }
  .setup-toolbar {
    display: flex; flex-wrap: wrap; align-items: center; gap: 0.65rem 1rem;
    margin: 0 0 0.75rem;
  }
  .setup-toolbar a { color: var(--cyan); font-size: 0.85rem; }
  .setup-summary { margin: 0 0 0.85rem; font-size: 0.9rem; }
  .setup-cards {
    display: grid; gap: 0.75rem;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 22rem), 1fr));
    margin: 0 0 1rem;
  }
  .setup-card {
    border: 1px solid var(--border); border-radius: 8px;
    background: var(--surface); padding: 0.75rem 0.9rem;
  }
  .setup-card-head {
    display: flex; align-items: center; justify-content: space-between; gap: 0.5rem;
    margin: 0 0 0.55rem;
  }
  .setup-card-name { font-size: 1rem; }
  .setup-card-layers {
    display: grid; gap: 0.25rem; font-size: 0.85rem; margin: 0 0 0.55rem;
  }
  .setup-layer-label {
    display: inline-block; min-width: 3.4rem; color: var(--text-muted); font-weight: 600;
  }
  .setup-tag { font-size: 0.75rem; color: var(--text-muted); }
  .setup-ok { color: var(--green, #3fb950); }
  .setup-warn { color: var(--orange, #d29922); }
  .setup-models {
    list-style: none; margin: 0 0 0.55rem; padding: 0; font-size: 0.8rem;
  }
  .setup-models li { margin: 0.15rem 0; }
  .setup-model-row {
    display: flex; flex-wrap: wrap; align-items: center; gap: 0.35rem 0.5rem;
    cursor: pointer;
  }
  .setup-model-remove {
    border: none; background: transparent; color: var(--text-muted); cursor: pointer;
    padding: 0 0.2rem; font-size: 0.75rem;
  }
  .setup-model-remove:hover { color: var(--red); }
  .setup-add-model {
    display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; margin: 0.35rem 0 0.55rem;
  }
  .setup-add-input {
    flex: 1 1 10rem; min-width: 0; padding: 6px 10px;
    background: rgba(255,255,255,0.06); border: 1px solid var(--border); border-radius: 4px;
    color: var(--text); font: inherit; font-size: 0.8rem;
  }
  .setup-card-actions {
    min-height: 1.5rem; display: flex; flex-wrap: wrap; gap: 0.45rem; align-items: center;
  }
  .setup-models-block { margin: 0 0 0.55rem; }
  .setup-models-block > .setup-layer-label { display: block; margin: 0 0 0.25rem; }
  .setup-models-hint { margin: 0 0 0.35rem; font-size: 0.78rem; line-height: 1.35; }
  .setup-model-status--passed { color: var(--green); }
  .setup-model-status--failed { color: var(--red); }
  .setup-model-status--testing, .setup-model-status--queued { color: var(--orange); }
  .session-status.setup-ready > span { color: var(--cyan); }
  .setup-next { margin: 0; font-size: 0.8rem; }
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
    vertical-align: middle; cursor: pointer; text-decoration: none;
  }
  .hchip--opencode { --hchip-bg: #58a6ff; }
  .hchip--claude { --hchip-bg: #d97757; }
  .hchip--codex { --hchip-bg: #3fb950; }
  .hchip--grok { --hchip-bg: #d4a72c; }
  .hchip--cursor { --hchip-bg: #a371f7; }
  .hchip--done { opacity: 0.35; }
  .hchip:hover { transform: scale(1.3); opacity: 1; }
  .hchip:focus-visible { outline: 2px solid var(--cyan); outline-offset: 2px; opacity: 1; }
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
  /* Diff chip stays on the summary row (right edge). */
  a.proc-diff, span.proc-diff {
    margin-left: auto; align-self: center; flex-shrink: 0; white-space: nowrap;
    font-size: 0.75rem; line-height: 1.4; text-decoration: none;
    min-width: 10.5rem; height: 1.85rem; padding: 0 0.75rem;
    text-align: center; box-sizing: border-box;
  }
  a.proc-diff:hover { text-decoration: none; }
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
  /* Bottom-center toast — brief, non-blocking feedback (e.g. invalid project name). */
  .toast {
    position: fixed; left: 50%; bottom: 1.75rem; z-index: 2000;
    transform: translateX(-50%) translateY(0.6rem);
    max-width: min(28rem, calc(100vw - 2rem));
    padding: 0.7rem 1.15rem; border-radius: 8px;
    background: var(--surface); border: 1px solid var(--border); color: var(--text);
    font-size: 0.92rem; line-height: 1.35; text-align: center;
    box-shadow: 0 10px 28px rgba(0, 0, 0, 0.4);
    opacity: 0; pointer-events: none;
  }
  .toast.show {
    opacity: 1; transform: translateX(-50%) translateY(0);
  }
  #repo-path.flash-open, #repo-open.flash-open {
    box-shadow: 0 0 0 2px var(--cyan);
  }
  .output {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.8rem;
    max-height: 24rem; overflow: auto; margin: 0.5rem 0; padding: 0.65rem;
    background: #0a0e14; border: 1px solid var(--border); border-radius: 4px;
    width: 100%; box-sizing: border-box;
  }
  .line { white-space: pre; }
  .detail { overflow-wrap: anywhere; white-space: pre-wrap; max-width: 100%; color: var(--text-muted); }
  .container { overflow-x: auto; white-space: pre; max-width: 100%; color: var(--text-muted); }
  .container-runtime-label { color: var(--text); font-weight: 700; }
  .container-runtime-name { color: var(--cyan); font-weight: 700; }
  .at { opacity: 0.5; margin-right: 0.35rem; }

  /* ── cast player embed ── */
  .cast {
    position: relative; margin: 0.5rem 0; border: 1px solid var(--border);
    border-radius: 6px; overflow: hidden; background: #000;
    width: 100%; min-width: 0; max-width: 100%; box-sizing: border-box;
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
  .annotation-link { margin-left: 0.25rem; white-space: nowrap; }
  .annotation-link--running { color: var(--orange) !important; border-color: var(--orange) !important; }
  .annotation-link--ok { color: var(--green) !important; border-color: var(--green) !important; }
  .annotation-link--fail { color: var(--red) !important; border-color: var(--red) !important; }
  .annotation-dots { display: inline-block; width: 3ch; text-align: left; }
  .annotation-dots::after { content: '.'; animation: annotation-dots 1.2s steps(1, end) infinite; }
  @keyframes annotation-dots {
    0%, 100% { content: '.'; }
    33% { content: '..'; }
    66% { content: '...'; }
  }
  @media (prefers-reduced-motion: reduce) { .annotation-dots::after { animation: none; content: '...'; } }
  .annotation-target {
    color: var(--orange); font-size: 0.75rem; white-space: nowrap; text-decoration: none;
    border-bottom: 1px solid currentColor;
  }
  .wf-annotation { display: block; font-size: 0.68rem; line-height: 1.2; white-space: nowrap; }
  .wf-annotation--running { color: var(--orange); }
  .wf-annotation--ok { color: var(--green); }
  .wf-annotation--fail { color: var(--red); }
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
  .cast-player {
    width: 100%; min-width: 0; max-width: 100%; overflow: hidden;
  }
  .cast-player .beecast-player {
    width: 100%; min-width: 0; max-width: 100%;
  }
  /* `screen-box` is beecast's stable part hook. Make its width budget the visible
     pane, never the terminal's max-content width, so beecast scales wide recordings
     down instead of centering a clipped terminal with both edges off-screen. */
  .cast-player [part~="screen-box"] {
    width: 100%; min-width: 0; max-width: 100%;
  }
  .cast-placeholder { padding: 1.5rem 1rem; color: var(--text-muted); }
  .cast .ap-player { width: 100%; height: 100%; }

  .permalink { margin-top: 1.5rem; font-size: 0.9rem; color: var(--text-muted); }
  .session-meta {
    font-size: 0.9rem; margin: 0.75rem 0 1rem; display: grid;
    grid-template-columns: max-content minmax(0, 1fr); gap: 0.25rem 1rem;
    align-items: baseline; width: 100%;
  }
  .session-meta dt { font-weight: 600; color: var(--text-muted); font-size: 0.75rem;
    text-transform: uppercase; letter-spacing: 0.05em; }
  .session-meta dd { margin: 0; min-width: 0; }
"#;

/// Live-daemon-only chrome: Force stop, snapshot download slots, and the in-app confirm.
/// Omitted from offline job/cast HTML exports — those pages have nothing to stop and no
/// daemon to talk to.
pub(crate) const LIVE_ONLY_CSS: &str = r#"
  /* Clear the absolute top-right action stack (snapshot + Force stop). */
  .card:has(.session-actions) .session-meta { padding-right: 11.5rem; }
  /* Snapshot above Force stop, pinned to the proc island's top-right. */
  .proc-actions {
    position: absolute; top: 0.35rem; right: 0.55rem; z-index: 2;
    display: grid; grid-template-columns: repeat(2, 10.5rem); gap: 0.35rem;
  }
  .proc-actions .proc-kill,
  .proc-actions .proc-snapshot,
  .proc-actions .proc-diff {
    margin-left: 0; align-self: stretch; width: 100%;
    min-width: 10.5rem; box-sizing: border-box; height: 1.85rem;
  }
  .proc-actions .proc-diff { grid-column: 1; grid-row: 1; }
  .proc-actions .proc-snapshot { grid-column: 2; grid-row: 1; }
  .proc-actions .proc-kill { grid-column: 2; grid-row: 2; }
  /* Keep the summary text clear of the absolute top-right action stack. */
  details.proc:has(.proc-actions) > summary { padding-right: 11.5rem; }
  details.proc:has(.proc-actions .proc-diff) > summary { padding-right: 22.35rem; }
  button.proc-kill {
    flex-shrink: 0;
    font: inherit; font-size: 0.75rem; line-height: 1.4; cursor: pointer;
    color: var(--text); background: var(--red); border: none;
    border-radius: 4px; padding: 0 0.45rem; opacity: 1;
  }
  button.proc-kill:hover:not(:disabled) { background: var(--red); }
  button.proc-kill:disabled { cursor: default; opacity: 0.45; color: var(--text-muted); background: var(--border); }
  /* In-app confirm — replaces browser confirm() for destructive Force stop actions. */
  .scsh-dialog-backdrop {
    position: fixed; inset: 0; z-index: 3000;
    display: flex; align-items: center; justify-content: center;
    padding: 1.25rem;
    background: rgba(1, 4, 9, 0.72);
  }
  .scsh-dialog {
    width: min(26rem, 100%);
    background: var(--surface); border: 1px solid var(--border); border-radius: 10px;
    padding: 1.1rem 1.2rem 1rem;
    box-shadow: 0 16px 40px rgba(0, 0, 0, 0.55);
  }
  .scsh-dialog-title {
    margin: 0 0 0.45rem; font-size: 1.05rem; font-weight: 600; color: var(--text);
  }
  .scsh-dialog-body {
    margin: 0 0 1rem; font-size: 0.9rem; line-height: 1.45; color: var(--text-muted);
  }
  .scsh-dialog-actions {
    display: flex; gap: 0.55rem; justify-content: flex-end; flex-wrap: wrap;
  }
  .scsh-dialog-actions .btn { min-width: 5.5rem; }
  /* Snapshot above Force stop, pinned to the meta island's top-right. */
  .session-actions {
    position: absolute; top: 0.7rem; right: 0.85rem; z-index: 2; margin: 0;
    display: flex; flex-direction: column; align-items: flex-end; gap: 0.35rem;
  }
  .session-actions #session-stop,
  .session-actions .session-export,
  .session-actions .job-diff {
    min-width: 10.5rem; /* Incomplete job / Job snapshot / Force stop share a stable width */
    box-sizing: border-box;
    height: 1.85rem;
  }
"#;

/// The location path shown bold on the LEFT of the top island — `scsh` on the index,
/// `scsh › jobs › <id>` on a job page. Every segment is a permalink (the id links
/// to its own job page); the daemon status cluster keeps the island's right side.
fn crumbs_html(session_id: Option<&str>, index_crumb: Option<(&str, &str)>) -> String {
  match session_id {
    Some(id) => format!(
      "<a href=\"/\">scsh</a><span class=\"crumb-sep\">›</span><a href=\"/jobs\">jobs</a>\
<span class=\"crumb-sep\">›</span><a class=\"job-id\" href=\"/job/{id}\">{id}</a>",
      id = crate::daemon::html::escape::esc(id)
    ),
    None => {
      let (path, label, hidden) = match index_crumb {
        Some((path, label)) => (path, label, ""),
        None => ("/", "", " hidden"),
      };
      format!(
        "<a href=\"/\">scsh</a><span id=\"index-crumb-tail\"{hidden}>\
<span class=\"crumb-sep\">›</span><a id=\"index-crumb\" href=\"{path}\">{label}</a></span>",
        path = esc(path),
        label = esc(label)
      )
    }
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

pub(crate) fn wrap_page(
  title: &str, port: u16, session_id: Option<&str>, index_crumb: Option<(&str, &str)>, lede: &str, body: &str,
) -> String {
  let session_js = match session_id {
    Some(id) => format!("const SESSION_ID = {};", quote_js(id)),
    None => "const SESSION_ID = null;".to_string(),
  };
  // Lede lives in the content column under the full-width status bar so it scrolls away
  // while the chrome stays pinned at the top of the viewport (WEB-UI §1).
  let lede_html = if lede.is_empty() { String::new() } else { format!("<p class=\"page-lede\">{lede}</p>\n") };
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
{player_css}
<style>{css}{live_css}</style>
</head>
<body>
<div id="daemon-status" class="daemon-status connecting">
<span class="crumbs">{crumbs}</span>
<span class="daemon-right"><span id="status-label">connecting…</span>
<span id="status-uptime" class="dim"></span>{scsh_version}<span class="dot" aria-hidden="true"></span></span></div>
<div class="page-shell">
{lede}{body}
</div>
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
    live_css = LIVE_ONLY_CSS,
    scsh_version = scsh_version_html(),
    lede = lede_html,
    crumbs = crumbs_html(session_id, index_crumb),
    projects_dir = quote_js(&crate::daemon::paths::projects_dir().to_string_lossy()),
    body = body,
    player_js = player_js,
    port = port,
    session_js = session_js,
    live_js = live_client_js()
  )
}
