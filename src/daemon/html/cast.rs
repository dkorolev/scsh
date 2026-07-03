//! Standalone player page for a proc's asciinema recording.
//!
//! Served at `/cast/<session>/<proc>/play`. Self-contained: the vendored
//! asciinema-player assets come from `/assets/asciinema-player.{js,css}`, and the cast
//! itself from `/cast/<session>/<proc>` (truncated server-side to whole NDJSON lines, so
//! an in-progress recording plays as far as it has gotten). Supports play/pause and
//! timeline scrubbing (native player controls), `#t=<seconds>` / `#t=<mm:ss>` deep links,
//! a copy-link-at-current-time button, and a reload button for live runs.

use super::escape::{esc, quote_js};
use crate::daemon::model::{ProcStatus, Store};

/// Vendored asciinema-player (see `vendor/README.md`), served at `/assets/…`.
pub const PLAYER_JS: &str = include_str!("vendor/asciinema-player.min.js");
pub const PLAYER_CSS: &str = include_str!("vendor/asciinema-player.css");

pub fn cast_player_page(store: &Store, session_id: &str, proc_index: usize) -> Option<String> {
  let session = store.sessions.get(session_id)?;
  let proc = session.procs.iter().find(|p| p.index == proc_index)?;
  proc.cast_path.as_ref()?;
  let live = proc.status == ProcStatus::Running;
  let live_note = if live {
    "<span class=\"live\">● live</span> <span class=\"dim\">recording still in progress — Reload picks up the latest output</span>"
  } else {
    ""
  };
  Some(format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>cast · {label} · {sid}</title>
<link rel="stylesheet" href="/assets/asciinema-player.css">
<style>
:root {{ color-scheme: dark; }}
body {{ margin: 0; background: #121317; color: #d7d9df; font: 14px/1.5 system-ui, sans-serif; }}
header {{ padding: 12px 16px; display: flex; gap: 12px; align-items: baseline; flex-wrap: wrap; }}
header a {{ color: #7ab4ff; text-decoration: none; }}
header code {{ background: #1d1f26; padding: 1px 5px; border-radius: 4px; }}
.dim {{ color: #8a8d97; }}
.live {{ color: #ff6a6a; }}
.controls {{ padding: 0 16px 10px; display: flex; gap: 14px; align-items: center; flex-wrap: wrap; }}
.controls a, .controls button {{ color: #7ab4ff; background: none; border: 1px solid #2a2d36;
  border-radius: 6px; padding: 4px 10px; font: inherit; cursor: pointer; text-decoration: none; }}
.controls button:hover, .controls a:hover {{ border-color: #7ab4ff; }}
#copied {{ color: #7dd87d; visibility: hidden; }}
#summary {{ padding: 0 16px 8px; font-size: 14px; max-width: 70ch; }}
#summary:empty {{ display: none; }}
#chapters {{ padding: 0 16px 10px; display: flex; flex-wrap: wrap; gap: 6px; }}
#chapters button {{ font: inherit; font-size: 12px; color: #cdd; background: #1d1f26; border: 1px solid #2a2d36;
  border-radius: 5px; padding: 2px 8px; cursor: pointer; }}
#chapters button:hover {{ border-color: #7ab4ff; color: #fff; }}
/* Fill the viewport below the header/controls so fit:'both' fits vertically too. */
#player-wrap {{ padding: 0 16px 16px; height: calc(100vh - 200px); min-height: 300px; }}
#player, .ap-player {{ width: 100%; height: 100%; max-width: 100%; }}
</style>
</head>
<body>
<header>
<a href="/session/{sid}">← session <code>{sid}</code></a>
<strong>{label}</strong>
{live_note}
</header>
<div class="controls">
<a href="{cast_url}?dl=1" download>⬇ download .cast</a>
<button id="copy-t">🔗 copy link at current time</button><span id="copied">copied</span>
<button id="reload">↻ reload recording</button>
<span class="dim">deep link: append <code>#t=90</code> or <code>#t=1:30</code> to this URL</span>
</div>
<div id="summary" class="dim"></div>
<div id="chapters"></div>
<div id="player-wrap"><div id="player"></div></div>
<script src="/assets/asciinema-player.js"></script>
<script>
const CAST_URL = {cast_url_js};
let player = null;
let MARKERS = [];
function hashStart() {{
  const m = location.hash.match(/^#t=([0-9:.]+)$/);
  return m ? m[1] : null;
}}
function fmtClock(t) {{ t = Math.max(0, Math.floor(t)); const m = Math.floor(t/60), s = t%60; return m + ':' + (s<10?'0':'') + s; }}
function create(startAt) {{
  if (player) {{ player.dispose(); player = null; }}
  const opts = {{ fit: 'both', idleTimeLimit: 2, preload: true, theme: 'asciinema', markers: MARKERS }};
  if (startAt != null) opts.startAt = startAt;
  player = AsciinemaPlayer.create(CAST_URL + '?ts=' + Date.now(), document.getElementById('player'), opts);
}}
// Load the summary + chapters sidecar, then build the player with chapter markers.
fetch(CAST_URL + '/chapters').then(r => r.ok ? r.json() : {{}}).catch(() => ({{}})).then(meta => {{
  const chapters = (meta.chapters || []).filter(c => typeof c.t === 'number');
  MARKERS = chapters.map(c => [c.t, String(c.title || '')]);
  if (meta.summary) document.getElementById('summary').textContent = meta.summary;
  const cbar = document.getElementById('chapters');
  cbar.innerHTML = chapters.map((c, i) => '<button data-seek="' + c.t + '">' + fmtClock(c.t) + ' ' +
    (c.title || ('Chapter ' + (i+1))).replace(/[<&]/g, x => x === '<' ? '&lt;' : '&amp;') + '</button>').join('');
  cbar.querySelectorAll('[data-seek]').forEach(b => b.addEventListener('click', () => {{ if (player) {{ player.seek(Number(b.dataset.seek)); player.play(); }} }}));
  create(hashStart());
}});
window.addEventListener('hashchange', () => {{
  const t = hashStart();
  if (t != null && player) player.seek(t);
}});
document.getElementById('copy-t').addEventListener('click', () => {{
  const t = player ? Math.floor(player.getCurrentTime() * 10) / 10 : 0;
  const url = location.origin + location.pathname + '#t=' + t;
  history.replaceState(null, '', '#t=' + t);
  navigator.clipboard && navigator.clipboard.writeText(url);
  const note = document.getElementById('copied');
  note.style.visibility = 'visible';
  setTimeout(() => {{ note.style.visibility = 'hidden'; }}, 1200);
}});
document.getElementById('reload').addEventListener('click', () => {{
  const t = player ? player.getCurrentTime() : null;
  create(t);
}});
</script>
</body>
</html>
"#,
    label = esc(&proc.label),
    sid = esc(session_id),
    live_note = live_note,
    cast_url = format!("/cast/{}/{}", esc(session_id), proc_index),
    cast_url_js = quote_js(&format!("/cast/{session_id}/{proc_index}")),
  ))
}
