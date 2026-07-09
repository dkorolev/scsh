//! Standalone player page for a proc's asciinema recording.
//!
//! Served at `/cast/<session>/<proc>/play`. Self-contained: the vendored
//! asciinema-player assets come from `/assets/asciinema-player.{js,css}`, and the cast
//! itself from `/cast/<session>/<proc>` (truncated server-side to whole NDJSON lines, so
//! an in-progress recording plays as far as it has gotten). Supports play/pause and
//! timeline scrubbing (native player controls), `#t=<seconds>` / `#t=<mm:ss>` deep links,
//! a copy-link-at-current-time button, download links for the raw `.cast` and the
//! self-contained offline `.html` export (hidden until the recording has frames — the
//! export endpoint 404s on a frameless cast), and a reload button for live runs. While the proc
//! is still running, the page listens for the daemon's `cast_growth` WebSocket
//! notifications: a cast with no frames yet shows a placeholder that upgrades in place,
//! growth surfaces an unobtrusive "Recording grew … — reload" button, and a Live toggle
//! follows the tail of the recording until the proc finishes.

use super::escape::{esc, quote_js};
use super::layout::FAVICON_LINK;
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
    "<span id=\"live-note\"><span class=\"live\">● live</span> <span class=\"dim\">recording still in progress — growth notifications arrive over the WebSocket</span></span>"
  } else {
    ""
  };
  Some(format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
{favicon}
<title>cast · {label} · {sid}</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Ubuntu:wght@400;500;700&display=swap" rel="stylesheet">
<link rel="stylesheet" href="/assets/asciinema-player.css">
<style>
:root {{
  --bg: #0d1117; --surface: #161b22; --border: #2a3140;
  --text: #e6edf3; --text-muted: #7d8590; --cyan: #58a6ff;
  --green: #3fb950; --red: #f85149; color-scheme: dark;
}}
body {{
  margin: 0; background: var(--bg); color: var(--text);
  font: 14px/1.5 'Ubuntu', ui-sans-serif, system-ui, sans-serif;
}}
header {{
  padding: 16px 20px 8px; display: flex; gap: 12px; align-items: baseline; flex-wrap: wrap;
}}
header a {{ color: var(--cyan); text-decoration: none; }}
header a:hover {{ text-decoration: underline; }}
header code {{
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  background: rgba(255,255,255,0.06); padding: 1px 6px; border-radius: 3px;
}}
.dim {{ color: var(--text-muted); }}
.live {{ color: var(--red); }}
.controls {{
  padding: 0 20px 12px; display: flex; gap: 10px; align-items: center; flex-wrap: wrap;
}}
.controls a, .controls button {{
  color: var(--text); background: var(--surface); border: 1px solid var(--border);
  border-radius: 4px; padding: 5px 12px; font: inherit; cursor: pointer; text-decoration: none;
}}
.controls button:hover, .controls a:hover {{ border-color: var(--cyan); color: var(--cyan); }}
.controls button.on {{ border-color: var(--red); color: var(--red); }}
.controls button:disabled {{ opacity: 0.5; cursor: default; }}
#copied {{ color: var(--green); visibility: hidden; }}
#summary {{ padding: 0 20px 8px; font-size: 14px; max-width: 70ch; }}
#summary:empty {{ display: none; }}
#chapters {{ padding: 0 20px 10px; display: flex; flex-wrap: wrap; gap: 6px; }}
#chapters button {{
  font: inherit; font-size: 12px; color: var(--text); background: var(--surface);
  border: 1px solid var(--border); border-radius: 4px; padding: 2px 8px; cursor: pointer;
}}
#chapters button:hover {{ border-color: var(--cyan); color: var(--cyan); }}
/* Fill the viewport below the header/controls so fit:'both' fits vertically too. */
#player-wrap {{ padding: 0 20px 20px; height: calc(100vh - 200px); min-height: 300px; }}
#player, .ap-player {{ width: 100%; height: 100%; max-width: 100%; }}
.cast-placeholder {{
  padding: 24px 16px; border: 1px dashed var(--border); border-radius: 6px; color: var(--text-muted);
}}
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
<a id="dl-html" href="{cast_url}/export.html" download hidden>⬇ download .html</a>
<button id="copy-t">🔗 copy link at current time</button><span id="copied">copied</span>
<button id="reload">↻ reload recording</button>
<button id="live-toggle"{live_toggle_hidden}>● Live</button>
<button id="grew" hidden></button>
<span class="dim">deep link: append <code>#t=90</code> or <code>#t=1:30</code> to this URL</span>
</div>
<div id="summary" class="dim"></div>
<div id="chapters"></div>
<div id="player-wrap"><div id="player"></div></div>
<script src="/assets/asciinema-player.js"></script>
<script>
const CAST_URL = {cast_url_js};
const SESSION = {sid_js};
const PROC = {proc_index};
const LIVE = {live_js};
let player = null;
let MARKERS = [];
function hashStart() {{
  const m = location.hash.match(/^#t=([0-9:.]+)$/);
  return m ? m[1] : null;
}}
function fmtClock(t) {{ t = Math.max(0, Math.floor(t)); const m = Math.floor(t/60), s = t%60; return m + ':' + (s<10?'0':'') + s; }}
// Available duration + event count of the fetched asciicast text (complete lines only).
// scsh records asciicast v3 (intervals → sum); a legacy v2 header (absolute times) takes max.
function castEventStats(text) {{
  let version = 3, duration = 0, events = 0;
  for (const raw of String(text || '').split('\n')) {{
    const line = raw.trim();
    if (!line || line[0] === '#') continue;
    if (line[0] === '{{') {{
      try {{ version = Number(JSON.parse(line).version) || 3; }} catch (_) {{}}
      continue;
    }}
    if (line[0] !== '[') continue;
    const t = parseFloat(line.slice(1));
    if (!isFinite(t)) continue;
    events++;
    duration = version === 3 ? duration + t : Math.max(duration, t);
  }}
  return {{ events, duration }};
}}
// Fetch the cast first, so an in-progress recording with no complete frames yet shows a
// calm placeholder instead of a player error on the empty/404 cast; the player mounts over
// the inline text ({{ data }}) once frames exist, and never re-fetches what we just loaded.
let loadedDuration = null;
function create(startAt, autoplay) {{
  if (player) {{ player.dispose(); player = null; }}
  hideGrew();
  const mount = document.getElementById('player');
  fetch(CAST_URL + '?ts=' + Date.now()).then(r => r.ok ? r.text() : null).catch(() => null).then(text => {{
    const stats = text == null ? {{ events: 0, duration: 0 }} : castEventStats(text);
    loadedDuration = stats.events ? stats.duration : null;
    // The .html export needs at least one complete frame (the server 404s otherwise), so
    // the download link rides the same no-frames state as the placeholder.
    document.getElementById('dl-html').hidden = !stats.events;
    if (!stats.events) {{
      mount.innerHTML = '<div class="cast-placeholder dim">' +
        (LIVE ? 'Recording in progress — no frames yet.' : 'No recorded frames.') + '</div>';
      return;
    }}
    mount.innerHTML = '';
    const opts = {{ fit: 'both', idleTimeLimit: 2, preload: true, theme: 'asciinema', markers: MARKERS }};
    if (startAt === 'end') startAt = stats.duration;
    // Numbers are clamped to what is loaded; '#t=mm:ss' strings pass through to the player.
    if (startAt != null) opts.startAt = typeof startAt === 'number' ? Math.max(0, Math.min(startAt, stats.duration)) : startAt;
    player = AsciinemaPlayer.create({{ data: text }}, mount, opts);
    if (autoplay) {{ try {{ player.play(); }} catch (_) {{}} }}
  }});
}}
// Growth notifications for this proc's recording arrive over the daemon's WebSocket
// (server-pushed — no JS polling loop). Without the WS the page degrades gracefully:
// finished casts play as always and the manual ↻ reload button still works.
let castRunning = LIVE;
let ws = null;
let wsDelay = 400;
function connectWs() {{
  if (!castRunning) return;
  try {{ ws = new WebSocket('ws://' + location.host + '/ws'); }} catch (_) {{ return; }}
  ws.onopen = () => {{ wsDelay = 400; }};
  ws.onmessage = (ev) => {{ try {{ onWsMessage(JSON.parse(ev.data)); }} catch (_) {{}} }};
  ws.onclose = () => {{ if (castRunning) {{ setTimeout(connectWs, wsDelay); wsDelay = Math.min(wsDelay * 2, 5000); }} }};
  ws.onerror = () => {{ try {{ ws.close(); }} catch (_) {{}} }};
}}
function onWsMessage(msg) {{
  if (msg.type !== 'cast_growth' || msg.session !== SESSION || msg.proc !== PROC) return;
  if (msg.running === false) {{ finishCast(); return; }}
  if (liveMode) {{ create(loadedDuration != null ? loadedDuration : 'end', true); return; }}
  if (loadedDuration == null) {{ create(hashStart()); return; }} // placeholder upgrades to a player
  showGrew(msg.duration);
}}
// Live mode mechanism, chosen deliberately: the vendored asciinema-player 3.17.0 build
// does ship streaming drivers (a websocket driver speaking the v1.alis / v2.asciicast
// subprotocols, and an eventsource driver), but they need a dedicated per-cast streaming
// endpoint next to the daemon's single JSON broadcast hub. Live mode instead rides the
// hub's cast_growth notifications: each one re-fetches the cast, re-creates the player
// seeked to where the previous load ended, and plays the newly appended tail.
let liveMode = false;
function setLiveMode(on) {{
  liveMode = !!on && castRunning;
  document.getElementById('live-toggle').classList.toggle('on', liveMode);
  if (liveMode) create('end');
}}
document.getElementById('live-toggle').addEventListener('click', () => setLiveMode(!liveMode));
function showGrew(available) {{
  if (loadedDuration == null || !(available > loadedDuration + 0.05)) return;
  const btn = document.getElementById('grew');
  const delta = Math.max(1, Math.round(available - loadedDuration));
  btn.textContent = 'Recording grew: +' + delta + 's (loaded ' + fmtClock(loadedDuration) +
    ', available ' + fmtClock(available) + ') — reload';
  btn.hidden = false;
}}
function hideGrew() {{ document.getElementById('grew').hidden = true; }}
// The proc finished: one last reload picks up the complete cast (keeping the current
// position), live mode ends cleanly (toggle off + disabled), the growth banner retires,
// and the WS is no longer needed.
function finishCast() {{
  castRunning = false;
  if (ws) {{ try {{ ws.close(); }} catch (_) {{}} ws = null; }}
  setLiveMode(false);
  const toggle = document.getElementById('live-toggle');
  toggle.disabled = true;
  const note = document.getElementById('live-note');
  if (note) note.innerHTML = '<span class="dim">recording finished</span>';
  create(player ? player.getCurrentTime() : hashStart());
}}
document.getElementById('grew').addEventListener('click', () => {{
  create(player ? player.getCurrentTime() : hashStart());
}});
connectWs();
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
    favicon = FAVICON_LINK,
    label = esc(&proc.label),
    sid = esc(session_id),
    live_note = live_note,
    cast_url = format!("/cast/{}/{}", esc(session_id), proc_index),
    cast_url_js = quote_js(&format!("/cast/{session_id}/{proc_index}")),
    sid_js = quote_js(session_id),
    proc_index = proc_index,
    live_js = if live { "true" } else { "false" },
    live_toggle_hidden = if live { "" } else { " hidden" },
  ))
}
