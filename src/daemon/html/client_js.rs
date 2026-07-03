//! WebSocket live-update client script embedded in session browser pages.

/// Browser-side tick handler, index/session rendering, and WebSocket reconnect logic.
pub(crate) fn live_client_js() -> &'static str {
  r#"
function esc(s) {
  return String(s ?? '').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
}
function fmtUptime(secs) {
  secs = Math.max(0, secs|0);
  if (secs < 60) return 'up ' + secs + 's';
  const m = Math.floor(secs / 60), s = secs % 60;
  if (secs < 3600) return 'up ' + m + 'm ' + s + 's';
  const h = Math.floor(secs / 3600);
  return 'up ' + h + 'h ' + (Math.floor((secs % 3600) / 60)) + 'm';
}
const SESSION_STALE_SECS = 10;
function sessionHasIncompleteProcs(session) {
  const procs = session.procs || [];
  return procs.some(p => p.status === 'running' || p.status === 'waiting');
}
function sessionLifecycle(session, nowUnix) {
  if (session.ended_at) {
    if (sessionHasIncompleteProcs(session)) return { label: 'cancelled', class: 'cancelled' };
    if ((session.procs || []).some(p => p.status === 'fail')) return { label: 'failed', class: 'failed' };
    return { label: 'completed', class: 'completed' };
  }
  const lastSeen = session.last_seen_at || session.started_at || 0;
  if (nowUnix - lastSeen > SESSION_STALE_SECS) return { label: 'terminated abruptly', class: 'terminated' };
  return { label: 'running', class: 'running' };
}
function sessionStatus(session) {
  return sessionLifecycle(session, Date.now() / 1000).label;
}
function sortSessionIds(sessions, nowUnix) {
  const ids = Object.keys(sessions || {});
  ids.sort((a, b) => {
    const sa = sessions[a], sb = sessions[b];
    const aLive = sessionLifecycle(sa, nowUnix).class === 'running';
    const bLive = sessionLifecycle(sb, nowUnix).class === 'running';
    if (aLive !== bLive) return aLive ? -1 : 1;
    return (sb.started_at || 0) - (sa.started_at || 0);
  });
  return ids;
}
function formatRelative(secsAgo) {
  secsAgo = Math.max(0, Math.floor(secsAgo || 0));
  if (secsAgo < 60) return secsAgo + 's ago';
  const m = Math.floor(secsAgo / 60);
  if (secsAgo < 3600) return m + 'm ago';
  const h = Math.floor(secsAgo / 3600);
  return h + 'h ' + Math.floor((secsAgo % 3600) / 60) + 'm ago';
}
function sessionDurationLabel(session, nowUnix, lifecycle) {
  const start = session.started_at || 0;
  if (session.ended_at && start) return formatDuration(session.ended_at - start);
  if (lifecycle.class === 'running' && start) return formatDuration(nowUnix - start) + ' so far';
  const lastSeen = session.last_seen_at || start;
  if (lifecycle.class === 'terminated' && start) return formatDuration(lastSeen - start);
  return '—';
}
function sessionStatusBadge(lifecycle) {
  return '<span class="session-status ' + esc(lifecycle.class) + '">' + esc(lifecycle.label) + '</span>';
}
function sessionStartedCell(session, nowUnix) {
  const ts = session.started_at || 0;
  const abs = formatUnixTime(ts);
  const rel = formatRelative(nowUnix - ts);
  return '<span class="session-started" data-started="' + esc(String(ts)) + '">' +
    '<span class="session-started-abs">' + esc(abs) + '</span><br>' +
    '<span class="dim session-started-rel">' + esc(rel) + '</span></span>';
}
function indexRowHtml(id, session, nowUnix) {
  const lifecycle = sessionLifecycle(session, nowUnix);
  const profile = session.profile || 'default';
  const n = (session.procs || []).length;
  const duration = sessionDurationLabel(session, nowUnix, lifecycle);
  return '<tr data-session-id="' + esc(id) + '"><td><a href="/session/' + esc(id) + '">' + esc(id) + '</a></td>' +
    '<td class="session-status-cell">' + sessionStatusBadge(lifecycle) + '</td>' +
    '<td class="session-started-cell">' + sessionStartedCell(session, nowUnix) + '</td>' +
    '<td class="session-duration-cell">' + esc(duration) + '</td>' +
    '<td>' + esc(profile) + '</td><td>' + n + '</td>' +
    '<td class="dim repo-path">' + esc(session.repo || '') + '</td></tr>';
}
function syncIndexRow(row, session, nowUnix) {
  const lifecycle = sessionLifecycle(session, nowUnix);
  const statusCell = row.querySelector('.session-status-cell');
  if (statusCell) statusCell.innerHTML = sessionStatusBadge(lifecycle);
  const startedCell = row.querySelector('.session-started-cell');
  if (startedCell) startedCell.innerHTML = sessionStartedCell(session, nowUnix);
  const durationCell = row.querySelector('.session-duration-cell');
  if (durationCell) setTextUnlessSelecting(durationCell, sessionDurationLabel(session, nowUnix, lifecycle));
}
function emptyOutputLabel(status) {
  return (status === 'ok' || status === 'fail') ? 'No output.' : 'No output yet.';
}
function emptyOutputHtml(status) {
  return '<div class="dim">' + emptyOutputLabel(status) + '</div>';
}
function glyph(status) {
  return ({waiting:'○',running:'◉',ok:'✓',fail:'✗'})[status] || '?';
}
function formatUnixTime(unix) {
  if (!unix) return '—';
  const d = new Date(unix * 1000);
  return d.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'medium' });
}
function formatDuration(secs) {
  secs = Math.max(0, Math.floor(secs || 0));
  if (secs < 60) return secs + 's';
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  if (secs < 3600) return m + 'm ' + s + 's';
  const h = Math.floor(secs / 3600);
  return h + 'h ' + Math.floor((secs % 3600) / 60) + 'm ' + s + 's';
}
function selectionInside(node) {
  const sel = document.getSelection();
  if (!sel || sel.rangeCount === 0 || sel.isCollapsed) return false;
  const range = sel.getRangeAt(0);
  return node.contains(range.commonAncestorContainer);
}
function setTextUnlessSelecting(el, text) {
  if (!el || selectionInside(el)) return;
  if (el.textContent !== text) el.textContent = text;
}
function formatElapsedClock(elapsed) {
  if (elapsed == null) return '—';
  return String(Math.floor(elapsed)) + 's';
}
function formatIdleClock(secs) {
  if (secs == null || secs < 1) return '';
  return ' · idle ' + Math.floor(secs) + 's';
}
function sessionRunning(session) {
  const procs = session.procs || [];
  if (procs.some(p => p.status === 'running' || p.status === 'waiting')) return true;
  if (!session.ended_at && procs.length === 0) return true;
  return false;
}
function sessionDurationSecs(session, nowUnix) {
  const start = session.started_at || 0;
  if (session.ended_at) return session.ended_at - start;
  if (nowUnix > start) return nowUnix - start;
  return 0;
}
function renderSessionMeta(session, nowUnix) {
  const el = document.getElementById('session-meta');
  if (!el || !session) return;
  const started = formatUnixTime(session.started_at);
  const ended = session.ended_at
    ? formatUnixTime(session.ended_at)
    : (sessionRunning(session) ? 'still running' : '—');
  const repo = session.repo || el.dataset.repo || '';
  const branch = session.branch || el.dataset.branch || '—';
  el.dataset.started = String(session.started_at || '');
  el.dataset.ended = session.ended_at ? String(session.ended_at) : '';
  el.dataset.repo = repo;
  el.dataset.branch = branch;
  if (!el.querySelector('[data-session-duration]')) {
    el.innerHTML =
      '<dt>Started</dt><dd data-session-started>' + esc(started) + '</dd>' +
      '<dt>Ended</dt><dd data-session-ended>' + esc(ended) + '</dd>' +
      '<dt>Duration</dt><dd data-session-duration>' +
      esc(formatDuration(sessionDurationSecs(session, nowUnix))) + '</dd>' +
      '<dt>Branch</dt><dd data-session-branch><code>' + esc(branch) + '</code></dd>' +
      '<dt>Repo</dt><dd data-session-repo><code class="repo-path">' + esc(repo) + '</code></dd>';
  } else {
    setTextUnlessSelecting(el.querySelector('[data-session-ended]'), ended);
    setTextUnlessSelecting(el.querySelector('[data-session-branch] code'), branch);
    const repoEl = el.querySelector('[data-session-repo] code');
    setTextUnlessSelecting(repoEl, repo);
  }
  syncSessionDuration(session, nowUnix);
}
function syncSessionDuration(session, nowUnix) {
  const el = document.getElementById('session-meta');
  if (!el) return;
  setTextUnlessSelecting(
    el.querySelector('[data-session-duration]'),
    formatDuration(sessionDurationSecs(session, nowUnix))
  );
}
function initSessionMetaFromDom() {
  const el = document.getElementById('session-meta');
  if (!el || !el.dataset.started) return;
  const session = {
    started_at: Number(el.dataset.started) || 0,
    ended_at: el.dataset.ended ? Number(el.dataset.ended) : null,
    repo: el.dataset.repo || '',
    branch: el.dataset.branch || '',
    procs: [],
  };
  renderSessionMeta(session, Date.now() / 1000);
}
function setScshVersion(version, git) {
  const el = document.getElementById('status-scsh-version');
  if (!el || !version) return;
  if (git) {
    el.innerHTML = 'scsh ' + esc(version) + ' · <code>' + esc(git) + '</code>';
  } else {
    el.textContent = 'scsh ' + version;
  }
}
function setDaemonStatus(kind, label, uptime) {
  const bar = document.getElementById('daemon-status');
  const lbl = document.getElementById('status-label');
  const up = document.getElementById('status-uptime');
  bar.className = 'daemon-status ' + kind;
  lbl.textContent = label;
  up.textContent = uptime != null ? fmtUptime(uptime) : '';
}
function renderIndex(sessions, nowUnix) {
  const body = document.getElementById('sessions-body');
  if (!body || sessions == null) return;
  nowUnix = nowUnix ?? (Date.now() / 1000);
  const ids = sortSessionIds(sessions, nowUnix);
  if (!ids.length) {
    body.innerHTML =
      '<tr><td colspan="7" class="dim">No sessions yet — run <code>scsh run</code> to create one.</td></tr>';
    return;
  }
  const existing = new Map();
  body.querySelectorAll('tr[data-session-id]').forEach(row => {
    existing.set(row.getAttribute('data-session-id'), row);
  });
  if (existing.size === 0) {
    body.innerHTML = ids.map(id => indexRowHtml(id, sessions[id], nowUnix)).join('');
    return;
  }
  const nextHtml = ids.map(id => indexRowHtml(id, sessions[id], nowUnix)).join('');
  if (body.innerHTML !== nextHtml) {
    body.innerHTML = nextHtml;
  } else {
    ids.forEach(id => {
      const row = existing.get(id);
      if (row) syncIndexRow(row, sessions[id], nowUnix);
    });
  }
}
function lineCountLabel(n) {
  return n + ' line' + (n === 1 ? '' : 's');
}
function lastLineAt(p) {
  return (p.lines || []).reduce((m, l) => Math.max(m, Number(l.at) || 0), 0);
}
function procElapsed(p, nowUnix) {
  if (p.elapsed != null) return Number(p.elapsed);
  if (p.status === 'running' && p.started_at != null) return Math.max(0, nowUnix - p.started_at);
  return null;
}
function idleSinceLine(p, nowUnix) {
  const elapsed = procElapsed(p, nowUnix);
  if (elapsed == null) return null;
  return Math.max(0, elapsed - lastLineAt(p));
}
function procStatHtml(p, nowUnix) {
  const n = (p.lines || []).length;
  const idle = formatIdleClock(idleSinceLine(p, nowUnix));
  return '<span class="proc-stat" data-proc-stat="' + esc(String(p.index)) + '">' +
    '<span class="line-count">' + esc(lineCountLabel(n)) + '</span>' +
    '<span class="idle">' + idle + '</span></span>';
}
let liveSessions = null;
let lastProcClockSec = null;
function syncProcStat(stat, p, nowUnix, skipIdle) {
  if (!stat) return;
  const lc = stat.querySelector('.line-count');
  setTextUnlessSelecting(lc, lineCountLabel((p.lines || []).length));
  if (!skipIdle) {
    setTextUnlessSelecting(stat.querySelector('.idle'), formatIdleClock(idleSinceLine(p, nowUnix)));
  }
}
function syncProcElapsed(meta, p, nowUnix, liveClock) {
  if (!meta) return;
  if (liveClock && p.status === 'running') return;
  setTextUnlessSelecting(meta, formatElapsedClock(procElapsed(p, nowUnix)));
}
function updateProcClocks(nowUnixSec) {
  if (nowUnixSec === lastProcClockSec) return;
  lastProcClockSec = nowUnixSec;
  if (!SESSION_ID || !liveSessions) return;
  const session = liveSessions[SESSION_ID];
  if (!session) return;
  syncSessionDuration(session, nowUnixSec);
  (session.procs || []).forEach(p => {
    if (p.status !== 'running') return;
    const det = document.querySelector('details.proc[data-index="' + CSS.escape(String(p.index)) + '"]');
    if (!det) return;
    const stat = det.querySelector('[data-proc-stat="' + CSS.escape(String(p.index)) + '"]');
    syncProcStat(stat, p, nowUnixSec, false);
    setTextUnlessSelecting(stat && stat.querySelector('.idle'), formatIdleClock(idleSinceLine(p, nowUnixSec)));
    const meta = det.querySelector('[data-proc-elapsed="' + CSS.escape(String(p.index)) + '"]');
    setTextUnlessSelecting(meta, formatElapsedClock(procElapsed(p, nowUnixSec)));
  });
}
function startProcClock() {
  const tick = () => updateProcClocks(Math.floor(Date.now() / 1000));
  tick();
  setInterval(tick, 1000);
}
function procMetaHtml(p) {
  if (p.kind === 'build') {
    if (!p.harness) return '';
    return '<div class="proc-meta"><span><strong>harness</strong> ' + esc(p.harness) + '</span> ' +
      '<span class="dim">image build</span></div>';
  }
  if (p.kind === 'skill') {
    let skillName = p.skill_name;
    let harness = p.harness;
    if (!skillName || !harness) {
      const m = String(p.label || '').match(/^([^:]+):\s*(.+)$/);
      if (m) {
        harness = harness || m[1].trim();
        skillName = skillName || m[2].trim();
      }
    }
    const parts = [];
    if (skillName) parts.push('<span><strong>skill</strong> <code>' + esc(skillName) + '</code></span>');
    if (harness) parts.push('<span><strong>harness</strong> ' + esc(harness) + '</span>');
    const model = p.model ? esc(p.model) : '<span class="dim">(harness default)</span>';
    parts.push('<span><strong>model</strong> ' + model + '</span>');
    if (p.fail_reason) parts.push('<span><strong>fail reason</strong> <code>' + esc(p.fail_reason) + '</code></span>');
    return '<div class="proc-meta">' + parts.join(' · ') + '</div>';
  }
  return '';
}
function procIsLive(status) {
  return status === 'running' || status === 'waiting';
}
function autoscrollCtlHtml(p) {
  if (!procIsLive(p.status)) return '';
  const scrollChecked = autoScrollEnabled(p.index) ? ' checked' : '';
  return '<label class="autoscroll-ctl"><input type="checkbox" data-autoscroll' + scrollChecked +
    '> Auto-scroll to bottom</label>';
}
function syncAutoscrollCtl(det, p) {
  const live = procIsLive(p.status);
  const ctl = det.querySelector('.autoscroll-ctl');
  if (live) {
    if (!ctl) {
      const label = document.createElement('label');
      label.className = 'autoscroll-ctl';
      label.innerHTML = '<input type="checkbox" data-autoscroll checked> Auto-scroll to bottom';
      const out = det.querySelector('.output');
      if (out) det.insertBefore(label, out);
    }
  } else if (ctl) {
    ctl.remove();
  }
}
const autoScrollByProc = new Map();
function autoScrollEnabled(index) {
  return autoScrollByProc.get(String(index)) !== false;
}
function isAtBottom(el, slack) {
  slack = slack ?? 4;
  return el.scrollHeight - el.scrollTop - el.clientHeight <= slack;
}
function scrollOutputToBottom(out) {
  if (!out) return;
  out._scshAutoScroll = true;
  const go = () => { out.scrollTop = out.scrollHeight; };
  go();
  requestAnimationFrame(() => {
    go();
    requestAnimationFrame(() => {
      go();
      out._scshAutoScroll = false;
    });
  });
}
function applyAutoScrollAll(root) {
  root.querySelectorAll('details.proc').forEach(det => {
    const cb = det.querySelector('[data-autoscroll]');
    const enabled = !cb || cb.checked;
    autoScrollByProc.set(det.dataset.index, enabled);
    if (enabled) scrollOutputToBottom(det.querySelector('.output'));
  });
}
function lineHtml(l) {
  return '<div class="line"><span class="at">+' + esc(Number(l.at).toFixed(1)) + 's</span> ' + esc(l.text) + '</div>';
}
function syncProcOutput(det, p) {
  const out = det.querySelector('.output');
  if (!out) return;
  const lines = p.lines || [];
  let existing = out.querySelectorAll('.line').length;
  if (existing === 0 && out.querySelector('.dim')) {
    out.innerHTML = '';
    existing = 0;
  }
  if (lines.length === 0 && existing === 0) {
    const label = emptyOutputLabel(p.status);
    const dim = out.querySelector('.dim');
    if (!dim) out.innerHTML = emptyOutputHtml(p.status);
    else if (dim.textContent !== label) dim.textContent = label;
    return;
  }
  if (lines.length > existing) {
    const chunk = lines.slice(existing).map(lineHtml).join('');
    out.insertAdjacentHTML('beforeend', chunk);
    if (autoScrollEnabled(det.dataset.index)) scrollOutputToBottom(out);
  }
}
function updateProcFields(det, p, nowUnix) {
  det.className = 'proc ' + p.status;
  const glyphEl = det.querySelector('summary .glyph');
  if (glyphEl) glyphEl.textContent = glyph(p.status);
  const labelEl = det.querySelector('summary .label');
  if (labelEl) labelEl.textContent = p.label || '';
  const stat = det.querySelector('[data-proc-stat="' + CSS.escape(String(p.index)) + '"]');
  syncProcStat(stat, p, nowUnix, p.status === 'running');
  const meta = det.querySelector('[data-proc-elapsed="' + CSS.escape(String(p.index)) + '"]');
  syncProcElapsed(meta, p, nowUnix, p.status === 'running');
  const noteEl = det.querySelector('summary .note');
  if (noteEl) noteEl.textContent = p.note || '';
  const detailEl = det.querySelector('.detail');
  if (detailEl) detailEl.textContent = p.detail || '';
  const containerEl = det.querySelector('.container');
  if (p.container_name) {
    if (containerEl) containerEl.textContent = 'container: ' + p.container_name;
    else {
      const div = document.createElement('div');
      div.className = 'container dim';
      div.textContent = 'container: ' + p.container_name;
      const before = det.querySelector('.cast') || det.querySelector('.autoscroll-ctl') || det.querySelector('.output');
      if (before) det.insertBefore(div, before);
    }
  } else if (containerEl) containerEl.remove();
  // A proc that gained a cast (rendered earlier as text) swaps its output for the embed.
  const castEl = det.querySelector('.cast');
  if (hasCast(p) && !castEl) {
    det.querySelector('.autoscroll-ctl')?.remove();
    det.querySelector('.output')?.remove();
    det.insertAdjacentHTML('beforeend', castEmbedHtml(p));
  } else if (castEl && castEl.dataset.status !== p.status) {
    // On finish, reload once so the player has the complete recording, not the partial one.
    const wasLive = castEl.dataset.status === 'running' || castEl.dataset.status === 'waiting';
    castEl.dataset.status = p.status;
    if (wasLive && (p.status === 'ok' || p.status === 'fail')) createCastPlayer(castEl);
  }
  const metaBlock = det.querySelector('.proc-meta');
  const metaHtml = procMetaHtml(p);
  if (metaHtml) {
    if (metaBlock) metaBlock.outerHTML = metaHtml;
    else {
      const summary = det.querySelector('summary');
      if (summary) summary.insertAdjacentHTML('afterend', metaHtml);
    }
  } else if (metaBlock) metaBlock.remove();
  syncAutoscrollCtl(det, p);
}
function hasCast(p) { return !!p.cast_path && SESSION_ID != null; }
function castEmbedHtml(p) {
  const base = '/cast/' + encodeURIComponent(SESSION_ID) + '/' + p.index;
  return '<div class="cast" data-cast-url="' + esc(base) + '" data-proc="' + esc(String(p.index)) +
    '" data-status="' + esc(p.status) + '">' +
    '<div class="cast-toolbar">' +
    '<button type="button" data-cast-fs>⛶ Fullscreen</button>' +
    '<button type="button" data-cast-link>🔗 Link at time</button>' +
    '<button type="button" data-cast-reload>↻ Reload</button>' +
    '<a href="' + esc(base) + '?dl=1" download>⬇ .cast</a>' +
    '<span class="cast-copied">copied</span>' +
    '<span class="cast-keys dim">space · ←/→ seek · &lt;/&gt; speed · [/] chapter</span>' +
    '</div><div class="cast-player"></div></div>';
}
// Mount an asciinema player into each not-yet-initialised .cast box, and wire its toolbar.
// fit:'both' scales the terminal to fit its box in both dimensions (inline and fullscreen).
function initCasts(root) {
  if (typeof AsciinemaPlayer === 'undefined') return;
  root.querySelectorAll('.cast:not([data-ready])').forEach(box => {
    box.dataset.ready = '1';
    createCastPlayer(box);
    const proc = box.dataset.proc;
    const playUrl = () => location.origin + '/cast/' + encodeURIComponent(SESSION_ID) + '/' + proc + '/play';
    box.querySelector('[data-cast-fs]').addEventListener('click', () => {
      if (document.fullscreenElement === box) document.exitFullscreen();
      else box.requestFullscreen && box.requestFullscreen();
    });
    box.querySelector('[data-cast-reload]').addEventListener('click', () => createCastPlayer(box));
    box.querySelector('[data-cast-link]').addEventListener('click', () => {
      const t = box._player ? Math.floor(box._player.getCurrentTime() * 10) / 10 : 0;
      const url = playUrl() + '#t=' + t;
      if (navigator.clipboard) navigator.clipboard.writeText(url);
      const note = box.querySelector('.cast-copied');
      note.style.visibility = 'visible';
      setTimeout(() => { note.style.visibility = 'hidden'; }, 1200);
    });
  });
}
function createCastPlayer(box) {
  if (typeof AsciinemaPlayer === 'undefined') return;
  const mount = box.querySelector('.cast-player');
  if (box._player) { try { box._player.dispose(); } catch (_) {} box._player = null; }
  mount.innerHTML = '';
  const proc = box.dataset.proc;
  const chaptersUrl = '/cast/' + encodeURIComponent(SESSION_ID) + '/' + proc + '/chapters';
  // Load the analysis sidecar (summary + chapters) if present, then build the player with
  // the chapters as markers (YouTube-style timeline highlights; [ / ] jump between them).
  fetch(chaptersUrl).then(r => r.ok ? r.json() : {}).catch(() => ({})).then(meta => {
    const chapters = (meta.chapters || []).filter(c => typeof c.t === 'number');
    const markers = chapters.map(c => [c.t, String(c.title || '')]);
    // ?ts= busts any HTTP cache so a reload of a still-growing cast fetches fresh bytes.
    box._player = AsciinemaPlayer.create(
      box.dataset.castUrl + '?ts=' + Date.now(), mount,
      { fit: 'both', controls: true, idleTimeLimit: 2, theme: 'asciinema', markers }
    );
    renderCastSummary(box, meta.summary);
    renderChapterChips(box, chapters);
  });
}
function renderCastSummary(box, summary) {
  let el = box.querySelector('.cast-summary');
  if (summary) {
    if (!el) { el = document.createElement('div'); el.className = 'cast-summary'; box.insertBefore(el, box.firstChild); }
    el.textContent = summary;
  } else if (el) el.remove();
}
function renderChapterChips(box, chapters) {
  let bar = box.querySelector('.cast-chapters');
  if (!chapters.length) { if (bar) bar.remove(); return; }
  if (!bar) {
    bar = document.createElement('div');
    bar.className = 'cast-chapters';
    box.querySelector('.cast-toolbar').insertAdjacentElement('afterend', bar);
  }
  bar.innerHTML = chapters.map((c, i) =>
    '<button type="button" data-seek="' + esc(String(c.t)) + '">' +
    esc(fmtClock(c.t)) + ' ' + esc(String(c.title || ('Chapter ' + (i + 1)))) + '</button>'
  ).join('');
  bar.querySelectorAll('[data-seek]').forEach(btn => btn.addEventListener('click', () => {
    if (box._player) { box._player.seek(Number(btn.dataset.seek)); box._player.play && box._player.play(); }
  }));
}
function fmtClock(t) {
  t = Math.max(0, Math.floor(t));
  const m = Math.floor(t / 60), s = t % 60;
  return m + ':' + (s < 10 ? '0' : '') + s;
}
// asciinema-player recomputes its fit on window resize; entering/exiting fullscreen changes
// the box size, so nudge a resize for the fullscreen player to refit both dimensions.
document.addEventListener('fullscreenchange', () => {
  try { window.dispatchEvent(new Event('resize')); } catch (_) {}
});
function procHtml(p, isOpen, nowUnix) {
  const container = p.container_name ? '<div class="container dim">container: ' + esc(p.container_name) + '</div>' : '';
  const body = hasCast(p)
    ? castEmbedHtml(p)
    : autoscrollCtlHtml(p) + '<div class="output">' + ((p.lines || []).map(l => lineHtml(l)).join('') || emptyOutputHtml(p.status)) + '</div>';
  const elapsed = procElapsed(p, nowUnix);
  const elapsedText = formatElapsedClock(elapsed);
  const summaryOpen = '<details class="proc ' + esc(p.status) + '" data-index="' + esc(String(p.index)) + '"' +
    (isOpen ? ' open' : '') + '><summary>' +
    '<span class="triangle" aria-hidden="true"></span><span class="glyph">' + glyph(p.status) + '</span> ' +
    '<span class="label">' + esc(p.label) + '</span> ' + procStatHtml(p, nowUnix) +
    ' <span class="meta" data-proc-elapsed="' + esc(String(p.index)) + '">' + esc(elapsedText) + '</span> ' +
    '<span class="note dim">' + esc(p.note || '') + '</span></summary>';
  return summaryOpen + procMetaHtml(p) + '<div class="detail">' + esc(p.detail || '') + '</div>' +
    container + body + '</details>';
}
function bindSessionProcs(root) {
  if (root.dataset.changeBound) return;
  root.dataset.changeBound = '1';
  root.addEventListener('change', (ev) => {
    if (!ev.target.matches('[data-autoscroll]')) return;
    const det = ev.target.closest('details.proc');
    const out = det && det.querySelector('.output');
    if (!det || !out) return;
    autoScrollByProc.set(det.dataset.index, ev.target.checked);
    if (ev.target.checked) scrollOutputToBottom(out);
  });
}
function setupOutputScroll(out) {
  if (!out || out.dataset.scrollBound) return;
  out.dataset.scrollBound = '1';
  const markUserScroll = () => { out._scshUserScroll = true; };
  out.addEventListener('wheel', markUserScroll, { passive: true });
  out.addEventListener('touchmove', markUserScroll, { passive: true });
  out.addEventListener('keydown', markUserScroll);
  out.addEventListener('mousedown', markUserScroll);
  out.addEventListener('scroll', () => {
    if (out._scshAutoScroll) return;
    const det = out.closest('details.proc');
    if (!det) return;
    if (!out._scshUserScroll) return;
    out._scshUserScroll = false;
    if (!isAtBottom(out)) {
      autoScrollByProc.set(det.dataset.index, false);
      const cb = det.querySelector('[data-autoscroll]');
      if (cb) cb.checked = false;
    }
  }, { passive: true });
}
function renderSession(session, nowUnix) {
  const root = document.getElementById('session-procs');
  if (!root || !session) return;
  const open = new Set([...root.querySelectorAll('details.proc')].filter(d => d.open).map(d => d.dataset.index));
  const procs = session.procs || [];
  procs.forEach(p => {
    const idx = String(p.index);
    const userOpen = open.has(idx);
    let det = root.querySelector('details.proc[data-index="' + CSS.escape(idx) + '"]');
    if (!det) {
      const wrap = document.createElement('div');
      wrap.innerHTML = procHtml(p, false, nowUnix);
      det = wrap.firstElementChild;
      root.appendChild(det);
      setupOutputScroll(det.querySelector('.output'));
      if (procIsLive(p.status)) {
        autoScrollByProc.set(idx, true);
        scrollOutputToBottom(det.querySelector('.output'));
      }
    } else {
      det.open = userOpen;
      updateProcFields(det, p, nowUnix);
      syncProcOutput(det, p);
    }
  });
  initCasts(root);
}
function onTick(msg) {
  if (msg.type !== 'tick') return;
  const alive = msg.alive_clients ?? msg.active_clients ?? 0;
  const nowUnix = msg.now_secs ?? (Date.now() / 1000);
  if (msg.sessions) {
    liveSessions = msg.sessions;
  }
  setScshVersion(msg.scsh_version, msg.scsh_git);
  let label = 'daemon up · ' + msg.mode + ' · ' + alive + ' client' + (alive === 1 ? '' : 's');
  if (msg.mode === 'ephemeral' && msg.shutdown_in_secs != null) {
    label += ' · shutting down in ' + formatDuration(msg.shutdown_in_secs);
  }
  setDaemonStatus('live', label, msg.uptime_secs);
  if (SESSION_ID) {
    const session = liveSessions[SESSION_ID];
    if (session) {
      renderSessionMeta(session, nowUnix);
      renderSession(session, nowUnix);
    }
  } else {
    const snapshot = msg.sessions ?? liveSessions;
    if (snapshot) renderIndex(snapshot, nowUnix);
  }
}
let ws;
let reconnectMs = 400;
function connectWs() {
  setDaemonStatus('connecting', 'connecting…', null);
  ws = new WebSocket('ws://127.0.0.1:' + WS_PORT + '/ws');
  ws.onopen = () => { reconnectMs = 400; setDaemonStatus('connecting', 'connecting…', null); };
  ws.onmessage = (ev) => { try { onTick(JSON.parse(ev.data)); } catch (_) {} };
  ws.onclose = () => {
    setDaemonStatus('connecting', 'connecting…', null);
    setTimeout(connectWs, reconnectMs);
    reconnectMs = Math.min(reconnectMs * 2, 5000);
  };
  ws.onerror = () => { try { ws.close(); } catch (_) {} };
}
connectWs();
startProcClock();
(function initSessionPage() {
  const root = document.getElementById('session-procs');
  if (!root) return;
  initSessionMetaFromDom();
  bindSessionProcs(root);
  root.querySelectorAll('.output').forEach(setupOutputScroll);
  root.querySelectorAll('details.proc').forEach(det => {
    const cb = det.querySelector('[data-autoscroll]');
    if (cb) autoScrollByProc.set(det.dataset.index, cb.checked);
  });
  applyAutoScrollAll(root);
  initCasts(root);
})();
"#
}
