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
const SESSION_STALE_SECS = 30;
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
  return '<span class="chamfer session-status ' + esc(lifecycle.class) + '"><span>' +
    esc(lifecycle.label) + '</span></span>';
}
function setBtnLabel(btn, text) {
  const span = btn.querySelector(':scope > span');
  if (span) span.textContent = text;
  else btn.textContent = text;
}
// In-app confirm dialog (Promise<boolean>). Replaces the browser confirm dialog for Force stop UX.
function scshConfirm(opts) {
  const title = (opts && opts.title) || 'Confirm';
  const body = (opts && opts.body) || '';
  const confirmLabel = (opts && opts.confirmLabel) || 'Confirm';
  const cancelLabel = (opts && opts.cancelLabel) || 'Cancel';
  const danger = !!(opts && opts.danger);
  return new Promise((resolve) => {
    const existing = document.getElementById('scsh-dialog');
    if (existing) existing.remove();
    const backdrop = document.createElement('div');
    backdrop.id = 'scsh-dialog';
    backdrop.className = 'scsh-dialog-backdrop';
    const panel = document.createElement('div');
    panel.className = 'scsh-dialog';
    panel.setAttribute('role', 'alertdialog');
    panel.setAttribute('aria-modal', 'true');
    panel.setAttribute('aria-labelledby', 'scsh-dialog-title');
    const h = document.createElement('p');
    h.id = 'scsh-dialog-title';
    h.className = 'scsh-dialog-title';
    h.textContent = title;
    const p = document.createElement('p');
    p.className = 'scsh-dialog-body';
    p.textContent = body;
    const actions = document.createElement('div');
    actions.className = 'scsh-dialog-actions';
    const cancelBtn = document.createElement('button');
    cancelBtn.type = 'button';
    cancelBtn.className = 'chamfer btn btn--sm btn--muted';
    cancelBtn.innerHTML = '<span></span>';
    cancelBtn.querySelector('span').textContent = cancelLabel;
    const okBtn = document.createElement('button');
    okBtn.type = 'button';
    okBtn.className = 'chamfer btn btn--sm ' + (danger ? 'btn--red' : 'btn--cyan');
    okBtn.innerHTML = '<span></span>';
    okBtn.querySelector('span').textContent = confirmLabel;
    actions.appendChild(cancelBtn);
    actions.appendChild(okBtn);
    panel.appendChild(h);
    panel.appendChild(p);
    panel.appendChild(actions);
    backdrop.appendChild(panel);
    // The dialog steals focus onto OK below, so remember where the user was and put
    // them back on close — otherwise keyboard focus is dumped at <body> and a screen
    // reader loses its place in the page.
    const prevFocus = document.activeElement;
    const finish = (ok) => {
      document.removeEventListener('keydown', onKey, true);
      backdrop.remove();
      if (prevFocus && document.contains(prevFocus) && typeof prevFocus.focus === 'function') prevFocus.focus();
      resolve(ok);
    };
    const onKey = (ev) => {
      if (ev.key === 'Escape') { ev.preventDefault(); finish(false); }
      else if (ev.key === 'Enter' && document.activeElement === okBtn) { ev.preventDefault(); finish(true); }
      else if (ev.key === 'Tab') {
        // Trap Tab inside the modal: aria-modal promises assistive tech that the page
        // behind is inert, so Tab must cycle Cancel ⇄ OK instead of wandering out.
        const focusables = panel.querySelectorAll('button:not(:disabled)');
        if (!focusables.length) return;
        const first = focusables[0], last = focusables[focusables.length - 1];
        if (!panel.contains(document.activeElement)) { ev.preventDefault(); first.focus(); }
        else if (ev.shiftKey && document.activeElement === first) { ev.preventDefault(); last.focus(); }
        else if (!ev.shiftKey && document.activeElement === last) { ev.preventDefault(); first.focus(); }
      }
    };
    backdrop.addEventListener('click', (ev) => { if (ev.target === backdrop) finish(false); });
    cancelBtn.addEventListener('click', () => finish(false));
    okBtn.addEventListener('click', () => finish(true));
    document.addEventListener('keydown', onKey, true);
    document.body.appendChild(backdrop);
    okBtn.focus();
  });
}
function sessionStartedCell(session, nowUnix) {
  const ts = session.started_at || 0;
  const abs = formatUnixTime(ts);
  const rel = formatRelative(nowUnix - ts);
  return '<span class="session-started" data-started="' + esc(String(ts)) + '">' +
    '<span class="session-started-abs">' + esc(abs) + '</span><br>' +
    '<span class="dim session-started-rel">' + esc(rel) + '</span></span>';
}
// Mirrors harness_chips_html in index.rs — keep the markup identical. A running chip's
// tooltip duration lives OUT of the markup (data-tip-running + the tip module's ticker),
// so live re-renders compare equal and the hover survives.
function harnessChipsHtml(session) {
  let out = '';
  (session.procs || []).forEach((p) => {
    if ((p.kind || 'skill') !== 'skill' || !p.harness) return;
    const done = (p.status === 'ok' || p.status === 'graceful' || p.status === 'fail' || p.status === 'skipped');
    const skill = p.skill_name || p.label || '';
    const base = p.harness + ' · ' + skill;
    let tip = base, runningAttr = '';
    if (p.status === 'running' && p.started_at) runningAttr = ' data-tip-running="' + esc(String(p.started_at)) + '"';
    else if (p.status === 'running') tip = base + '\nrunning';
    else if (p.status === 'waiting') tip = base + '\nwaiting';
    else if (p.status === 'ok') tip = base + '\ndone';
    else if (p.status === 'graceful') tip = base + '\ngraceful shutdown';
    else if (p.status === 'fail') tip = base + '\nfailed';
    else tip = base + '\nskipped';
    out += '<span class="hchip hchip--' + esc(p.harness) + (done ? ' hchip--done' : '') + '" data-tip="' +
      esc(tip) + '"' + runningAttr + '>' +
      esc(p.harness.charAt(0).toUpperCase()) + '</span>';
  });
  return out;
}
function chipCountHtml(n) {
  return '<span class="chip-count" data-tip="' + n + ' run' + (n === 1 ? '' : 's') + ' in this job">' + n + '</span>';
}
function indexRowHtml(id, session, nowUnix) {
  const lifecycle = sessionLifecycle(session, nowUnix);
  const profile = session.profile || 'default';
  const n = (session.procs || []).length;
  const duration = sessionDurationLabel(session, nowUnix, lifecycle);
  return '<tr data-session-id="' + esc(id) + '"><td><a class="job-id" href="/job/' + esc(id) + '">' + esc(id) + '</a></td>' +
    '<td class="session-status-cell">' + sessionStatusBadge(lifecycle) + '</td>' +
    '<td class="session-started-cell">' + sessionStartedCell(session, nowUnix) + '</td>' +
    '<td class="session-duration-cell">' + esc(duration) + '</td>' +
    '<td>' + esc(profile) + '</td><td class="session-procs-cell">' + harnessChipsHtml(session) +
    chipCountHtml(n) + '</td>' +
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
  const procsCell = row.querySelector('.session-procs-cell');
  if (procsCell) {
    const next = harnessChipsHtml(session) + chipCountHtml((session.procs || []).length);
    if (procsCell.innerHTML !== next) procsCell.innerHTML = next;
  }
}
// A bare repo-relative artifact path (a system pointer like tmp/scsh/<id>/add.json), as
// opposed to an agent's prose answer. Mirrored by the server-side renderer in session.rs.
function looksLikeArtifactPath(text) {
  return /^(\/|tmp\/|\.harness\/)\S+$/.test(text || '');
}
// Compact single-unit age for dense lists — mirrors format_short_age in format.rs.
function formatShortAge(secsAgo) {
  secsAgo = Math.max(0, Math.floor(secsAgo || 0));
  if (secsAgo < 60) return secsAgo + 's';
  if (secsAgo < 3600) return Math.floor(secsAgo / 60) + 'm';
  if (secsAgo < 86400) return Math.floor(secsAgo / 3600) + 'h';
  return Math.floor(secsAgo / 86400) + 'd';
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
// Mirrors elapsed_phrase in proc.rs — status-aware text before the timer.
function elapsedPhrase(status, elapsed, failReason) {
  const clock = elapsed == null ? null : formatElapsedClock(elapsed);
  if (status === 'waiting') return clock ? 'waiting · ' + clock : 'waiting';
  if (status === 'running') return clock ? 'running for ' + clock : 'running';
  if (status === 'ok') return clock ? 'done in ' + clock : 'done';
  if (status === 'graceful') return clock ? 'graceful shutdown in ' + clock : 'graceful shutdown';
  if (status === 'skipped') return 'skipped';
  if (status === 'fail') {
    if (failReason === 'force_stopped') return clock ? 'force-stopped after ' + clock : 'force-stopped';
    if (failReason === 'container_inactive') return clock ? 'stalled after ' + clock : 'stalled';
    if (failReason === 'container_timeout') return clock ? 'timed out after ' + clock : 'timed out';
    return clock ? 'failed in ' + clock : 'failed';
  }
  return clock || '—';
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
// Wall-clock seconds for the job meta — mirrors Session::duration_secs. A terminated
// (heartbeat-stale) session freezes at last_seen, never keeps ticking as "still running".
function sessionDurationSecs(session, nowUnix) {
  const start = session.started_at || 0;
  if (session.ended_at) return Math.max(0, session.ended_at - start);
  const life = sessionLifecycle(session, nowUnix);
  if (life.class === 'running') return Math.max(0, nowUnix - start);
  if (life.class === 'terminated') {
    const lastSeen = session.last_seen_at || start;
    return Math.max(0, lastSeen - start);
  }
  return 0;
}
function sessionEndedLabel(session, nowUnix) {
  if (session.ended_at) return formatUnixTime(session.ended_at);
  const life = sessionLifecycle(session, nowUnix);
  if (life.class === 'running') return 'still running';
  // Heartbeat-stale: last_seen is the effective end time (badge already says terminated).
  if (life.class === 'terminated') return formatUnixTime(session.last_seen_at || session.started_at);
  return '—';
}
function renderSessionMeta(session, nowUnix) {
  const el = document.getElementById('session-meta');
  if (!el || !session) return;
  const started = formatUnixTime(session.started_at);
  const ended = sessionEndedLabel(session, nowUnix);
  const repo = session.repo || el.dataset.repo || '';
  const branch = session.branch || el.dataset.branch || '—';
  el.dataset.started = String(session.started_at || '');
  el.dataset.ended = session.ended_at ? String(session.ended_at) : '';
  el.dataset.lastSeen = String(session.last_seen_at || session.started_at || '');
  el.dataset.repo = repo;
  el.dataset.branch = branch;
  if (!el.querySelector('[data-session-duration]')) {
    el.innerHTML =
      '<dt>Started</dt><dd data-session-started>' + esc(started) + '</dd>' +
      '<dt>Ended</dt><dd data-session-ended>' + esc(ended) + '</dd>' +
      '<dt>Duration</dt><dd data-session-duration>' +
      esc(formatDuration(sessionDurationSecs(session, nowUnix))) + '</dd>' +
      '<dt>Repo</dt><dd data-session-repo><code class="repo-path">' + esc(repo) + '</code></dd>' +
      '<dt>Branch</dt><dd data-session-branch><code>' + esc(branch) + '</code></dd>';
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
    last_seen_at: Number(el.dataset.lastSeen || el.dataset.started) || 0,
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
  const filter = parseIndexFilter(location.pathname);
  const wantRepo = filter && filter.repo;
  const filtered = {};
  Object.keys(sessions).forEach(id => {
    const s = sessions[id];
    if (!s) return;
    if (wantRepo && s.repo !== wantRepo) return;
    filtered[id] = s;
  });
  const ids = sortSessionIds(filtered, nowUnix);
  if (!ids.length) {
    body.innerHTML = wantRepo
      ? '<tr><td colspan="7" class="dim">No jobs for this project or repository.</td></tr>'
      : '<tr><td colspan="7" class="dim">No jobs yet — run <code>scsh run</code> to start one.</td></tr>';
    return;
  }
  const existing = new Map();
  body.querySelectorAll('tr[data-session-id]').forEach(row => {
    existing.set(row.getAttribute('data-session-id'), row);
  });
  if (existing.size === 0) {
    body.innerHTML = ids.map(id => indexRowHtml(id, filtered[id], nowUnix)).join('');
    return;
  }
  const nextHtml = ids.map(id => indexRowHtml(id, filtered[id], nowUnix)).join('');
  if (body.innerHTML !== nextHtml) {
    body.innerHTML = nextHtml;
  } else {
    ids.forEach(id => {
      const row = existing.get(id);
      if (row) syncIndexRow(row, filtered[id], nowUnix);
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
  setTextUnlessSelecting(meta, elapsedPhrase(p.status, procElapsed(p, nowUnix), p.fail_reason));
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
    setTextUnlessSelecting(meta, elapsedPhrase(p.status, procElapsed(p, nowUnixSec), p.fail_reason));
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
    if (skillName && p.skill_source && /^[A-Za-z0-9_]+-(?:repeat|while-[A-Za-z0-9_]+)-\d+$/.test(skillName)) {
      // Keep generated loop ids in anchors and graph wiring, but show the authored action name.
      skillName = p.skill_source;
    }
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
function uiPrefsKey() {
  return 'scsh.ui.' + (typeof SESSION_ID === 'string' && SESSION_ID ? SESSION_ID : 'index');
}
function loadUiPrefs() {
  try { return JSON.parse(localStorage.getItem(uiPrefsKey()) || '{}') || {}; }
  catch (_) { return {}; }
}
function saveUiPrefs(patch) {
  const next = Object.assign(loadUiPrefs(), patch);
  try { localStorage.setItem(uiPrefsKey(), JSON.stringify(next)); } catch (_) {}
  return next;
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
function followOutput(out) {
  // Sticky follow: stay pinned to the bottom unless the viewer scrolled up.
  return !out || out._scshFollow !== false;
}
function lineHtml(l) {
  return '<div class="line"><span class="at">+' + esc(Number(l.at).toFixed(1)) + 's</span> ' + esc(l.text) + '</div>';
}
function syncProcOutput(det, p) {
  const lines = p.lines || [];
  let out = det.querySelector('.output');
  if (!out) {
    // Unrecorded procs start as slim rows; the output box exists only once the first log
    // line arrives — annotate rows without a recording never grow one.
    if (!lines.length || hasCast(p)) return;
    det.insertAdjacentHTML('beforeend', '<div class="output"></div>');
    out = det.querySelector('.output');
    setupOutputScroll(out);
  }
  const existing = out.querySelectorAll('.line').length;
  if (lines.length > existing) {
    const chunk = lines.slice(existing).map(lineHtml).join('');
    out.insertAdjacentHTML('beforeend', chunk);
    if (followOutput(out)) scrollOutputToBottom(out);
  }
}
function updateProcFields(det, p, nowUnix) {
  det.className = 'proc ' + p.status;
  const labelEl = det.querySelector('summary .label');
  if (labelEl) labelEl.textContent = p.label || '';
  const stat = det.querySelector('[data-proc-stat="' + CSS.escape(String(p.index)) + '"]');
  syncProcStat(stat, p, nowUnix, p.status === 'running');
  const meta = det.querySelector('[data-proc-elapsed="' + CSS.escape(String(p.index)) + '"]');
  syncProcElapsed(meta, p, nowUnix, p.status === 'running');
  const noteEl = det.querySelector('summary .note');
  // Finished rows show their ANSWER (the finish detail) in the collapsed summary; only
  // rows still working show the transient note. A bare artifact path is SYSTEM info and
  // renders as code; anything else is the agent's own text.
  const finished = p.status !== 'running' && p.status !== 'waiting';
  if (noteEl) {
    const text = (finished && p.detail) ? p.detail : (p.note || '');
    if (finished && looksLikeArtifactPath(text)) noteEl.innerHTML = '<code>' + esc(text) + '</code>';
    else noteEl.textContent = text;
  }
  // Per-proc Force stop: show only while the step is live; remove once it finishes.
  const killEl = det.querySelector('button[data-proc-stop]');
  const live = p.status === 'running' || p.status === 'waiting';
  if (killEl && !live) killEl.remove();
  else if (!killEl && live) {
    let actions = det.querySelector('.proc-actions');
    if (!actions) {
      actions = document.createElement('div');
      actions.className = 'proc-actions';
      const summary = det.querySelector('summary');
      if (summary) det.insertBefore(actions, summary);
      else det.prepend(actions);
    }
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'chamfer btn btn--red btn--sm proc-kill';
    btn.setAttribute('data-proc-stop', String(p.index));
    btn.setAttribute('data-proc-kind', p.kind || 'skill');
    btn.setAttribute('data-session', SESSION_ID);
    btn.title = p.kind === 'annotate'
      ? 'Stop this annotation — the recording remains unchanged'
      : 'Force-stop this container only — the rest of the job continues';
    btn.innerHTML = '<span>' + (p.kind === 'annotate' ? 'Stop annotation' : 'Force stop') + '</span>';
    actions.appendChild(btn);
    btn.addEventListener('click', () => killProc(btn));
  }
  // Run snapshot label tracks live vs finished; the link itself is unhidden once frames exist.
  const exportLink = det.querySelector('a[data-cast-export]');
  if (exportLink) {
    const label = live ? 'Incomplete run ⬇' : 'Run snapshot ⬇';
    const span = exportLink.querySelector('span');
    if (span) span.textContent = label;
    else exportLink.textContent = label;
  }
  // A step whose commits were integrated gains its "⇄ commits diff" chip. Integration
  // (and the packdiff pack) happens after the step finished, so this lands on a late tick.
  if (p.diff_path && !det.querySelector('a[data-proc-diff]')) {
    let actions = det.querySelector('.proc-actions');
    if (!actions) {
      actions = document.createElement('div');
      actions.className = 'proc-actions';
      const summary = det.querySelector('summary');
      if (summary) det.insertBefore(actions, summary);
      else det.prepend(actions);
    }
    actions.insertAdjacentHTML('afterbegin', procDiffBtnHtml(p));
    wireProcDiff(actions.querySelector('a[data-proc-diff]'));
  }
  const detailEl = det.querySelector('.detail');
  if (detailEl) detailEl.textContent = p.detail || '';
  const containerEl = det.querySelector('.container');
  if (p.container_name) {
    if (containerEl) containerEl.textContent = 'container: ' + p.container_name;
    else {
      const div = document.createElement('div');
      div.className = 'container dim';
      div.textContent = 'container: ' + p.container_name;
      // A slim row (no recording, no lines yet) has no body element to anchor on, so the
      // container line simply closes out the row until an output box appears above it.
      const before = det.querySelector('.cast') || det.querySelector('.output');
      if (before) det.insertBefore(div, before);
      else det.appendChild(div);
    }
  } else if (containerEl) containerEl.remove();
  // A proc that gained a cast (rendered earlier as text) swaps its output for the embed
  // and gets a run-snapshot link above Force stop.
  const castEl = det.querySelector('.cast');
  if (hasCast(p) && !castEl) {
    det.querySelector('.output')?.remove();
    ensureProcSnapshot(det, p);
    det.insertAdjacentHTML('beforeend', castEmbedHtml(p));
  } else if (castEl && castEl.dataset.status !== p.status) {
    // On finish, reload once so the player has the complete recording, not the partial
    // one; keep the viewer's position and leave live mode (player remounts without live).
    const wasRunning = castEl.dataset.status === 'running' || castEl.dataset.status === 'waiting';
    castEl.dataset.status = p.status;
    if (wasRunning && (p.status === 'ok' || p.status === 'graceful' || p.status === 'fail')) {
      castEl.dataset.ended = String(Math.round(Date.now() / 1000));
      if (castEl._live) setCastLive(castEl, false);
      createCastPlayer(castEl, castEl._player ? castEl._player.getCurrentTime() : null);
    }
  } else if (hasCast(p)) {
    ensureProcSnapshot(det, p);
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
}
function hasCast(p) { return !!p.cast_path && SESSION_ID != null; }
// Insert the run-snapshot link above Force stop when a cast appears mid-job.
function ensureProcSnapshot(det, p) {
  if (det.querySelector('a[data-cast-export]')) return;
  let actions = det.querySelector('.proc-actions');
  if (!actions) {
    actions = document.createElement('div');
    actions.className = 'proc-actions';
    const kill = det.querySelector('button[data-proc-stop]');
    if (kill && kill.parentElement === det) {
      det.insertBefore(actions, kill);
      actions.appendChild(kill);
    } else {
      const summary = det.querySelector('summary');
      if (summary) det.insertBefore(actions, summary);
      else det.prepend(actions);
      if (kill) actions.appendChild(kill);
    }
  }
  const live = p.status === 'running' || p.status === 'waiting';
  const label = live ? 'Incomplete run ⬇' : 'Run snapshot ⬇';
  const href = '/cast/' + encodeURIComponent(SESSION_ID) + '/' + p.index + '/export.html';
  const a = document.createElement('a');
  a.className = 'chamfer btn btn--cyan btn--sm proc-snapshot';
  a.href = href;
  a.setAttribute('data-cast-export', '');
  a.setAttribute('download', '');
  a.hidden = true;
  a.title = 'Offline HTML snapshot of this run';
  a.innerHTML = '<span>' + label + '</span>';
  actions.appendChild(a);
}
// Mirrors proc_diff_btn_html in session.rs.
function procDiffBtnHtml(p) {
  return '<a class="chamfer btn btn--purple btn--sm proc-diff" data-proc-diff href="/diff/' +
    encodeURIComponent(SESSION_ID) + '/' + p.index +
    '" title="Browse the commits this step brought into your branch — one self-contained review page"><span>⇄ commits diff</span></a>';
}
// The chip lives inside the <summary>; keep a click on it from toggling the details row.
function wireProcDiff(a) {
  if (a) a.addEventListener('click', (ev) => ev.stopPropagation());
}
function initProcDiffs(root) {
  (root || document).querySelectorAll('a[data-proc-diff]').forEach(wireProcDiff);
}
function castEmbedHtml(p) {
  const base = '/cast/' + encodeURIComponent(SESSION_ID) + '/' + p.index;
  const ended = (p.started_at && p.elapsed != null && p.status !== 'running' && p.status !== 'waiting')
    ? ' data-ended="' + Math.round(p.started_at + p.elapsed) + '"' : '';
  return '<div class="cast" data-cast-url="' + esc(base) + '" data-proc="' + esc(String(p.index)) +
    '" data-status="' + esc(p.status) + '"' + ended + '>' +
    '<div class="cast-toolbar">' +
    '<a href="' + esc(base) + '?dl=1" download>⬇ .cast</a>' +
    '<span class="cast-keys dim">space · ←/→ seek · &lt;/&gt; speed<span data-chapter-keys></span> · f fullscreen</span>' +
    '</div><div class="cast-player"></div></div>';
}
// Mount an asciinema player into each not-yet-initialised .cast box, and wire its toolbar.
// fit:'both' scales the terminal to fit its box in both dimensions (inline and fullscreen).
function initCasts(root) {
  if (typeof BeeCastPlayer === 'undefined') return;
  root.querySelectorAll('.cast:not([data-ready])').forEach(box => {
    box.dataset.ready = '1';
    // Opening a section hands its player the keyboard: space plays, f fullscreens.
    const det = box.closest('details');
    if (det) det.addEventListener('toggle', () => { if (det.open) focusCastPlayer(box); });
    // Still-running recordings start live (player toolbar ● Live; seek back to leave).
    // The player owns live state and suppresses the play overlay while following.
    box.addEventListener('beecast-livechange', (e) => {
      box._live = !!(e.detail && e.detail.live);
    });
    if (box.dataset.status === 'running') { box._live = true; createCastPlayer(box, 'end'); }
    else createCastPlayer(box);
  });
}
// The available duration and event count of loaded asciicast text (complete lines only —
// the cast endpoint truncates to whole lines). scsh records asciicast v3, where event
// times are intervals (duration = sum); a legacy v2 header (absolute times) takes the max.
function castEventStats(text) {
  let version = 3, duration = 0, events = 0;
  for (const raw of String(text || '').split('\n')) {
    const line = raw.trim();
    if (!line || line[0] === '#') continue;
    if (line[0] === '{') {
      try { version = Number(JSON.parse(line).version) || 3; } catch (_) {}
      continue;
    }
    if (line[0] !== '[') continue;
    const t = parseFloat(line.slice(1));
    if (!isFinite(t)) continue;
    events++;
    duration = version === 3 ? duration + t : Math.max(duration, t);
  }
  return { events, duration };
}
function castPlaceholderHtml(status) {
  const live = status === 'running' || status === 'waiting';
  return '<div class="cast-placeholder dim">' +
    (live ? 'Recording in progress — no frames yet.' : 'No recorded frames.') + '</div>';
}
// (Re-)create the player for a .cast box: fetch the cast text (cache-busted) and the
// chapters sidecar together, then either mount the player over the inline data or — for a
// cast with no complete event lines yet (a run that just started) — show a calm placeholder
// instead of letting the player error on the empty/404 cast. The placeholder upgrades to a
// real player on the next reload (a WS cast_growth notification, or the finish reload).
function createCastPlayer(box, startAt, autoplay) {
  if (typeof BeeCastPlayer === 'undefined') return;
  const mount = box.querySelector('.cast-player');
  if (box._player) { try { box._player.dispose(); } catch (_) {} box._player = null; }
  box._loading = true;
  const proc = box.dataset.proc;
  const chaptersUrl = '/cast/' + encodeURIComponent(SESSION_ID) + '/' + proc + '/chapters';
  Promise.all([
    // ?ts= busts any HTTP cache so a reload of a still-growing cast fetches fresh bytes.
    fetch(box.dataset.castUrl + '?ts=' + Date.now()).then(r => r.ok ? r.text() : null).catch(() => null),
    // The analysis sidecar (summary + chapters): chapters become markers on the timeline
    // (YouTube-style highlights; [ / ] jump between them).
    fetch(chaptersUrl).then(r => r.ok ? r.json() : {}).catch(() => ({})),
  ]).then(([text, meta]) => {
    box._loading = false;
    const stats = text == null ? { events: 0, duration: 0 } : castEventStats(text);
    box._loadedDuration = stats.events ? stats.duration : null;
    // The .html export needs at least one complete frame (the server 404s otherwise), so
    // the download link rides the same no-frames state as the placeholder.
    const det = box.closest('details.proc');
    const exportLink = (det || box).querySelector('[data-cast-export]');
    if (exportLink) exportLink.hidden = !stats.events;
    if (!stats.events) {
      mount.innerHTML = castPlaceholderHtml(box.dataset.status);
      return;
    }
    mount.innerHTML = '';
    const chapters = (meta.chapters || []).filter(c => typeof c.t === 'number');
    setChapterKeys(box, chapters.length > 0);
    // Chapters are player chrome (the ☰ panel + seek-bar ticks + [/] keys): markers are
    // ALL the wiring they need. scsh renders only the one-line summary above the player.
    const markers = chapters.map(c => [c.t, String(c.title || '')]);
    // Beecast owns fullscreen. Its player root fills the display without carrying the
    // surrounding scsh toolbar into a mode whose only purpose is watching the recording.
    const running = box.dataset.status === 'running';
    const opts = {
      fit: 'both',
      controls: running ? { live: true } : true,
      idleTimeLimit: 2,
      markers,
      accessibility: 'snapshot',
      // Still-running: start declared-live (no play overlay) until the viewer seeks back.
      live: !!(box._live || running),
    };
    if (startAt === 'end') startAt = stats.duration;
    if (startAt != null) opts.startAt = Math.max(0, Math.min(startAt, stats.duration));
    // The text is passed inline ({ data }) — it was already fetched to decide placeholder
    // vs player, so the player must not fetch it a second time. `_loadedChars` marks how
    // much of the recording the player holds; live growth appends only the suffix.
    box._player = BeeCastPlayer.create({ data: text }, mount, opts);
    box._loadedChars = text.length;
    if (autoplay) { try { box._player.play(); } catch (_) {} }
    // Keyboard-first: a player mounting into an OPEN section takes focus, so space
    // (play/pause) and f (fullscreen) work immediately. Never steal focus from something
    // the user is actually in — only take it from the body or from this box's own
    // (just-disposed) previous player.
    const active = document.activeElement;
    if ((!det || det.open) && (!active || active === document.body || box.contains(active))) focusCastPlayer(box);
    if (box._live || running) setCastLive(box, true);
    renderCastSummary(box, meta.summary);
    renderAnnotationLink(box, meta);
    // Chapters are written by the annotation pass AFTER the run ends; a finished cast with
    // none yet shows a clear "summarizing…" element and swaps the chapters in live when the
    // sidecar lands — no browser refresh. (Polling stops quietly if annotation never comes,
    // e.g. no annotator on the host.) An annotator recording is already the act of
    // producing chapters for another cast; recursively summarizing it is nonsensical.
    if (!chapters.length && box.dataset.status !== 'running' && box.dataset.kind !== 'annotate') {
      pollForChapters(box, chaptersUrl);
    }
  });
}
function focusCastPlayer(box) {
  const root = box.querySelector('.beecast-player');
  if (!root) return;
  try { root.focus({ preventScroll: true }); } catch (_) { try { root.focus(); } catch (_) {} }
}
function setChapterKeys(box, hasChapters) {
  const hint = box.querySelector('[data-chapter-keys]');
  if (hint) hint.textContent = hasChapters ? ' · [/] chapter · c chapters' : '';
}
// The annotation pass starts right after the run ends, so chapters land within minutes or
// never (no annotator on the host, or a recording from before annotation existed). Show the
// indicator and poll only inside that window — an old cast gets neither.
const CHAPTERS_WAIT_SECS = 300;
// Persistent annotation state: an animated pen while active, then a durable colored link.
function renderAnnotationLink(box, meta) {
  if (!box || box.dataset.kind === 'annotate') return;
  const bar = box.querySelector('.cast-toolbar');
  if (!bar) return;
  let link = bar.querySelector('.annotation-link');
  const job = meta && meta.annotation_job;
  const status = meta && meta.annotation_status;
  if (!job || !status) { if (link) link.remove(); return; }
  if (!link) { link = document.createElement('a'); bar.appendChild(link); }
  link.href = '/job/' + encodeURIComponent(job) + '#proc-' + Number(meta.annotation_proc || 0);
  link.className = 'annotation-link annotation-link--' + status + (status === 'running' ? ' chap-pending' : '');
  link.innerHTML = status === 'running'
    ? '<span aria-hidden="true">🖊</span> annotating<span class="annotation-dots" aria-hidden="true"></span>'
    : (status === 'ok' ? '✓ annotation complete' : '✗ annotation failed');
}
function pollForChapters(box, chaptersUrl) {
  if (box._chapPoll) return;
  const endedAt = Number(box.dataset.ended || 0);
  const sinceEnd = () => Date.now() / 1000 - endedAt;
  if (!endedAt || sinceEnd() > CHAPTERS_WAIT_SECS) return;
  // liveSessions is null until the first WS tick — never index it bare (SESSION_ID is a
  // property name; `null[SESSION_ID]` throws TypeError in the console).
  const sessionForChapters = () => (SESSION_ID && liveSessions ? liveSessions[SESSION_ID] : null);
  {
    const s = sessionForChapters();
    if (s) syncChaptersPending(s);
  }
  box._chapPoll = setInterval(() => {
    if (sinceEnd() > CHAPTERS_WAIT_SECS) { clearInterval(box._chapPoll); box._chapPoll = null; const s = sessionForChapters(); if (s) syncChaptersPending(s); return; }
    fetch(chaptersUrl).then(r => r.ok ? r.json() : {}).then(meta => {
      renderAnnotationLink(box, meta);
      const chapters = (meta.chapters || []).filter(c => typeof c.t === 'number');
      if (!chapters.length) {
        return;
      }
      clearInterval(box._chapPoll);
      box._chapPoll = null;
      // Re-create at the same position so the timeline gains its markers too.
      createCastPlayer(box, box._player ? box._player.getCurrentTime() : null);
      const s = sessionForChapters();
      if (s) syncChaptersPending(s);
    }).catch(() => {});
  }, 5000);
}
// Live mode: while the proc runs, follow the tail of the recording as it grows.
//
// Mechanism, chosen deliberately: rather than a dedicated per-cast streaming WS endpoint
// next to the daemon's single JSON broadcast hub, live mode rides the hub's existing
// cast_growth notifications: each one re-fetches the cast (cheap; the bytes are local), re-creates
// the player seeked to where the previous load ended, and plays the newly appended tail.
// When the proc finishes, the status-change reload loads the complete cast and the
// toggle turns off and hides.
function setCastLive(box, on) {
  box._live = !!on;
  // Declared-live (player.setLive): parked at the growing edge, appends pinned
  // unconditionally, the bar full-width in live green. The player drops it itself on a
  // rewind (beecast-livechange re-syncs box._live); ● Live lives in the player toolbar.
  if (!box._player) return;
  if (box._live) {
    followCastGrowth(box);
    box._player.setLive(true);
  } else {
    box._player.setLive(false);
  }
}
// A server-pushed cast_growth notification for this session: upgrade a placeholder to a
// player as soon as the first frames exist, otherwise append the newly recorded suffix in
// place — the player grows smoothly, with no re-creation, no seek, and no reload banner.
// A viewer parked at the live edge sees the new frames immediately; one who paused or
// seeked back just watches the duration grow. The final running:false notice needs no
// action — the finish reload is driven by the proc's status change in the tick payload.
function onCastGrowth(msg) {
  if (!SESSION_ID || msg.session !== SESSION_ID) return;
  const det = document.querySelector('details.proc[data-index="' + CSS.escape(String(msg.proc)) + '"]');
  const box = det && det.querySelector('.cast[data-ready]');
  if (!box || box._loading) return;
  if (msg.running === false) return;
  if (box._loadedDuration == null) { createCastPlayer(box); return; }
  followCastGrowth(box);
}
// Fetch the (local, append-only) recording and hand the player only the bytes it has not
// seen; partial trailing lines are the player's problem (it buffers them internally).
function followCastGrowth(box) {
  if (!box._player || box._appending) return;
  box._appending = true;
  fetch(box.dataset.castUrl).then(r => r.ok ? r.text() : null).then(text => {
    box._appending = false;
    if (text == null || !box._player) return;
    const prev = box._loadedChars || 0;
    if (text.length <= prev) return;
    box._player.append(text.slice(prev));
    box._loadedChars = text.length;
    box._loadedDuration = box._player.cast.duration;
  }).catch(() => { box._appending = false; });
}
function renderCastSummary(box, summary) {
  let el = box.querySelector('.cast-summary');
  if (summary) {
    if (!el) { el = document.createElement('div'); el.className = 'cast-summary'; box.insertBefore(el, box.firstChild); }
    el.textContent = summary;
  } else if (el) el.remove();
}
// Entering/exiting fullscreen changes the box size: refit the player (beecast-player
// re-lays-out on window resize). Chapters are player chrome now — the ☰ panel rides
// into fullscreen with the player; no scsh-side sidebar to manage.
document.addEventListener('fullscreenchange', () => {
  try { window.dispatchEvent(new Event('resize')); } catch (_) {}
});
function procHtml(p, isOpen, nowUnix) {
  const container = p.container_name ? '<div class="container dim">container: ' + esc(p.container_name) + '</div>' : '';
  // Mirrors the server-rendered shape (session.rs): recorded procs embed the player;
  // text-logging procs keep the output box (sticky follow); a proc with neither — an
  // annotate row without a recording, say — stays a slim summary-only row.
  const lines = p.lines || [];
  const body = hasCast(p)
    ? castEmbedHtml(p)
    : (lines.length ? '<div class="output">' + lines.map(l => lineHtml(l)).join('') + '</div>' : '');
  const elapsedText = elapsedPhrase(p.status, procElapsed(p, nowUnix), p.fail_reason);
  const step = workflowStepIdForProc(p);
  const taskAttrs = step ? ' id="task-' + esc(step) + '" data-workflow-step="' + esc(step) + '"' : '';
  const live = p.status === 'running' || p.status === 'waiting';
  const snapLabel = live ? 'Incomplete run ⬇' : 'Run snapshot ⬇';
  const snap = hasCast(p)
    ? '<a class="chamfer btn btn--cyan btn--sm proc-snapshot" href="/cast/' + encodeURIComponent(SESSION_ID) +
      '/' + p.index + '/export.html" data-cast-export download hidden title="Offline HTML snapshot of this run"><span>' +
      snapLabel + '</span></a>'
    : '';
  const diff = p.diff_path ? procDiffBtnHtml(p) : '';
  const kill = live
    ? '<button type="button" class="chamfer btn btn--red btn--sm proc-kill" data-proc-stop="' +
      esc(String(p.index)) + '" data-proc-kind="' + esc(p.kind || 'skill') + '" data-session="' + esc(SESSION_ID) +
      '" title="' + (p.kind === 'annotate' ? 'Stop this annotation — the recording remains unchanged' :
        'Force-stop this container only — the rest of the job continues') + '"><span>' +
      (p.kind === 'annotate' ? 'Stop annotation' : 'Force stop') + '</span></button>'
    : '';
  const summaryOpen = '<details class="proc ' + esc(p.status) + '" data-index="' + esc(String(p.index)) + '"' + taskAttrs +
    (isOpen ? ' open' : '') + '>' +
    ((diff || snap || kill) ? '<div class="proc-actions">' + diff + snap + kill + '</div>' : '') +
    '<summary>' +
    '<span class="triangle" aria-hidden="true"></span> ' +
    '<span class="label">' + esc(p.label) + '</span> ' + procStatHtml(p, nowUnix) +
    ' <span class="meta" data-proc-elapsed="' + esc(String(p.index)) + '">' + esc(elapsedText) + '</span> ' +
    '<span class="note dim">' + esc(p.note || '') + '</span></summary>';
  return summaryOpen + procMetaHtml(p) + '<div class="detail">' + esc(p.detail || '') + '</div>' +
    container + body + '</details>';
}
function workflowStepIdForProc(p) {
  const session = SESSION_ID && liveSessions ? liveSessions[SESSION_ID] : null;
  const nodes = session && session.workflow && session.workflow.nodes;
  if (nodes) {
    const hit = nodes.find(n => n.proc_index === p.index);
    if (hit) return hit.id;
  }
  if (p.kind === 'build') return p.harness ? ('build_' + p.harness) : 'build_base';
  return p.skill_name || p.skill_source || null;
}
function wfStateIcon(state) {
  return ({waiting:'○',ready:'○',running:'◉',done:'✓',graceful:'!',failed:'✗','force-stopped':'✕',skipped:'⊘',stalled:'!'})[state] || '○';
}
function wfStateLabel(state) {
  return ({waiting:'Waiting',ready:'Ready',running:'Running',done:'Succeeded',graceful:'Graceful shutdown',failed:'Failed',
    'force-stopped':'Force-stopped',skipped:'Skipped',stalled:'Abandoned'})[state] || state;
}
function wfJobOutcome(session, nowUnix) {
  const life = sessionLifecycle(session, nowUnix);
  const text = ({running:'Job running',completed:'Job succeeded',failed:'Job failed',
    cancelled:'Job cancelled',terminated:'Job terminated abruptly'})[life.class] || ('Job ' + life.label);
  return { className: life.class, text };
}
function wfJobOutcomeHtml(session, nowUnix) {
  const outcome = wfJobOutcome(session, nowUnix);
  return '<span class="workflow-outcome workflow-outcome--' + outcome.className +
    '" data-workflow-outcome>' + outcome.text + '</span>';
}
function wfUnmetNeedIds(session, node) {
  const nodes = (session.workflow && session.workflow.nodes) || [];
  const byId = Object.fromEntries(nodes.map(n => [n.id, n]));
  const procs = session.procs || [];
  const terminal = s => s === 'ok' || s === 'graceful' || s === 'fail' || s === 'skipped';
  return (node.needs || []).filter(need => {
    const n = byId[need];
    if (!n || n.proc_index == null) return true;
    const p = procs.find(x => x.index === n.proc_index);
    return !p || !terminal(p.status);
  });
}
function wfUnmetNeeds(session, node) {
  return wfUnmetNeedIds(session, node).length;
}
function wfNodeTitle(id) {
  if (id === 'build_base') return 'base';
  if (id.indexOf('build_') === 0) return id.slice(6);
  const looped = id.match(/^([A-Za-z0-9_]+)-(?:repeat|while-[A-Za-z0-9_]+)-(\d+)$/);
  if (looped) return looped[1] + ' · iteration ' + looped[2];
  return id;
}
function wfBlockerLine(session, id, nowUnix) {
  const nodes = (session.workflow && session.workflow.nodes) || [];
  const dep = nodes.find(n => n.id === id);
  const title = wfNodeTitle(id);
  const isBuild = id === 'build_base' || id.indexOf('build_') === 0;
  const kind = isBuild ? 'image build' : 'task';
  if (!dep) return title + ' (missing)';
  const p = (session.procs || []).find(x => x.index === dep.proc_index);
  if (!p) return title + ' (' + kind + ', not registered yet)';
  const st = wfDisplayState(session, dep, nowUnix);
  const bits = [];
  if (!isBuild && p.harness) bits.push(p.harness);
  bits.push(kind, st);
  return title + ' (' + bits.join(' · ') + ')';
}
function wfNodeTip(session, node, state, unmetIds, nowUnix) {
  const title = wfNodeTitle(node.id);
  const lines = [title];
  if (state === 'waiting' && unmetIds.length) {
    lines.push('Waiting on:');
    unmetIds.forEach(id => lines.push('• ' + wfBlockerLine(session, id, nowUnix)));
  } else if (state === 'waiting') lines.push('Waiting to start');
  else if (state === 'ready') lines.push('Ready — dependencies finished; not started yet');
  else if (state === 'running') {
    lines.push((node.id === 'build_base' || node.id.indexOf('build_') === 0) ? 'Image build running' : 'Running');
  }   else if (state === 'done') lines.push('Succeeded');
  else if (state === 'graceful') lines.push('Graceful shutdown — valid result and inner exit 0');
  else if (state === 'failed') lines.push('Failed');
  else if (state === 'force-stopped') lines.push('Force-stopped from the session browser');
  else if (state === 'skipped') lines.push('Skipped');
  else if (state === 'stalled') lines.push('Abandoned — job stopped updating');
  if (node.conditional && state !== 'skipped') lines.push('Runs only when its gate passes');
  return lines.join('\n');
}
function wfDisplayState(session, node, nowUnix) {
  const life = sessionLifecycle(session, nowUnix).class;
  // Ready/Running only while the job is live — cancelled/terminated/failed must not keep a
  // waiting step looking like it is about to start ("ready — not started yet").
  const live = life === 'running';
  const procs = session.procs || [];
  const p = node.proc_index != null ? procs.find(x => x.index === node.proc_index) : null;
  if (!p) return live ? 'waiting' : 'stalled';
  if (p.status === 'ok') return 'done';
  if (p.status === 'graceful') return 'graceful';
  if (p.status === 'fail') {
    return p.fail_reason === 'force_stopped' ? 'force-stopped' : 'failed';
  }
  if (p.status === 'skipped') return 'skipped';
  if (p.status === 'running') return live ? 'running' : 'stalled';
  if (p.status === 'waiting') {
    if (!live) return 'stalled';
    return wfUnmetNeeds(session, node) === 0 ? 'ready' : 'waiting';
  }
  return 'waiting';
}
function wfLegendHtml(present) {
  const order = ['running','done','graceful','failed','force-stopped','stalled','waiting','ready','skipped'];
  const items = order.filter(s => present[s]).map(s =>
    '<li class="wf-leg wf-leg-' + s + '"><span class="wf-ico" aria-hidden="true">' +
    wfStateIcon(s) + '</span> ' + wfStateLabel(s) + '</li>'
  ).join('');
  return items ? '<ul class="workflow-legend" aria-label="Status legend">' + items + '</ul>' : '';
}
function wfSummaryHtml(counts, total, first) {
  const parts = [total + (total === 1 ? ' task' : ' tasks')];
  const shown = (key) => key === 'done' ? 'succeeded' : (key === 'stalled' ? 'abandoned' :
    (key === 'graceful' ? 'graceful shutdown' : key));
  for (const [n, label] of [[counts.done,'done'],[counts.graceful,'graceful'],[counts.running,'running'],[counts.waiting,'waiting'],
    [counts.ready,'ready'],[counts.failed,'failed'],[counts.force_stopped,'force-stopped'],
    [counts.stalled,'stalled'],[counts.skipped,'skipped']]) {
    if (n <= 0) continue;
    const id = first && first[label];
    const word = shown(label);
    if (id) {
      parts.push('<a class="wf-jump" href="' + '#task-' + encodeURIComponent(id) +
        '" data-wf-status="' + label + '" title="Jump to first ' + word + ' task">' +
        n + ' ' + word + '</a>');
    } else {
      parts.push(n + ' ' + word);
    }
  }
  return parts.join(' · ');
}
function wfFirstIdByState(session, nodes, nowUnix) {
  const layout = wfLayoutNodes(session, nodes, nowUnix).slice().sort((a, b) => a.y - b.y || a.x - b.x);
  const first = Object.create(null);
  layout.forEach(pos => {
    const node = nodes.find(n => n.id === pos.id);
    if (!node) return;
    const st = wfDisplayState(session, node, nowUnix);
    if (first[st] == null) first[st] = node.id;
  });
  return first;
}
// Layout constants — keep in lockstep with src/daemon/html/workflow.rs.
const WF_NODE_W = 200, WF_NODE_H = 72, WF_GAP_X = 56, WF_GAP_Y = 28, WF_PAD = 16;
const WF_BOOKEND_W = 48, WF_BOOKEND_H = 48;
const WF_START_ID = '__start', WF_FINISH_ID = '__finish';
let pendingWorkflowStep = null;
let wfHistorySilent = false;
let workflowZoom = 1;
let workflowExpanded = false;
function wfNodeRanks(nodes) {
  const byId = Object.fromEntries(nodes.map(n => [n.id, n]));
  const ranks = Object.create(null);
  function rankOf(id) {
    if (ranks[id] != null) return ranks[id];
    const node = byId[id];
    if (!node) { ranks[id] = 0; return 0; }
    const needs = node.needs || [];
    const r = needs.length ? (1 + Math.max(...needs.map(rankOf))) : 0;
    ranks[id] = r;
    return r;
  }
  return nodes.map(n => rankOf(n.id));
}
function wfStatusStackRank(state) {
  return ({done:0,failed:1,'force-stopped':2,skipped:3,running:4,stalled:5,ready:6,waiting:7})[state] ?? 9;
}
function wfLayoutNodes(session, nodes, nowUnix) {
  const ranks = wfNodeRanks(nodes);
  const byRank = Object.create(null);
  nodes.forEach((n, i) => {
    const r = ranks[i];
    (byRank[r] || (byRank[r] = [])).push(i);
  });
  Object.keys(byRank).forEach(r => byRank[r].sort((a, b) => {
    const sa = wfStatusStackRank(wfDisplayState(session, nodes[a], nowUnix));
    const sb = wfStatusStackRank(wfDisplayState(session, nodes[b], nowUnix));
    return sa - sb || (nodes[a].order || 0) - (nodes[b].order || 0) ||
      String(nodes[a].id).localeCompare(String(nodes[b].id));
  }));
  const maxInRank = Math.max(1, ...Object.keys(byRank).map(r => byRank[r].length));
  const colH = maxInRank * WF_NODE_H + (maxInRank - 1) * WF_GAP_Y;
  const out = [];
  Object.keys(byRank).map(Number).sort((a, b) => a - b).forEach(rank => {
    const idxs = byRank[rank];
    const n = idxs.length;
    const blockH = n * WF_NODE_H + (n - 1) * WF_GAP_Y;
    const y0 = WF_PAD + (colH - blockH) / 2;
    const x = WF_PAD + rank * (WF_NODE_W + WF_GAP_X);
    idxs.forEach((i, row) => {
      out.push({ id: nodes[i].id, x, y: y0 + row * (WF_NODE_H + WF_GAP_Y), order: nodes[i].order || 0, index: i, w: WF_NODE_W, h: WF_NODE_H });
    });
  });
  out.sort((a, b) => a.order - b.order);
  return out;
}
function wfLayoutWithBookends(session, nodes, nowUnix) {
  const layout = wfLayoutNodes(session, nodes, nowUnix);
  const shift = WF_BOOKEND_W + WF_GAP_X;
  const loopTop = nodes.some(n => /^[A-Za-z0-9_]+-(?:repeat|while-[A-Za-z0-9_]+)-\d+$/.test(n.id)) ? 36 : 0;
  layout.forEach(n => { n.x += shift; n.y += loopTop; });
  const rootIds = wfGraphRoots(nodes), sinkIds = wfGraphSinks(nodes);
  const centerY = ids => {
    const rows = layout.filter(n => ids.includes(n.id));
    return rows.length ? rows.reduce((sum, n) => sum + n.y + n.h / 2, 0) / rows.length : WF_PAD + WF_NODE_H / 2;
  };
  const startY = Math.max(WF_PAD, centerY(rootIds) - WF_BOOKEND_H / 2);
  const finishY = Math.max(WF_PAD, centerY(sinkIds) - WF_BOOKEND_H / 2);
  const start = { id: WF_START_ID, x: WF_PAD, y: startY, order: 0, w: WF_BOOKEND_W, h: WF_BOOKEND_H };
  const finishX = Math.max(WF_PAD + WF_BOOKEND_W, ...layout.map(n => n.x + n.w)) + WF_GAP_X;
  const finish = { id: WF_FINISH_ID, x: finishX, y: finishY, order: 1e9, w: WF_BOOKEND_W, h: WF_BOOKEND_H };
  return { layout, start, finish };
}
function wfGraphRoots(nodes) {
  const ids = new Set(nodes.map(n => n.id));
  return nodes.filter(n => (n.needs || []).every(need => !ids.has(need))).map(n => n.id);
}
function wfGraphSinks(nodes) {
  const ids = new Set(nodes.map(n => n.id));
  const dependedOn = new Set();
  nodes.forEach(n => (n.needs || []).forEach(need => { if (ids.has(need)) dependedOn.add(need); }));
  return nodes.filter(n => !dependedOn.has(n.id)).map(n => n.id);
}
function wfPortY(nodeY, nodeH, index, count) {
  if (count <= 1) return nodeY + nodeH / 2;
  const margin = nodeH * 0.22;
  const usable = nodeH - 2 * margin;
  return nodeY + margin + usable * index / (count - 1);
}
function wfEdgesSvg(nodes, layout, start, finish) {
  const byId = Object.fromEntries(layout.map(n => [n.id, n]));
  byId[WF_START_ID] = start;
  byId[WF_FINISH_ID] = finish;
  const pairs = [];
  nodes.forEach(node => {
    (node.needs || []).forEach(need => {
      if (byId[need] && byId[node.id]) pairs.push([need, node.id]);
    });
  });
  wfGraphRoots(nodes).forEach(id => { if (byId[id]) pairs.push([WF_START_ID, id]); });
  wfGraphSinks(nodes).forEach(id => { if (byId[id]) pairs.push([id, WF_FINISH_ID]); });
  const outN = Object.create(null), inN = Object.create(null);
  pairs.forEach(([s, d]) => { outN[s] = (outN[s] || 0) + 1; inN[d] = (inN[d] || 0) + 1; });
  const outRank = Object.create(null), inRank = Object.create(null);
  pairs.forEach((p, i) => {
    (outRank[p[0]] || (outRank[p[0]] = [])).push(i);
    (inRank[p[1]] || (inRank[p[1]] = [])).push(i);
  });
  Object.keys(outRank).forEach(src => outRank[src].sort((a, b) => byId[pairs[a][1]].y - byId[pairs[b][1]].y || a - b));
  Object.keys(inRank).forEach(dst => inRank[dst].sort((a, b) => byId[pairs[a][0]].y - byId[pairs[b][0]].y || a - b));
  const outPort = Object.create(null), inPort = Object.create(null);
  Object.keys(outRank).forEach(src => outRank[src].forEach((ei, port) => { outPort[ei] = port; }));
  Object.keys(inRank).forEach(dst => inRank[dst].forEach((ei, port) => { inPort[ei] = port; }));
  return pairs.map((p, i) => {
    const src = byId[p[0]], dst = byId[p[1]];
    const x1 = src.x + (src.w || WF_NODE_W);
    const y1 = wfPortY(src.y, src.h || WF_NODE_H, outPort[i], outN[p[0]]);
    const x2 = dst.x - 1.5;
    const y2 = wfPortY(dst.y, dst.h || WF_NODE_H, inPort[i], inN[p[1]]);
    if (Math.abs(y1 - y2) < 0.5) {
      return '<path class="wf-edge" d="M' + x1.toFixed(1) + ',' + y1.toFixed(1) +
        ' L' + x2.toFixed(1) + ',' + y1.toFixed(1) + '" marker-end="url(#wf-arrow)" />';
    }
    const dx = Math.max(24, x2 - x1);
    const c1x = x1 + dx * 0.42, c2x = x2 - dx * 0.42;
    return '<path class="wf-edge" d="M' + x1.toFixed(1) + ',' + y1.toFixed(1) +
      ' C' + c1x.toFixed(1) + ',' + y1.toFixed(1) + ' ' + c2x.toFixed(1) + ',' + y2.toFixed(1) +
      ' ' + x2.toFixed(1) + ',' + y2.toFixed(1) + '" marker-end="url(#wf-arrow)" />';
  }).join('');
}
function wfBookendHtml(pos, isStart) {
  const cls = isStart ? 'wf-bookend wf-start' : 'wf-bookend wf-finish';
  const id = isStart ? WF_START_ID : WF_FINISH_ID;
  const title = isStart ? 'Start' : 'Finish';
  const glyph = isStart
    ? '<span class="wf-start-play" aria-hidden="true"></span>'
    : '<span class="wf-finish-flag" aria-hidden="true"></span>';
  return '<div class="' + cls + '" id="wf-node-' + id + '" style="left:' + pos.x.toFixed(1) +
    'px;top:' + pos.y.toFixed(1) + 'px;width:' + pos.w.toFixed(0) + 'px;min-height:' + WF_BOOKEND_H +
    'px" title="' + title + '" aria-hidden="true">' + glyph + '</div>';
}
function wfLoopProgressText(plan, shown, lifecycle) {
  const iterations = n => n === 1 ? 'iteration' : 'iterations';
  if (plan && plan.exact) {
    if (plan.max_iterations == null) return '↻ more iterations planned';
    const remaining = Math.max(0, plan.max_iterations - shown);
    return remaining === 0
      ? '✓ all ' + plan.max_iterations + ' ' + iterations(plan.max_iterations) + ' shown'
      : '↻ ' + remaining + ' more ' + iterations(remaining) + ' planned';
  }
  if (plan && plan.max_iterations != null) {
    if (shown >= plan.max_iterations) return '! safety limit reached after ' + shown + ' ' + iterations(shown);
    const remaining = plan.max_iterations - shown;
    if (lifecycle === 'completed') return '✓ stopped here · up to ' + remaining + ' more possible';
    return lifecycle === 'running'
      ? '↻ may continue · up to ' + remaining + ' more'
      : '↻ loop permits up to ' + remaining + ' more';
  }
  if (lifecycle === 'completed') return '✓ stopped after ' + shown + ' ' + iterations(shown);
  return lifecycle === 'running' ? '↻ more iterations may follow' : '↻ loop permits more iterations';
}
function wfLoopIslandsHtml(session, layout) {
  const groups = Object.create(null);
  const plans = session.workflow_loops || [];
  const lifecycle = sessionLifecycle(session, Date.now() / 1000).class;
  layout.forEach(pos => {
    const match = pos.id.match(/^([A-Za-z0-9_]+)-(repeat|while-([A-Za-z0-9_]+))-(\d+)$/);
    if (!match) return;
    // do-while islands group by the loop (its final step); repeat islands by the one step.
    const key = match[3] ? 'while-' + match[3] : 'repeat-' + match[1];
    (groups[key] || (groups[key] = [])).push(pos);
  });
  return Object.entries(groups).map(([key, items]) => {
    // Name a do-while island for its whole body — "first → final" — via the lowest-order
    // member; a repeat (or single-step) island keeps the single step name.
    let label, loopId;
    if (key.indexOf('while-') === 0) {
      const end = key.slice('while-'.length);
      loopId = end;
      const first = items.slice().sort((a, b) => a.order - b.order)[0];
      const firstBase = (first && (first.id.match(/^([A-Za-z0-9_]+)-while-/) || [])[1]) || end;
      label = 'do-while · ' + (firstBase !== end ? firstBase + ' → ' + end : end);
    } else {
      loopId = key.slice('repeat-'.length);
      label = 'repeat · ' + loopId;
    }
    const shown = Math.max(...items.map(item => Number((item.id.match(/-(\d+)$/) || [])[1]) || 1));
    const progress = wfLoopProgressText(plans.find(plan => plan.id === loopId), shown, lifecycle);
    const pad = 14, bottomPad = 38, labelH = 22;
    const left = Math.min(...items.map(p => p.x)) - pad;
    const top = Math.min(...items.map(p => p.y)) - pad - labelH;
    const right = Math.max(...items.map(p => p.x + (p.w || WF_NODE_W))) + pad;
    const bottom = Math.max(...items.map(p => p.y + (p.h || WF_NODE_H))) + bottomPad;
    return '<div class="wf-loop-island" data-loop-id="' + esc(loopId) + '" data-loop-shown="' + shown +
      '" style="left:' + left.toFixed(1) + 'px;top:' + top.toFixed(1) +
      'px;width:' + (right - left).toFixed(1) + 'px;height:' + (bottom - top).toFixed(1) +
      'px"><span class="wf-loop-title">' + esc(label) + '</span><span class="wf-loop-progress">' +
      esc(progress) + '</span></div>';
  }).join('');
}
function annotationForProc(session, proc) {
  if (!session || !proc || !proc.cast_path || !liveSessions) return null;
  const sourceName = String(proc.cast_path).split('/').pop();
  for (const [jobId, candidate] of Object.entries(liveSessions)) {
    if (!candidate || (jobId !== session.id && candidate.parent_session !== session.id)) continue;
    for (const ann of (candidate.procs || [])) {
      if (ann.kind !== 'annotate' || !ann.annotate_target) continue;
      const targetName = String(ann.annotate_target).split('/').pop();
      if (ann.annotate_target !== proc.cast_path && targetName !== sourceName) continue;
      const jobIsLive = sessionLifecycle(candidate, Date.now() / 1000).class === 'running';
      const status = (ann.status === 'running' || ann.status === 'waiting') && jobIsLive ? 'running'
        : (ann.status === 'ok' ? 'ok' : 'fail');
      return { jobId, proc: ann.index, status };
    }
  }
  return null;
}
function wfAnnotationHtml(session, proc) {
  const ann = annotationForProc(session, proc);
  if (!ann) return '';
  const text = ann.status === 'running' ? '🖊 annotating' : (ann.status === 'ok' ? '✓ annotation complete' : '✗ annotation failed');
  const dots = ann.status === 'running' ? '<span class="annotation-dots" aria-hidden="true"></span>' : '';
  return '<span class="wf-annotation wf-annotation--' + ann.status + '">' + text + dots + '</span>';
}
function wfBuildGraphHtml(session, nowUnix) {
  const nodes = (session.workflow && session.workflow.nodes) || [];
  if (!nodes.length) return '';
  const { layout, start, finish } = wfLayoutWithBookends(session, nodes, nowUnix);
  const all = layout.concat([start, finish]);
  const w = Math.max(...all.map(n => n.x + n.w)) + WF_PAD;
  const h = Math.max(...all.map(n => n.y + (n.h || WF_NODE_H))) + WF_PAD;
  const present = Object.create(null);
  const counts = { done: 0, graceful: 0, running: 0, waiting: 0, ready: 0, failed: 0, force_stopped: 0, stalled: 0, skipped: 0 };
  const byId = Object.fromEntries(layout.map(n => [n.id, n]));
  const nodesHtml = wfBookendHtml(start, true) + nodes.map(node => {
    const pos = byId[node.id];
    if (!pos) return '';
    const state = wfDisplayState(session, node, nowUnix);
    present[state] = true;
    if (state === 'done') counts.done++;
    else if (state === 'graceful') counts.graceful++;
    else if (state === 'running') counts.running++;
    else if (state === 'waiting') counts.waiting++;
    else if (state === 'ready') counts.ready++;
    else if (state === 'failed') counts.failed++;
    else if (state === 'force-stopped') counts.force_stopped++;
    else if (state === 'stalled') counts.stalled++;
    else if (state === 'skipped') counts.skipped++;
    const p = (session.procs || []).find(x => x.index === node.proc_index);
    const isBuild = node.id === 'build_base' || node.id.indexOf('build_') === 0;
    const title = wfNodeTitle(node.id);
    const unmetIds = wfUnmetNeedIds(session, node);
    const tip = wfNodeTip(session, node, state, unmetIds, nowUnix);
    const bits = [];
    if (isBuild) bits.push('image build');
    else if (p && p.harness) bits.push(p.harness);
    if (p && p.model) bits.push(p.model);
    if (state === 'waiting' && unmetIds.length === 1) bits.push('waiting on ' + wfNodeTitle(unmetIds[0]));
    else if (state === 'waiting' && unmetIds.length > 1 && unmetIds.length <= 3) {
      bits.push('waiting on ' + unmetIds.map(wfNodeTitle).join(', '));
    } else if (state === 'waiting' && unmetIds.length > 3) bits.push('waiting on ' + unmetIds.length + ' tasks');
    if (state === 'ready') bits.push('ready');
    const gate = node.conditional
      ? '<span class="wf-gate" data-tip="Runs only when its gate passes" aria-label="Runs only when its gate passes">when</span>'
      : '';
    const procAttr = node.proc_index != null ? ' data-proc-index="' + esc(String(node.proc_index)) + '"' : '';
    const tipRunning = (state === 'running' && p && p.started_at)
      ? ' data-tip-running="' + esc(String(p.started_at)) + '"' : '';
    return '<a class="wf-node wf-' + state + (isBuild ? ' wf-build' : '') +
      '" href="' + '#task-' + encodeURIComponent(node.id) + '" id="wf-node-' + esc(node.id) +
      '" data-workflow-step="' + esc(node.id) + '" data-wf-state="' + state + '"' + procAttr +
      ' style="left:' + pos.x.toFixed(1) + 'px;top:' + pos.y.toFixed(1) + 'px;width:' + (pos.w || WF_NODE_W) +
      'px;min-height:' + WF_NODE_H + 'px" data-tip="' + esc(tip) + '"' + tipRunning +
      ' aria-label="' + esc(tip.replace(/\n/g, ', ')) +
      '"><span class="wf-state"><span class="wf-ico" aria-hidden="true">' + wfStateIcon(state) +
      '</span><span class="wf-state-label">' + wfStateLabel(state) + '</span></span><span class="wf-id">' +
      esc(title) + gate + '</span>' + wfAnnotationHtml(session, p) +
      '<span class="wf-meta dim">' + esc(bits.join(' · ')) + '</span></a>';
  }).join('') + wfBookendHtml(finish, false);
  return '<div class="card card--accent-left-orange workflow-card" id="workflow-graph" data-workflow-graph>' +
    '<div class="workflow-head"><h2 class="workflow-title">Job graph</h2>' +
    wfJobOutcomeHtml(session, nowUnix) +
    '<p class="workflow-summary dim">' + wfSummaryHtml(counts, nodes.length, wfFirstIdByState(session, nodes, nowUnix)) + '</p>' +
    wfLegendHtml(present) + '<div class="workflow-zoom" aria-label="Graph view controls">' +
    '<button type="button" data-wf-zoom-out aria-label="Zoom out">−</button>' +
    '<button type="button" data-wf-zoom-reset>100%</button>' +
    '<button type="button" data-wf-zoom-in aria-label="Zoom in">+</button>' +
    '<button type="button" data-wf-zoom-fit>Fit</button>' +
    '<button type="button" data-wf-expand aria-label="Open graph in large view" aria-pressed="false">Full screen</button>' +
    '</div></div>' +
    '<div class="workflow-scroll" role="region" aria-label="Job dependency graph" tabindex="0">' +
    '<div class="workflow-stage" style="width:' + w.toFixed(0) + 'px;height:' + h.toFixed(0) + 'px">' +
    wfLoopIslandsHtml(session, layout) +
    '<svg class="workflow-edges" width="' + w.toFixed(0) + '" height="' + h.toFixed(0) +
    '" viewBox="0 0 ' + w.toFixed(1) + ' ' + h.toFixed(1) + '" aria-hidden="true"><defs>' +
    '<marker id="wf-arrow" viewBox="0 0 14 14" refX="12" refY="7" markerWidth="9" markerHeight="9" orient="auto" markerUnits="userSpaceOnUse">' +
    '<path class="wf-arrowhead" d="M3.5 2.5 L11 7 L3.5 11.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>' +
    '</marker></defs>' + wfEdgesSvg(nodes, layout, start, finish) + '</svg>' +
    '<div class="workflow-nodes">' + nodesHtml + '</div></div></div></div>';
}
function ensureWorkflowGraphMounted(session, nowUnix) {
  const nodes = (session && session.workflow && session.workflow.nodes) || [];
  let root = document.querySelector('[data-workflow-graph]');
  if (!nodes.length) {
    if (root) root.remove();
    workflowExpanded = false;
    document.body.classList.remove('wf-modal-open');
    return null;
  }
  const liveIds = nodes.map(n => {
    const st = wfDisplayState(session, n, nowUnix);
    return n.id + '\t' + wfStatusStackRank(st);
  }).slice().sort().join('\0');
  let preserved = null;
  if (root) {
    const domIds = Array.from(root.querySelectorAll('[data-workflow-step]'))
      .map(el => {
        const id = el.getAttribute('data-workflow-step');
        const st = el.getAttribute('data-wf-state') || '';
        return id + '\t' + wfStatusStackRank(st);
      }).filter(Boolean).sort().join('\0');
    if (domIds === liveIds) return root;
    const oldScroller = root.querySelector('.workflow-scroll');
    // The graph card has a fixed viewport, so a remount cannot reflow the page — preserve
    // only the card's own scroll. The page viewport never moves on its own.
    preserved = {
      left: oldScroller ? oldScroller.scrollLeft : 0,
      top: oldScroller ? oldScroller.scrollTop : 0
    };
    root.remove();
    root = null;
  }
  const html = wfBuildGraphHtml(session, nowUnix);
  if (!html) return null;
  const procs = document.getElementById('session-procs');
  if (procs) procs.insertAdjacentHTML('beforebegin', html);
  else {
    const main = document.querySelector('.page-shell') || document.body;
    main.insertAdjacentHTML('beforeend', html);
  }
  root = document.querySelector('[data-workflow-graph]');
  if (root) {
    delete root.dataset.bound;
    initWorkflowGraph();
    if (preserved) {
      const newScroller = root.querySelector('.workflow-scroll');
      if (newScroller) {
        newScroller.scrollLeft = preserved.left;
        newScroller.scrollTop = preserved.top;
      }
    }
  }
  return root;
}
function updateWorkflowGraph(session, nowUnix) {
  const nodes = (session && session.workflow && session.workflow.nodes) || [];
  if (!nodes.length) {
    const gone = document.querySelector('[data-workflow-graph]');
    if (gone) gone.remove();
    workflowExpanded = false;
    document.body.classList.remove('wf-modal-open');
    return;
  }
  const root = ensureWorkflowGraphMounted(session, nowUnix);
  if (!root) return;
  const present = Object.create(null);
  const counts = { done: 0, graceful: 0, running: 0, waiting: 0, ready: 0, failed: 0, force_stopped: 0, stalled: 0, skipped: 0 };
  nodes.forEach(node => {
    const el = root.querySelector('.wf-node[data-workflow-step="' + CSS.escape(node.id) + '"]');
    if (!el) return;
    const state = wfDisplayState(session, node, nowUnix);
    present[state] = true;
    if (state === 'done') counts.done++;
    else if (state === 'graceful') counts.graceful++;
    else if (state === 'running') counts.running++;
    else if (state === 'waiting') counts.waiting++;
    else if (state === 'ready') counts.ready++;
    else if (state === 'failed') counts.failed++;
    else if (state === 'force-stopped') counts.force_stopped++;
    else if (state === 'stalled') counts.stalled++;
    else if (state === 'skipped') counts.skipped++;
    const prev = el.dataset.wfState;
    if (prev !== state) {
      const build = el.classList.contains('wf-build') ? ' wf-build' : '';
      el.className = 'wf-node wf-' + state + build;
      el.dataset.wfState = state;
      const ico = el.querySelector('.wf-ico');
      const lab = el.querySelector('.wf-state-label');
      if (ico) ico.textContent = wfStateIcon(state);
      if (lab) lab.textContent = wfStateLabel(state);
    }
    if (node.proc_index != null) el.setAttribute('data-proc-index', String(node.proc_index));
    const p = (session.procs || []).find(x => x.index === node.proc_index);
    const oldAnnotation = el.querySelector('.wf-annotation');
    if (oldAnnotation) oldAnnotation.remove();
    const annotationHtml = wfAnnotationHtml(session, p);
    if (annotationHtml) {
      const meta = el.querySelector('.wf-meta');
      if (meta) meta.insertAdjacentHTML('beforebegin', annotationHtml);
    }
    const unmetIds = wfUnmetNeedIds(session, node);
    const tip = wfNodeTip(session, node, state, unmetIds, nowUnix);
    el.setAttribute('data-tip', tip);
    el.setAttribute('aria-label', tip.replace(/\n/g, ', '));
    if (state === 'running' && p && p.started_at) el.setAttribute('data-tip-running', String(p.started_at));
    else el.removeAttribute('data-tip-running');
    const meta = el.querySelector('.wf-meta');
    if (meta) {
      const bits = [];
      if (node.id === 'build_base' || node.id.indexOf('build_') === 0) bits.push('image build');
      else if (p && p.harness) bits.push(p.harness);
      if (p && p.model) bits.push(p.model);
      const elapsed = p ? procElapsed(p, nowUnix) : null;
      if (elapsed != null && (state === 'running' || state === 'done' || state === 'graceful' || state === 'failed' ||
          state === 'force-stopped' || state === 'stalled')) {
        bits.push(formatElapsedClock(elapsed));
      }
      if (state === 'waiting' && unmetIds.length === 1) bits.push('waiting on ' + wfNodeTitle(unmetIds[0]));
      else if (state === 'waiting' && unmetIds.length > 1 && unmetIds.length <= 3) {
        bits.push('waiting on ' + unmetIds.map(wfNodeTitle).join(', '));
      } else if (state === 'waiting' && unmetIds.length > 3) bits.push('waiting on ' + unmetIds.length + ' tasks');
      if (state === 'ready') bits.push('ready');
      meta.textContent = bits.join(' · ');
    }
  });
  const loopPlans = session.workflow_loops || [];
  const lifecycle = sessionLifecycle(session, nowUnix).class;
  root.querySelectorAll('.wf-loop-island[data-loop-id]').forEach(island => {
    const id = island.getAttribute('data-loop-id');
    const shown = Number(island.getAttribute('data-loop-shown')) || 1;
    const progress = island.querySelector('.wf-loop-progress');
    if (progress) progress.textContent = wfLoopProgressText(loopPlans.find(plan => plan.id === id), shown, lifecycle);
  });
  const head = root.querySelector('.workflow-head');
  if (head) {
    const outcome = wfJobOutcome(session, nowUnix);
    let outcomeEl = head.querySelector('[data-workflow-outcome]');
    if (!outcomeEl) {
      const title = head.querySelector('.workflow-title');
      if (title) title.insertAdjacentHTML('afterend', wfJobOutcomeHtml(session, nowUnix));
      outcomeEl = head.querySelector('[data-workflow-outcome]');
    }
    if (outcomeEl) {
      outcomeEl.className = 'workflow-outcome workflow-outcome--' + outcome.className;
      outcomeEl.textContent = outcome.text;
    }
    const summary = head.querySelector('.workflow-summary');
    if (summary) summary.innerHTML = wfSummaryHtml(counts, nodes.length, wfFirstIdByState(session, nodes, nowUnix));
    const next = wfLegendHtml(present);
    const cur = head.querySelector('.workflow-legend');
    if (next) {
      if (cur) cur.outerHTML = next;
      else {
        if (summary) summary.insertAdjacentHTML('afterend', next);
        else head.insertAdjacentHTML('beforeend', next);
      }
    } else if (cur) {
      cur.remove();
    }
  }
  // Resolve a pending pre-registration selection exactly once when its panel appears.
  if (pendingWorkflowStep) {
    const det = document.getElementById('task-' + pendingWorkflowStep) ||
      document.querySelector('details.proc[data-workflow-step="' + CSS.escape(pendingWorkflowStep) + '"]');
    if (det) {
      const step = pendingWorkflowStep;
      pendingWorkflowStep = null;
      setWorkflowPendingStatus('');
      // Resolved by a DATA update, not a fresh click — open the panel but never move the
      // viewport: the page scrolls only in direct response to human input.
      activateProcPanel(det, null, false, false);
      markWorkflowNodeSelected(step);
    }
  }
}
function setWorkflowPendingStatus(msg) {
  let el = document.getElementById('wf-pending-status');
  if (!msg) {
    if (el) el.remove();
    return;
  }
  if (!el) {
    el = document.createElement('p');
    el.id = 'wf-pending-status';
    el.className = 'dim';
    el.setAttribute('role', 'status');
    el.setAttribute('aria-live', 'polite');
    const root = document.querySelector('[data-workflow-graph] .workflow-head');
    if (root) root.appendChild(el);
    else return;
  }
  el.textContent = msg;
}
function activateProcPanel(det, hash, pushHistory, scroll) {
  if (!det) return false;
  det.open = true;
  if (scroll !== false) {
    const reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    det.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'nearest' });
  }
  const summary = det.querySelector('summary');
  if (summary) {
    try { summary.focus({ preventScroll: true }); } catch (_) { try { summary.focus(); } catch (_) {} }
  }
  if (hash) {
    const cur = location.hash || '';
    if (pushHistory && cur !== hash) {
      history.pushState({ task: hash }, '', hash);
    } else if (!pushHistory && cur !== hash) {
      history.replaceState({ task: hash }, '', hash);
    }
  }
  persistOpenProcs();
  return true;
}
function activateWorkflowTask(stepId, opts) {
  if (!stepId) return;
  opts = opts || {};
  const pushHistory = opts.pushHistory !== false && !opts.fromHistory;
  const hash = '#task-' + encodeURIComponent(stepId);
  const det = document.getElementById('task-' + stepId) ||
    document.querySelector('details.proc[data-workflow-step="' + CSS.escape(stepId) + '"]');
  if (det) {
    pendingWorkflowStep = null;
    setWorkflowPendingStatus('');
    if (pushHistory && (location.hash || '') === hash) {
      activateProcPanel(det, null, false);
    } else {
      activateProcPanel(det, hash, pushHistory && !opts.fromHistory);
    }
    return;
  }
  // Pre-registration: remember selection; do not silently ignore the click.
  pendingWorkflowStep = stepId;
  setWorkflowPendingStatus('Task details are not available yet; waiting for the task to register.');
  if (pushHistory && !opts.fromHistory) {
    if ((location.hash || '') !== hash) history.pushState({ task: hash }, '', hash);
  } else if ((location.hash || '') !== hash) {
    history.replaceState({ task: hash }, '', hash);
  }
}
function syncWorkflowTaskFromLocation() {
  const m = /^#task-(.+)$/.exec(location.hash || '');
  if (!m) return;
  let step;
  try { step = decodeURIComponent(m[1]); } catch (_) { return; }
  activateWorkflowTask(step, { fromHistory: true, pushHistory: false });
}
function initWorkflowGraph() {
  const root = document.querySelector('[data-workflow-graph]');
  if (!root || root.dataset.bound) return;
  root.dataset.bound = '1';
  const scroller = root.querySelector('.workflow-scroll');
  if (scroller && !scroller.getAttribute('aria-label')) {
    scroller.setAttribute('role', 'region');
    scroller.setAttribute('aria-label', 'Job dependency graph');
    scroller.setAttribute('tabindex', '0');
  }
  const stage = root.querySelector('.workflow-stage');
  const reset = root.querySelector('[data-wf-zoom-reset]');
  const expand = root.querySelector('[data-wf-expand]');
  const applyExpanded = (expanded, focusButton) => {
    workflowExpanded = expanded;
    root.classList.toggle('wf-expanded', expanded);
    document.body.classList.toggle('wf-modal-open', expanded);
    if (expanded) {
      root.setAttribute('role', 'dialog');
      root.setAttribute('aria-modal', 'true');
      root.setAttribute('aria-label', 'Job graph, large view');
    } else {
      root.removeAttribute('role');
      root.removeAttribute('aria-modal');
      root.removeAttribute('aria-label');
    }
    if (expand) {
      expand.textContent = expanded ? 'Close' : 'Full screen';
      expand.setAttribute('aria-label', expanded ? 'Close large graph view' : 'Open graph in large view');
      expand.setAttribute('aria-pressed', expanded ? 'true' : 'false');
      if (focusButton) expand.focus({ preventScroll: true });
    }
  };
  const applyZoom = next => {
    workflowZoom = Math.max(0.5, Math.min(2, next));
    // Zoom scales the stage INSIDE the fixed viewport; the card itself never changes size,
    // so zooming (like graph growth) can never reflow the page around it.
    if (stage) stage.style.zoom = String(workflowZoom);
    if (reset) reset.textContent = Math.round(workflowZoom * 100) + '%';
  };
  const fit = () => {
    if (!stage || !scroller) return;
    const naturalWidth = parseFloat(stage.style.width) || stage.scrollWidth;
    applyZoom((scroller.clientWidth - 16) / naturalWidth);
    scroller.scrollLeft = 0;
  };
  applyZoom(workflowZoom);
  root.querySelector('[data-wf-zoom-out]')?.addEventListener('click', () => applyZoom(workflowZoom - 0.1));
  root.querySelector('[data-wf-zoom-in]')?.addEventListener('click', () => applyZoom(workflowZoom + 0.1));
  reset?.addEventListener('click', () => applyZoom(1));
  root.querySelector('[data-wf-zoom-fit]')?.addEventListener('click', fit);
  expand?.addEventListener('click', () => applyExpanded(!workflowExpanded, false));
  applyExpanded(workflowExpanded, false);
  if (!window.__scshWfExpandBound) {
    window.__scshWfExpandBound = true;
    document.addEventListener('keydown', ev => {
      if (!workflowExpanded) return;
      const current = document.querySelector('[data-workflow-graph]');
      if (ev.key === 'Tab' && current) {
        const focusable = Array.from(current.querySelectorAll('a[href],button,[tabindex]:not([tabindex="-1"])'))
          .filter(el => !el.disabled && el.getClientRects().length > 0);
        if (focusable.length) {
          const first = focusable[0], last = focusable[focusable.length - 1];
          if (ev.shiftKey && document.activeElement === first) {
            ev.preventDefault();
            last.focus();
          } else if (!ev.shiftKey && document.activeElement === last) {
            ev.preventDefault();
            first.focus();
          }
        }
        return;
      }
      if (ev.key !== 'Escape') return;
      const button = current && current.querySelector('[data-wf-expand]');
      workflowExpanded = false;
      document.body.classList.remove('wf-modal-open');
      if (current) {
        current.classList.remove('wf-expanded');
        current.removeAttribute('role');
        current.removeAttribute('aria-modal');
        current.removeAttribute('aria-label');
      }
      if (button) {
        button.textContent = 'Full screen';
        button.setAttribute('aria-label', 'Open graph in large view');
        button.setAttribute('aria-pressed', 'false');
        button.focus({ preventScroll: true });
      }
    });
  }
  scroller?.addEventListener('wheel', ev => {
    ev.preventDefault();
    ev.stopPropagation();
    if (ev.ctrlKey || ev.metaKey) {
      applyZoom(workflowZoom + (ev.deltaY < 0 ? 0.1 : -0.1));
      return;
    }
    scroller.scrollLeft += ev.deltaX;
    scroller.scrollTop += ev.deltaY;
  }, { passive: false });
  root.addEventListener('click', (ev) => {
    const jump = ev.target.closest('a.wf-jump');
    if (jump && root.contains(jump)) {
      const href = jump.getAttribute('href') || '';
      const m = /^#task-(.+)$/.exec(href);
      if (!m) return;
      ev.preventDefault();
      let step;
      try { step = decodeURIComponent(m[1]); } catch (_) { return; }
      if (workflowExpanded) applyExpanded(false, false);
      activateWorkflowTask(step, { pushHistory: true });
      return;
    }
    const a = ev.target.closest('a.wf-node');
    if (!a || !root.contains(a)) return;
    const step = a.getAttribute('data-workflow-step');
    if (!step) return;
    ev.preventDefault();
    if (workflowExpanded) applyExpanded(false, false);
    activateWorkflowTask(step, { pushHistory: true });
  });
  if (!window.__scshWfHistoryBound) {
    window.__scshWfHistoryBound = true;
    window.addEventListener('popstate', () => {
      if (wfHistorySilent) return;
      syncWorkflowTaskFromLocation();
    });
    window.addEventListener('hashchange', () => {
      if (wfHistorySilent) return;
      syncWorkflowTaskFromLocation();
    });
  }
  // Initial fragment: replaceState semantics (no extra history entry).
  if (/^#task-/.test(location.hash || '')) {
    setTimeout(() => syncWorkflowTaskFromLocation(), 0);
  }
}
function persistOpenProcs() {
  if (typeof SESSION_ID !== 'string' || !SESSION_ID) return;
  const open = [];
  document.querySelectorAll('details.proc[data-index]').forEach((det) => {
    if (det.open) open.push(det.dataset.index);
  });
  saveUiPrefs({ openProcs: open });
}
function restoreOpenProcs() {
  if (typeof SESSION_ID !== 'string' || !SESSION_ID) return;
  const open = loadUiPrefs().openProcs;
  if (!Array.isArray(open)) return;
  open.forEach((idx) => {
    const det = document.querySelector('details.proc[data-index="' + CSS.escape(String(idx)) + '"]');
    if (det) det.open = true;
  });
}
function bindSessionProcs(root) {
  if (root.dataset.changeBound) return;
  root.dataset.changeBound = '1';
  root.addEventListener('toggle', (ev) => {
    if (ev.target && ev.target.matches && ev.target.matches('details.proc')) persistOpenProcs();
  }, true);
}
function setupOutputScroll(out) {
  if (!out || out.dataset.scrollBound) return;
  out.dataset.scrollBound = '1';
  out._scshFollow = true;
  const markUserScroll = () => { out._scshUserScroll = true; };
  out.addEventListener('wheel', markUserScroll, { passive: true });
  out.addEventListener('touchmove', markUserScroll, { passive: true });
  out.addEventListener('keydown', markUserScroll);
  out.addEventListener('mousedown', markUserScroll);
  out.addEventListener('scroll', () => {
    if (out._scshAutoScroll) return;
    if (!out._scshUserScroll) return;
    out._scshUserScroll = false;
    // Scroll up → pause follow; return to the bottom → resume (no checkbox).
    out._scshFollow = isAtBottom(out);
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
      initProcDiffs(det);
      setupOutputScroll(det.querySelector('.output'));
      if (procIsLive(p.status)) scrollOutputToBottom(det.querySelector('.output'));
    } else {
      det.open = userOpen;
      updateProcFields(det, p, nowUnix);
      syncProcOutput(det, p);
      const step = workflowStepIdForProc(p);
      if (step && !det.id) {
        det.id = 'task-' + step;
        det.setAttribute('data-workflow-step', step);
      }
    }
  });
  initCasts(root);
  updateWorkflowGraph(session, nowUnix);
}
function onWsMessage(msg) {
  if (msg.type === 'cast_growth') { onCastGrowth(msg); return; }
  onTick(msg);
}
let lastTickSecs = 0;
function onTick(msg) {
  if (msg.type !== 'tick') return;
  // Render time must be monotonic: a stale frame (reconnect backlog, a superseded socket,
  // a throttled tab flushing its queue) carries an old now_secs AND an old snapshot, and
  // rendering it verbatim snaps every live duration backwards, then forwards on the next
  // fresh frame — the "oscillating Duration" bug. Newer snapshots fully supersede older
  // ones, so dropping stale frames loses nothing.
  if ((msg.now_secs || 0) < lastTickSecs) return;
  lastTickSecs = msg.now_secs || lastTickSecs;
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
    const session = liveSessions ? liveSessions[SESSION_ID] : null;
    if (session) {
      renderSessionMeta(session, nowUnix);
      renderSession(session, nowUnix);
      syncSessionStopButton(session);
    }
  } else {
    const snapshot = msg.sessions ?? liveSessions;
    if (snapshot) {
      renderIndex(snapshot, nowUnix);
      renderRepoJobs(snapshot, nowUnix);
      renderInternalJobs(snapshot, nowUnix);
    }
  }
}
let ws;
let reconnectMs = 400;
function connectWs() {
  // Retire any superseded socket completely — two live sockets would interleave a fresh
  // stream with a lagging one and make every duration on the page see-saw.
  if (ws) { try { ws.onclose = null; ws.onmessage = null; ws.onerror = null; ws.close(); } catch (_) {} }
  setDaemonStatus('connecting', 'connecting…', null);
  ws = new WebSocket('ws://127.0.0.1:' + WS_PORT + '/ws');
  ws.onopen = () => { reconnectMs = 400; setDaemonStatus('connecting', 'connecting…', null); };
  ws.onmessage = (ev) => { try { onWsMessage(JSON.parse(ev.data)); } catch (_) {} };
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
  root.querySelectorAll('.output').forEach(out => {
    setupOutputScroll(out);
    if (followOutput(out)) scrollOutputToBottom(out);
  });
  initCasts(root);
  initSessionStop();
  initProcKills(root);
  initProcDiffs(root);
  initHarnessStops();
  initFleetJumps();
  initWorkflowGraph();
  restoreOpenProcs();
})();
function initFleetJumps() {
  document.querySelectorAll('.fleet-jump').forEach((btn) => {
    btn.addEventListener('click', (ev) => {
      ev.preventDefault();
      const idx = btn.getAttribute('data-proc');
      if (idx == null) return;
      const det = document.querySelector('details.proc[data-index="' + CSS.escape(idx) + '"]');
      const step = det && det.getAttribute('data-workflow-step');
      activateProcPanel(det, step ? '#task-' + step : null);
    });
  });
}
function syncSessionStopButton(session) {
  const lifecycle = sessionLifecycle(session, Date.now() / 1000);
  const running = lifecycle.class === 'running';
  let btn = document.getElementById('session-stop');
  // Force stop only while running — remove it when the job settles (no grayed stub).
  if (!running) {
    if (btn) btn.remove();
  } else if (!btn) {
    const actions = document.querySelector('.session-actions');
    if (actions) {
      btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'chamfer btn btn--red btn--sm';
      btn.id = 'session-stop';
      btn.setAttribute('data-session', session.id || SESSION_ID);
      btn.title = 'Force-stop this job? Running containers will be killed.';
      btn.innerHTML = '<span>Force stop</span>';
      actions.appendChild(btn);
      btn.addEventListener('click', () => forceStopSession(btn));
    }
  } else {
    btn.disabled = false;
    btn.title = 'Force-stop this job? Running containers will be killed.';
    setBtnLabel(btn, 'Force stop');
  }
  syncChaptersPending(session);
  const pending = chaptersPendingCount(session);
  const exportBtn = document.querySelector('a.session-export span') || document.querySelector('a.session-export');
  if (exportBtn) {
    let label = 'Job snapshot ⬇';
    if (lifecycle.class === 'running') label = 'Incomplete job ⬇';
    else if (pending > 0) label = 'Chapters pending ⬇';
    if (exportBtn.tagName === 'SPAN') exportBtn.textContent = label;
    else setBtnLabel(exportBtn, label);
  }
  const actions = document.querySelector('.session-actions');
  const hasDiff = !!(session.procs || []).find(p => p.diff_path);
  let diff = document.querySelector('a[data-job-diff]');
  if (hasDiff && !diff && actions) {
    actions.insertAdjacentHTML('afterbegin', '<a class="chamfer btn btn--purple btn--sm job-diff" data-job-diff href="/diff/' + encodeURIComponent(session.id || SESSION_ID) + '/all" title="Browse the entire end-to-end commits diff"><span>⇄ all commits</span></a>');
  } else if (!hasDiff && diff) {
    diff.remove();
  }
}
function chaptersPendingCount(session) {
  let annotateLive = 0;
  if (session && session.procs) {
    for (const p of session.procs) {
      if (p.kind === 'annotate' && (p.status === 'running' || p.status === 'waiting')) annotateLive++;
    }
  }
  const domPending = document.querySelectorAll('.chap-pending').length;
  if (annotateLive > 0 || domPending > 0) return Math.max(annotateLive, domPending);
  const el = document.getElementById('chapters-pending');
  const ssr = el ? (parseInt(el.getAttribute('data-pending') || '0', 10) || 0) : 0;
  if (!ssr) return 0;
  // Cast players loaded and none still summarizing → chapters settled (or never will).
  if (document.querySelectorAll('.cast[data-ready]').length > 0) return 0;
  return ssr;
}
function syncChaptersPending(session) {
  const n = chaptersPendingCount(session);
  let el = document.getElementById('chapters-pending');
  const card = document.querySelector('.card.card--accent-left-purple');
  if (n > 0) {
    const text = n + ' cast' + (n === 1 ? '' : 's') + ' finalizing chapters';
    if (!el && card) {
      el = document.createElement('p');
      el.className = 'chapters-pending dim';
      el.id = 'chapters-pending';
      card.appendChild(el);
    }
    if (el) {
      el.setAttribute('data-pending', String(n));
      el.textContent = text;
    }
  } else if (el) {
    el.remove();
  }
}
async function forceStopSession(btn) {
  const id = btn.getAttribute('data-session') || SESSION_ID;
  if (!id) return;
  const ok = await scshConfirm({
    title: 'Force stop this job?',
    body: 'Running containers will be killed.',
    confirmLabel: 'Force stop',
    danger: true,
  });
  if (!ok) return;
  btn.disabled = true;
  setBtnLabel(btn, 'Stopping…');
  try {
    const resp = await fetch('/api/v1/session/stop', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ session: id }),
    });
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok || data.ok === false) {
      btn.disabled = false;
      setBtnLabel(btn, 'Force stop');
      showToast(data.error || ('stop failed (HTTP ' + resp.status + ')'));
      return;
    }
    setBtnLabel(btn, data.already_ended ? 'Already ended' : 'Stopped');
    btn.remove();
  } catch (e) {
    btn.disabled = false;
    setBtnLabel(btn, 'Force stop');
    showToast(String(e));
  }
}
function initSessionStop() {
  const btn = document.getElementById('session-stop');
  if (!btn) return;
  btn.addEventListener('click', () => forceStopSession(btn));
}
// ---- per-proc kill (session page) ----
async function killProc(btn) {
  const session = btn.getAttribute('data-session');
  const proc = parseInt(btn.getAttribute('data-proc-stop'), 10);
  const annotate = btn.getAttribute('data-proc-kind') === 'annotate';
  if (!session || Number.isNaN(proc)) return;
  const ok = await scshConfirm({
    title: annotate ? 'Stop this annotation?' : 'Force stop this container?',
    body: annotate ? 'The recording remains unchanged and no annotation will be added.' :
      'Only this run stops; the rest of the job continues.',
    confirmLabel: annotate ? 'Stop annotation' : 'Force stop',
    danger: true,
  });
  if (!ok) return;
  btn.disabled = true;
  setBtnLabel(btn, 'Stopping…');
  try {
    const resp = await fetch('/api/v1/proc/stop', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ session: session, proc: proc }),
    });
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok || data.ok !== true) {
      btn.disabled = false;
      setBtnLabel(btn, annotate ? 'Stop annotation' : 'Force stop');
      showToast(data.error || ('stop failed (HTTP ' + resp.status + ')'));
      return;
    }
    btn.remove();
  } catch (e) {
    btn.disabled = false;
    setBtnLabel(btn, annotate ? 'Stop annotation' : 'Force stop');
    showToast(String(e));
  }
}
// ---- stop-all-of-a-harness (index page) ----
async function stopHarness(btn) {
  const harness = btn.getAttribute('data-harness-stop');
  if (!harness) return;
  const ok = await scshConfirm({
    title: 'Stop all ' + harness + ' containers?',
    body: 'Every running ' + harness + ' container across every job will be force-stopped.',
    confirmLabel: 'Stop all ' + harness,
    danger: true,
  });
  if (!ok) return;
  btn.disabled = true;
  setBtnLabel(btn, 'stopping\u2026');
  try {
    const resp = await fetch('/api/v1/harness/stop', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ harness: harness }),
    });
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok || data.ok !== true) {
      btn.disabled = false;
      setBtnLabel(btn, '\u2715 stop all ' + harness);
      showToast(data.error || ('stop failed (HTTP ' + resp.status + ')'));
      return;
    }
    setBtnLabel(btn, 'stopped ' + (data.stopped || 0));
  } catch (e) {
    btn.disabled = false;
    setBtnLabel(btn, '\u2715 stop all ' + harness);
    showToast(String(e));
  }
}
function initHarnessStops() {
  document.querySelectorAll('button[data-harness-stop]').forEach((btn) => {
    btn.addEventListener('click', () => stopHarness(btn));
  });
}
function initProcKills(root) {
  (root || document).querySelectorAll('button[data-proc-stop]').forEach((btn) => {
    btn.addEventListener('click', (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      killProc(btn);
    });
  });
}
// ---- Setup tab (index page only) ----
function imageStatusBadge(img) {
  if (!img.exists) return '<span class="chamfer session-status failed"><span>missing</span></span>';
  if (!img.up_to_date) return '<span class="chamfer session-status cancelled"><span>stale</span></span>';
  return '<span class="chamfer session-status completed"><span>up to date</span></span>';
}
function imageCheckingBadge() {
  return '<span class="chamfer session-status checking"><span>checking…</span></span>';
}
function setupOverallBadge(overall, label) {
  const text = label || overall || 'unknown';
  let cls = 'cancelled';
  if (overall === 'needs_build') cls = 'failed';
  else if (overall === 'needs_login') cls = 'cancelled';
  else if (overall === 'not_tested') cls = 'setup-ready';
  else if (overall === 'ready') cls = 'completed';
  return '<span class="chamfer session-status ' + cls + '"><span>' + esc(text) + '</span></span>';
}
function setupModelStatusHtml(status) {
  if (!status || status === 'not_tested') return '';
  const labels = {
    passed: 'passed', failed: 'failed', testing: 'testing', queued: 'queued',
    unavailable: 'unavailable', blocked: 'blocked', cancelled: 'cancelled',
  };
  const word = labels[status] || status;
  return ' <span class="setup-model-status setup-model-status--' + esc(status) + '">' + esc(word) + '</span>';
}
function setupImageLayer(img) {
  if (!img) return '<span class="dim">—</span>';
  const word = img.status || (img.exists ? (img.up_to_date ? 'ready' : 'stale') : 'missing');
  let cls = 'dim';
  if (word === 'ready') cls = 'setup-ok';
  else if (word === 'missing' || word === 'stale') cls = 'setup-warn';
  return '<span class="' + cls + '">' + esc(word.charAt(0).toUpperCase() + word.slice(1)) + '</span>' +
    (img.tag ? ' <code class="setup-tag">' + esc(img.tag) + '</code>' : '');
}
function setupLoginLayer(login) {
  if (!login) return '<span class="dim">—</span>';
  let cls = 'dim';
  if (login.status === 'found') cls = 'setup-ok';
  else if (login.status === 'missing' || login.status === 'expired' || login.status === 'disabled') cls = 'setup-warn';
  const tip = login.hint ? ' data-tip="' + esc(login.hint) + '"' : '';
  return '<span class="' + cls + '"' + tip + '>' + esc(login.label || login.status) + '</span>';
}
function setupModelsHtml(h) {
  const harness = h.id;
  const custom = (loadUiPrefs().setupCustomModels || {})[harness] || [];
  const selected = (loadUiPrefs().setupSelectedModels || {})[harness];
  const builtin = (h.models || []).map(m => ({
    id: m.id,
    kind: m.kind || 'builtin',
    primary: !!(m.primary_smoke || m.kind === 'primary'),
    status: m.status || 'not_tested',
  }));
  const seen = new Set(builtin.map(m => m.id));
  custom.forEach(id => {
    if (!seen.has(id)) {
      builtin.push({ id, kind: 'custom', primary: false, status: 'not_tested' });
      seen.add(id);
    }
  });
  if (!builtin.length) return '<ul class="setup-models dim"><li>No curated models</li></ul>';
  const rows = builtin.map(m => {
    const checked = selected
      ? selected.indexOf(m.id) >= 0
      : m.primary;
    const kind = m.kind === 'custom' ? ' · custom' : (m.primary ? ' · primary smoke' : ' · optional');
    const remove = m.kind === 'custom'
      ? ' <button type="button" class="setup-model-remove" data-setup-remove="' + esc(harness) +
        '" data-model="' + esc(m.id) + '" title="Remove custom model">✕</button>'
      : '';
    return '<li><label class="setup-model-row">' +
      '<input type="checkbox" class="setup-model-check" data-harness="' + esc(harness) +
      '" data-model="' + esc(m.id) + '"' + (checked ? ' checked' : '') + '>' +
      '<code>' + esc(m.id) + '</code>' +
      '<span class="dim">' + esc(kind) + '</span>' +
      setupModelStatusHtml(m.status) +
      remove +
      '</label></li>';
  }).join('');
  const hint = (h.overall === 'not_tested' || (h.action && h.action.kind === 'test'))
    ? '<p class="setup-models-hint dim">Check the models above, then click <strong>Test selected</strong> below.</p>'
    : '';
  return '<div class="setup-models-block">' +
    '<span class="setup-layer-label">Models</span>' +
    hint +
    '<ul class="setup-models">' + rows + '</ul>' +
    '<div class="setup-add-model">' +
    '<input class="input setup-add-input" type="text" data-harness="' + esc(harness) +
    '" placeholder="Add model id…" autocomplete="off" spellcheck="false">' +
    '<button type="button" class="chamfer btn btn--purple btn--sm" data-setup-add="' +
    esc(harness) + '"><span>Add model</span></button>' +
    '</div></div>';
}
function setupCardActions(h) {
  const a = h.action || {};
  const bits = [];
  if (a.kind === 'build' || a.kind === 'update') {
    bits.push('<button type="button" class="chamfer btn btn--cyan btn--sm setup-build-btn" data-setup-build="' +
      esc(h.id) + '" data-uptodate="' + (a.kind === 'update' ? '1' : '0') + '"><span>' +
      esc(a.label || 'Build image') + '</span></button>');
  }
  if (a.kind === 'login' && a.hint) {
    bits.push('<p class="setup-next dim">' + esc(a.hint) + '</p>');
  }
  if (a.kind === 'test' || a.kind === 'none' || !a.kind) {
    bits.push('<button type="button" class="chamfer btn btn--green btn--sm" data-setup-test="' +
      esc(h.id) + '" title="Run a real container probe for each checked model"><span>Test selected</span></button>');
    bits.push('<span class="setup-next dim">May incur provider cost</span>');
  }
  return bits.join(' ');
}
function setupCardHtml(h) {
  return '<article class="setup-card" data-harness="' + esc(h.id) + '">' +
    '<header class="setup-card-head">' +
    '<strong class="setup-card-name">' + esc(h.name || h.id) + '</strong>' +
    setupOverallBadge(h.overall, h.overall_label) +
    '</header>' +
    '<div class="setup-card-layers">' +
    '<div><span class="setup-layer-label">Image</span> ' + setupImageLayer(h.image) + '</div>' +
    '<div><span class="setup-layer-label">Login</span> ' + setupLoginLayer(h.login) + '</div>' +
    '</div>' +
    setupModelsHtml(h) +
    '<div class="setup-card-actions">' + setupCardActions(h) + '</div>' +
    '</article>';
}
function markSetupChecking() {
  const cards = document.getElementById('setup-cards');
  if (cards) {
    cards.querySelectorAll('.setup-card').forEach(card => {
      card.dataset.pending = '1';
      const badge = card.querySelector('.setup-card-head .session-status');
      if (badge) badge.outerHTML = imageCheckingBadge();
      card.querySelectorAll('.setup-layer-value').forEach(el => {
        el.textContent = 'checking…';
        el.className = 'setup-layer-value dim';
      });
    });
  }
  const summary = document.getElementById('setup-summary');
  if (summary) summary.textContent = 'checking agents…';
  markImagesChecking();
}
// Keep every known image row visible while the runtime inspect runs (§13: no empty limbo).
function markImagesChecking() {
  const body = document.getElementById('images-body');
  if (!body) return;
  body.querySelectorAll('tr[data-image]').forEach(tr => {
    tr.dataset.pending = '1';
    const status = tr.querySelector('.image-status-cell');
    if (status) status.innerHTML = imageCheckingBadge();
    const created = tr.querySelector('.image-created-cell');
    if (created) { created.textContent = '—'; created.classList.add('dim'); }
    const size = tr.querySelector('.image-size-cell');
    if (size) { size.textContent = '—'; size.classList.add('dim'); }
    const cb = tr.querySelector('.image-select');
    if (cb) cb.disabled = true;
  });
  const note = document.getElementById('images-note');
  if (note) note.textContent = 'checking container runtime…';
  const btn = document.getElementById('images-build-selected');
  if (btn) btn.disabled = true;
}
function imageRowHtml(img) {
  const checkbox = '<input type="checkbox" class="image-select" value="' + esc(img.name) + '">';
  // Per-row build: "Rebuild" (forced) once the image is up to date, "Build" otherwise —
  // the base row included: `base` is a first-class image name, buildable on its own.
  const upToDate = !!(img.exists && img.up_to_date);
  const label = upToDate ? 'Rebuild' : 'Build';
  const title = upToDate ? 'Force-rebuild this image' : 'Build this image';
  const action = '<button type="button" class="image-build-btn" data-image-build="' + esc(img.name) +
    '" data-uptodate="' + (upToDate ? '1' : '0') + '" title="' + title + '">' + label + '</button>';
  return '<tr data-image="' + esc(img.name) + '"><td class="image-select-cell">' + checkbox + '</td>' +
    '<td><code>' + esc(img.tag) + '</code></td>' +
    '<td class="image-status-cell">' + imageStatusBadge(img) + '</td>' +
    '<td class="image-created-cell">' + esc(img.created || '—') + '</td>' +
    '<td class="image-size-cell">' + esc(img.size || '—') + '</td>' +
    '<td class="image-action-cell">' + action + '</td></tr>';
}
function wireImageBuildButtons(body) {
  body.querySelectorAll('button[data-image-build]').forEach(btn => btn.addEventListener('click', () => {
    btn.disabled = true;
    startImageBuildOne(btn.getAttribute('data-image-build'), btn.getAttribute('data-uptodate') === '1');
  }));
}
function wireImageSelectButtons(body) {
  const btn = document.getElementById('images-build-selected');
  if (!btn || !body) return;
  body.querySelectorAll('.image-select').forEach(cb => cb.addEventListener('change', () => {
    btn.disabled = body.querySelectorAll('.image-select:checked').length === 0;
  }));
  btn.disabled = body.querySelectorAll('.image-select:checked').length === 0;
}
function wireSetupBuildButtons(root) {
  (root || document).querySelectorAll('button[data-setup-build]').forEach(btn => {
    btn.addEventListener('click', () => {
      btn.disabled = true;
      startImageBuildOne(btn.getAttribute('data-setup-build'), btn.getAttribute('data-uptodate') === '1');
    });
  });
}
function setupModelIdOk(id) {
  const s = (id || '').trim();
  if (!s || s.length > 128) return false;
  if (/[\x00-\x1f"`'$]/.test(s)) return false;
  return true;
}
function persistSetupSelection(harness, model, checked) {
  const prefs = loadUiPrefs();
  const map = Object.assign({}, prefs.setupSelectedModels || {});
  const cur = new Set(map[harness] || []);
  if (checked) cur.add(model); else cur.delete(model);
  map[harness] = Array.from(cur);
  saveUiPrefs({ setupSelectedModels: map });
}
function addCustomSetupModel(harness, raw) {
  if (!setupModelIdOk(raw)) {
    showToast('Enter a valid model id (no quotes/backticks; max 128 chars)');
    return;
  }
  const id = raw.trim();
  const prefs = loadUiPrefs();
  const map = Object.assign({}, prefs.setupCustomModels || {});
  const list = (map[harness] || []).slice();
  if (list.indexOf(id) < 0) list.push(id);
  map[harness] = list;
  const sel = Object.assign({}, prefs.setupSelectedModels || {});
  const selected = new Set(sel[harness] || []);
  selected.add(id);
  sel[harness] = Array.from(selected);
  saveUiPrefs({ setupCustomModels: map, setupSelectedModels: sel });
  refreshSetup();
}
function removeCustomSetupModel(harness, id) {
  const prefs = loadUiPrefs();
  const map = Object.assign({}, prefs.setupCustomModels || {});
  map[harness] = (map[harness] || []).filter(x => x !== id);
  const sel = Object.assign({}, prefs.setupSelectedModels || {});
  sel[harness] = (sel[harness] || []).filter(x => x !== id);
  saveUiPrefs({ setupCustomModels: map, setupSelectedModels: sel });
  refreshSetup();
}
function collectSetupTests(harnessFilter) {
  const cards = document.getElementById('setup-cards');
  if (!cards) return [];
  const tests = [];
  cards.querySelectorAll('.setup-card').forEach(card => {
    const harness = card.getAttribute('data-harness');
    if (harnessFilter && harness !== harnessFilter) return;
    card.querySelectorAll('.setup-model-check:checked').forEach(cb => {
      tests.push({ harness, model: cb.getAttribute('data-model') });
    });
  });
  return tests;
}
function collectPrimarySetupTests() {
  const cards = document.getElementById('setup-cards');
  if (!cards) return [];
  const tests = [];
  (window.__SETUP_HARNESSES || []).forEach(h => {
    if (h.overall === 'needs_build' || h.overall === 'needs_login') return;
    const primary = (h.models || []).find(m => m.primary_smoke || m.kind === 'primary');
    if (primary) tests.push({ harness: h.id, model: primary.id });
  });
  return tests;
}
function startSetupTests(tests, btn) {
  if (!tests.length) {
    showToast('Select at least one model to test');
    return;
  }
  if (btn) btn.disabled = true;
  const body = { tests };
  if (IMAGES_RUNTIME) body.runtime = IMAGES_RUNTIME;
  fetch('/api/v1/setup/tests', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  }).then(r => r.json().then(j => ({ ok: r.ok, status: r.status, j }))).then(({ ok, j }) => {
    if (btn) btn.disabled = false;
    if (!ok || !j.ok) {
      showToast((j && j.error) || 'setup test failed to start');
      return;
    }
    location.href = '/job/' + encodeURIComponent(j.session);
  }).catch(e => {
    if (btn) btn.disabled = false;
    showToast(String(e));
  });
}
function wireSetupModelControls(root) {
  const scope = root || document;
  scope.querySelectorAll('.setup-model-check').forEach(cb => {
    cb.addEventListener('change', () => {
      persistSetupSelection(cb.getAttribute('data-harness'), cb.getAttribute('data-model'), cb.checked);
    });
  });
  scope.querySelectorAll('button[data-setup-add]').forEach(btn => {
    btn.addEventListener('click', () => {
      const harness = btn.getAttribute('data-setup-add');
      const input = scope.querySelector('.setup-add-input[data-harness="' + harness + '"]');
      addCustomSetupModel(harness, input ? input.value : '');
    });
  });
  scope.querySelectorAll('button[data-setup-remove]').forEach(btn => {
    btn.addEventListener('click', () => {
      removeCustomSetupModel(btn.getAttribute('data-setup-remove'), btn.getAttribute('data-model'));
    });
  });
  scope.querySelectorAll('button[data-setup-test]').forEach(btn => {
    btn.addEventListener('click', () => {
      startSetupTests(collectSetupTests(btn.getAttribute('data-setup-test')), btn);
    });
  });
}
function renderSetupSummary(data) {
  const el = document.getElementById('setup-summary');
  if (!el) return;
  const s = data.summary || {};
  const parts = [];
  if (s.needs_build) parts.push(s.needs_build + ' need' + (s.needs_build === 1 ? 's' : '') + ' build');
  if (s.needs_login) parts.push(s.needs_login + ' need' + (s.needs_login === 1 ? 's' : '') + ' login');
  if (s.not_tested) parts.push(s.not_tested + ' ready to test');
  const agents = s.agents || (data.harnesses || []).length || 0;
  el.textContent = agents + ' agents' + (parts.length ? ' — ' + parts.join(' · ') : '');
  const checked = document.getElementById('setup-checked');
  if (checked && data.checked_at) {
    checked.textContent = 'checked ' + formatDuration(Date.now() / 1000 - data.checked_at) + ' ago';
  }
}
function renderImagesTable(data) {
  const body = document.getElementById('images-body');
  if (!body) return;
  const note = document.getElementById('images-note');
  if (data.error) {
    body.querySelectorAll('tr[data-image]').forEach(tr => {
      delete tr.dataset.pending;
      const status = tr.querySelector('.image-status-cell');
      if (status) status.innerHTML = '<span class="chamfer session-status failed"><span>unavailable</span></span>';
      const cb = tr.querySelector('.image-select');
      if (cb) cb.disabled = true;
    });
    if (note) note.textContent = data.error;
    return;
  }
  body.innerHTML = (data.images || []).map(imageRowHtml).join('');
  if (note) note.textContent = data.runtime ? ('runtime: ' + data.runtime) : '';
  wireImageSelectButtons(body);
  wireImageBuildButtons(body);
}
function renderSetup(data) {
  const cards = document.getElementById('setup-cards');
  if (data.error) {
    if (cards) {
      cards.querySelectorAll('.setup-card').forEach(card => {
        delete card.dataset.pending;
        const badge = card.querySelector('.setup-card-head .session-status');
        if (badge) badge.outerHTML = '<span class="chamfer session-status failed"><span>unavailable</span></span>';
      });
    }
    const summary = document.getElementById('setup-summary');
    if (summary) summary.textContent = data.error;
    renderImagesTable(data);
    return;
  }
  if (cards) {
    window.__SETUP_HARNESSES = data.harnesses || [];
    cards.innerHTML = (data.harnesses || []).map(setupCardHtml).join('');
    wireSetupBuildButtons(cards);
    wireSetupModelControls(cards);
  }
  renderSetupSummary(data);
  renderRuntimeSelector(data);
  renderImagesTable(data);
}
function refreshSetup() {
  markSetupChecking();
  const url = '/api/v1/setup' + (IMAGES_RUNTIME ? '?runtime=' + encodeURIComponent(IMAGES_RUNTIME) : '');
  fetch(url).then(r => r.json()).then(renderSetup).catch(() => {
    renderSetup({ error: 'setup unavailable (daemon error)' });
  });
}
function refreshImages() {
  // Advanced section refresh uses the same Setup payload (includes images[]).
  refreshSetup();
}
let IMAGES_RUNTIME = ''; // '' = the host default; set by the selector in the panel
function renderRuntimeSelector(data) {
  const box = document.getElementById('images-runtimes');
  if (!box) return;
  const available = data.available || [];
  if (available.length < 2) { box.innerHTML = ''; return; }
  // Apple `container` and docker/podman keep SEPARATE image stores — this segmented
  // control says which world the whole tab (cards + Advanced builds) talks to.
  const label = (r) => r === 'container' ? 'Apple Containers' : r === 'docker' ? 'Docker' : r === 'podman' ? 'Podman' : r;
  box.innerHTML = '<span class="seg" data-tip="Each runtime keeps its own image store — cards and Build buttons apply to the selected one">' +
    available.map(r =>
      '<button type="button" class="seg-opt' + (r === data.runtime ? ' active' : '') +
      '" data-runtime="' + esc(r) + '">' + esc(label(r)) + '</button>').join('') +
    '</span>';
  box.querySelectorAll('[data-runtime]').forEach(b => b.addEventListener('click', () => {
    if (b.classList.contains('active')) return;
    IMAGES_RUNTIME = b.dataset.runtime;
    refreshSetup();
  }));
}
function postImagesBuild(req) {
  if (IMAGES_RUNTIME) req.runtime = IMAGES_RUNTIME;
  const note = document.getElementById('images-note');
  if (note) note.textContent = 'starting build…';
  fetch('/api/v1/images/build', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  }).then(r => r.json()).then(resp => {
    if (resp.ok && resp.session) {
      window.location.href = '/job/' + resp.session;
    } else if (note) {
      note.textContent = resp.error || 'build request failed';
    }
  }).catch(() => { if (note) note.textContent = 'build request failed'; });
}
function startImagesBuild(all) {
  const harnesses = all ? [] :
    Array.from(document.querySelectorAll('.image-select:checked')).map(cb => cb.value);
  postImagesBuild({
    harnesses: harnesses,
    rebuild_base: !!document.getElementById('images-rebuild-base')?.checked,
    force: !!document.getElementById('images-force')?.checked,
  });
}
function startImageBuildOne(name, upToDate) {
  // `base` rides the same path: harnesses:["base"] builds ONLY the shared base.
  postImagesBuild({ harnesses: [name], rebuild_base: false, force: upToDate });
}
(function initSetupPanel() {
  if (!document.getElementById('setup-cards') && !document.getElementById('images-body')) return;
  refreshSetup();
  document.getElementById('images-build-selected')?.addEventListener('click', () => startImagesBuild(false));
  document.getElementById('images-build-all')?.addEventListener('click', () => startImagesBuild(true));
  document.getElementById('images-refresh')?.addEventListener('click', (e) => { e.preventDefault(); refreshSetup(); });
  document.getElementById('setup-refresh')?.addEventListener('click', (e) => { e.preventDefault(); refreshSetup(); });
  document.getElementById('setup-test-all')?.addEventListener('click', (e) => {
    e.preventDefault();
    startSetupTests(collectPrimarySetupTests(), e.currentTarget);
  });
})();
// ---- instant tooltips ----
// One floating tip for every [data-tip] element, wired by delegation so it works for
// server-rendered and live-re-rendered markup alike, with none of the native title
// tooltip's hover delay (which live table re-renders kept resetting anyway).
(function initTips() {
  const tip = document.createElement('div');
  tip.className = 'ui-tip';
  tip.hidden = true;
  document.body.appendChild(tip);
  let anchor = null, timer = null;
  const hide = () => {
    anchor = null;
    tip.hidden = true;
    if (timer) { clearInterval(timer); timer = null; }
  };
  // Tips are multi-line (pre-line CSS). A running chip carries data-tip-running (its start
  // time) instead of baking the duration into the markup, so the "running for …" line
  // ticks live here while live re-renders keep comparing the row's HTML as unchanged.
  const render = (el) => {
    let text = el.dataset.tip;
    const started = Number(el.dataset.tipRunning || 0);
    if (started) text += '\nrunning for ' + formatDuration(Date.now() / 1000 - started);
    tip.textContent = text;
    tip.style.left = '0px';
    tip.style.top = '0px';
    const r = el.getBoundingClientRect();
    let x = r.left + r.width / 2 - tip.offsetWidth / 2;
    x = Math.max(6, Math.min(x, window.innerWidth - tip.offsetWidth - 6));
    let y = r.top - tip.offsetHeight - 8;
    if (y < 6) y = r.bottom + 8;
    tip.style.left = x + 'px';
    tip.style.top = y + 'px';
  };
  document.addEventListener('mouseover', (e) => {
    const el = e.target.closest ? e.target.closest('[data-tip]') : null;
    if (el === anchor) return;
    if (timer) { clearInterval(timer); timer = null; }
    if (!el) { hide(); return; }
    anchor = el;
    tip.hidden = false;
    render(el);
    if (el.dataset.tipRunning) {
      timer = setInterval(() => {
        if (!document.contains(el)) { hide(); return; }
        render(el);
      }, 1000);
    }
  });
  document.addEventListener('scroll', hide, true);
})();
// ---- repositories panel (index page only) ----
let OPEN_REPO = null;
let OPEN_REPO_RUNNABLE = false;
const OPEN_REPOS = {};    // path -> { clean }
const DEFS_BY_NAME = {};  // name -> definition
// ---- tabs ----
// Explicit tab clicks push history (/jobs, /projects, /setup, /); Back/Forward restore (WEB-UI §1).
// /project/… and /repo/… are filtered Projects views — keep the path, open the Projects tab.
(function initTabs() {
  const tabs = document.querySelectorAll('.tab');
  if (!tabs.length) return;
  function pathFilter() {
    const p = location.pathname || '/';
    return p === '/project' || p.indexOf('/project/') === 0 || p === '/repo' || p.indexOf('/repo/') === 0;
  }
  function normalizeTab(id) {
    if (id === 'images') return 'setup';
    if (id === 'start') return 'run'; // legacy prefs / #tab=start
    if (id === 'dirs') return 'projects'; // legacy prefs / #tab=dirs
    return id;
  }
  function pathForTab(id) {
    id = normalizeTab(id);
    if (id === 'jobs') return '/jobs';
    if (id === 'projects') return '/projects';
    if (id === 'setup') return '/setup';
    return '/'; // run
  }
  function tabFromLocation() {
    if (pathFilter()) return 'projects';
    const p = (location.pathname || '/').replace(/\/+$/, '') || '/';
    if (p === '/jobs') return 'jobs';
    if (p === '/projects') return 'projects';
    if (p === '/setup' || p === '/images') return 'setup';
    if (p === '/run' || p === '/') return 'run';
    // Legacy bookmarks: /#tab=dirs → projects, etc.
    const m = (location.hash || '').match(/^#tab=([a-z]+)$/);
    if (m) return normalizeTab(m[1]);
    return null;
  }
  function activate(id, mode) {
    id = normalizeTab(id);
    const t = document.querySelector('.tab[data-tab="' + id + '"]');
    if (!t) id = 'run';
    const active = document.querySelector('.tab[data-tab="' + id + '"]') || tabs[0];
    id = active.dataset.tab;
    document.querySelectorAll('.tab').forEach(x => {
      const on = x === active;
      x.classList.toggle('active', on);
      x.setAttribute('aria-selected', on ? 'true' : 'false');
      // Roving tabindex (ARIA tabs pattern): Tab reaches only the active tab; the
      // arrows walk between tabs, so inactive ones leave the page's tab order.
      x.tabIndex = on ? 0 : -1;
    });
    document.querySelectorAll('.tab-panel').forEach(p => p.classList.toggle('active', p.id === 'tab-' + id));
    if (id === 'setup' && typeof refreshSetup === 'function') refreshSetup();
    if (pathFilter()) {
      // Stay on /project/… or /repo/…; only rewrite when leaving the filtered view.
      if (mode === 'push' && id !== 'projects') {
        history.pushState({ tab: id }, '', pathForTab(id));
      }
      return;
    }
    const next = pathForTab(id);
    if (mode === 'push') history.pushState({ tab: id }, '', next);
    else if ((location.pathname || '/') !== next || (location.hash || '').indexOf('#tab=') === 0) {
      history.replaceState({ tab: id }, '', next);
    }
    if (typeof SESSION_ID !== 'string' || !SESSION_ID) saveUiPrefs({ tab: id });
  }
  const tabList = Array.from(tabs);
  tabs.forEach(t => {
    t.setAttribute('role', 'tab');
    t.addEventListener('click', () => activate(t.dataset.tab, 'push'));
    // ArrowLeft/ArrowRight move between tabs (wrapping), and activation follows
    // focus — the standard keyboard contract for an ARIA tablist.
    t.addEventListener('keydown', (e) => {
      if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
      e.preventDefault();
      const step = e.key === 'ArrowRight' ? 1 : tabList.length - 1;
      const next = tabList[(tabList.indexOf(t) + step) % tabList.length];
      activate(next.dataset.tab, 'push');
      next.focus();
    });
  });
  window.addEventListener('popstate', () => {
    activate(tabFromLocation() || 'run', 'sync');
  });
  const fromLoc = tabFromLocation();
  const savedRaw = loadUiPrefs().tab;
  const saved = savedRaw ? normalizeTab(savedRaw) : null;
  activate(fromLoc || saved || 'run', 'sync');
})();
function defSourceBadge(src) {
  // builtin wears purple (the setup color); repo/home keep the muted status hues.
  if (src === 'builtin') return '<span class="chamfer badge badge--purple"><span>builtin</span></span>';
  const cls = src === 'repo' ? 'completed' : 'cancelled';
  return '<span class="chamfer session-status ' + cls + '"><span>' + esc(src) + '</span></span>';
}
function pickRepo() {
  // The daemon is local, so it can pop the native OS folder chooser and hand back the path.
  const note = document.getElementById('repo-note');
  if (note) note.textContent = 'opening the folder picker…';
  fetch('/api/v1/repos/pick', { method: 'POST' }).then(r => r.json()).then(resp => {
    if (resp.ok && resp.path) {
      const input = document.getElementById('repo-path');
      if (input) input.value = resp.path;
      if (note) note.textContent = '';
      openRepo();
    } else if (resp.cancelled) {
      if (note) note.textContent = '';
    } else if (note) {
      note.textContent = resp.error || 'picker unavailable — type or paste the path instead';
    }
  }).catch(() => { if (note) note.textContent = 'picker unavailable — type or paste the path instead'; });
}
function openRepo() {
  const input = document.getElementById('repo-path');
  const note = document.getElementById('repo-note');
  const path = (input?.value || '').trim();
  if (!path) { if (note) note.textContent = 'enter a repository path'; return; }
  if (note) note.textContent = 'opening…';
  fetch('/api/v1/repos/open', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path: path }),
  }).then(r => r.json()).then(resp => {
    handleRepoOpened(resp, note);
  }).catch(() => { if (note) note.textContent = 'could not open'; });
}
// Create a fresh project under ~/.scsh/projects/<name> — a new git repo born runnable — and
// open it in place, so a demo job can start seconds later with no terminal involved.
// Names: letters/digits/-/_ only (no dots or slashes). An existing name copies into Open + toasts.
function projectNameOk(name) {
  return /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(name) && !/[./]/.test(name);
}
function showToast(message) {
  let el = document.getElementById('scsh-toast');
  if (!el) {
    el = document.createElement('div');
    el.id = 'scsh-toast';
    el.className = 'toast';
    el.setAttribute('role', 'status');
    document.body.appendChild(el);
  }
  el.textContent = message;
  el.classList.remove('show');
  // Retrigger the CSS transition when the same message fires twice in a row.
  void el.offsetWidth;
  el.classList.add('show');
  clearTimeout(showToast._timer);
  showToast._timer = setTimeout(() => { el.classList.remove('show'); }, 2800);
}
function createProject() {
  const input = document.getElementById('project-name');
  const note = document.getElementById('repo-note');
  const name = (input?.value || '').trim();
  if (!name) { if (note) note.textContent = 'enter a project name'; return; }
  if (!projectNameOk(name)) {
    showToast('Project names: letters, digits, - or _ only (no dots or slashes).');
    if (note) note.textContent = '';
    return;
  }
  if (note) note.textContent = 'creating…';
  fetch('/api/v1/projects/create', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: name }),
  }).then(async r => {
    const resp = await r.json();
    // Create-or-open: existing names return 200 with created:false (same shape as open).
    handleRepoOpened(resp, note);
    if (resp.ok) {
      const pathInput = document.getElementById('repo-path');
      if (pathInput) pathInput.value = resp.repo;
    }
  }).catch(() => { if (note) note.textContent = 'could not create'; });
}
// Shared tail of open/create: surface blockers, render definitions, remember the repo.
function handleRepoOpened(resp, note) {
  if (!resp.ok) { if (note) note.textContent = resp.error || 'could not open'; return; }
  OPEN_REPO = resp.repo;
  OPEN_REPO_RUNNABLE = !!resp.runnable;
  OPEN_REPOS[resp.repo] = { clean: resp.runnable };
  const panel = document.getElementById('defs-panel');
  if (panel) panel.hidden = false;
  const label = document.getElementById('open-repo-path');
  if (label) label.textContent = resp.repo;
  // Show any blockers prominently; Start stays disabled until they are cleared.
  const bl = document.getElementById('repo-blockers');
  if (bl) {
    const list = resp.blockers || [];
    if (list.length) {
      bl.hidden = false;
      bl.innerHTML = '<strong>Not ready to run:</strong><ul>' +
        list.map(b => '<li>' + esc(b) + '</li>').join('') + '</ul>';
    } else {
      bl.hidden = true;
      bl.innerHTML = '';
    }
  }
  if (note) {
    const verb = resp.created ? 'created' : 'opened';
    note.textContent = resp.runnable ? verb + ' — ready to run' : verb + ', but not ready to run (see below)';
  }
  renderDefs(resp.defs || []);
  const form = document.getElementById('def-form');
  if (form) form.innerHTML = '';
  renderRepoJobs(liveSessions, Date.now() / 1000);
  renderInternalJobs(liveSessions, Date.now() / 1000);
  // After the list is filled, scroll its first actionable area into view so Open / New
  // project invites the next step without pinning the control against the viewport edge.
  // #defs-list owns a blank top inset; explanatory copy above it need not stay visible.
  const list = document.getElementById('defs-list');
  if (list) {
    requestAnimationFrame(() => {
      const reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
      list.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'start' });
    });
  }
}
function renderDefs(defs) {
  const list = document.getElementById('defs-list');
  if (!list) return;
  for (const k in DEFS_BY_NAME) delete DEFS_BY_NAME[k];
  defs.forEach(d => { DEFS_BY_NAME[d.name] = d; });
  if (!defs.length) { list.innerHTML = '<p class="dim">no harness definitions found.</p>'; return; }
  list.innerHTML = defs.map(d => {
    const agents = (d.agents || []).map(a =>
      '<span class="chamfer agent-badge"><span>' + esc(a.agent) +
      (a.model ? ' · ' + esc(a.model) : '') + '</span></span>').join(' ');
    const wf = d.workflow
      ? ' <span class="chamfer session-status completed"><span>workflow · ' + d.steps + ' steps</span></span>'
      : '';
    return '<div class="def-card">' +
      '<button type="button" class="chamfer btn btn--cyan btn--sm def-pick" data-def="' +
      esc(d.name) + '"><span>' + esc(d.name) + '</span></button> ' +
      defSourceBadge(d.source) + wf + ' <span class="dim">' + esc(d.description) + '</span>' +
      '<div class="def-agents">' + agents + '</div></div>';
  }).join('');
  list.querySelectorAll('.def-pick').forEach(b =>
    b.addEventListener('click', () => selectDef(b.dataset.def)));
}
function selectDef(name) {
  const def = DEFS_BY_NAME[name];
  const form = document.getElementById('def-form');
  if (!def || !form) return;
  const fields = (def.params || []).map(p => {
    const id = 'param-' + p.name;
    let input;
    if (p.type === 'bool') {
      input = '<input type="checkbox" id="' + id + '"' + (p.default === 'true' ? ' checked' : '') + '>';
    } else if (p.type === 'enum') {
      input = '<select id="' + id + '">' + (p.choices || []).map(c =>
        '<option' + (c === p.default ? ' selected' : '') + '>' + esc(c) + '</option>').join('') + '</select>';
    } else {
      const t = p.type === 'int' ? 'number' : 'text';
      input = '<input type="' + t + '" id="' + id + '" value="' + esc(p.default || '') + '">';
    }
    return '<div class="param-row"><label for="' + id + '">' + esc(p.name) +
      (p.required ? ' <span class="param-req">*</span>' : '') + '</label> ' + input +
      (p.description ? ' <span class="dim">' + esc(p.description) + '</span>' : '') + '</div>';
  }).join('');
  const disabled = OPEN_REPO_RUNNABLE ? '' : ' disabled';
  const hint = OPEN_REPO_RUNNABLE ? '' : 'the repository is not ready to run (see the blockers above)';
  form.innerHTML = '<h4 class="form-title">run <code>' + esc(name) + '</code></h4>' + fields +
    '<div class="images-controls"><button type="button" class="chamfer btn btn--green btn--sm" id="def-start"' +
    disabled + '><span>Start job</span></button>' +
    '<span id="def-note" class="dim">' + hint + '</span></div>';
  document.getElementById('def-start')?.addEventListener('click', () => startJob(name));
  // Cmd/Ctrl+Enter anywhere in the form is the default button.
  form.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') document.getElementById('def-start')?.click();
  });
  // The form renders below the definitions list — bring it to the user instead of making
  // them hunt for what their click produced.
  const reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  form.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'start' });
}
function collectParams(def) {
  const out = {};
  (def.params || []).forEach(p => {
    const el = document.getElementById('param-' + p.name);
    if (!el) return;
    out[p.name] = p.type === 'bool' ? (el.checked ? 'true' : 'false') : el.value;
  });
  return out;
}
function startJob(name) {
  const def = DEFS_BY_NAME[name];
  const note = document.getElementById('def-note');
  if (!def || !OPEN_REPO) return;
  if (!OPEN_REPO_RUNNABLE) { if (note) note.textContent = 'the repository is not ready to run'; return; }
  const req = { repo: OPEN_REPO, def: name, params: collectParams(def) };
  if (note) note.textContent = 'starting…';
  fetch('/api/v1/jobs/start', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  }).then(r => r.json()).then(resp => {
    if (resp.ok && resp.session) { window.location.href = '/job/' + resp.session; }
    else if (note) note.textContent = resp.error || 'could not start job';
  }).catch(() => { if (note) note.textContent = 'could not start job'; });
}
// Mirrors repo_jobs_rows in index.rs — keep the markup identical. The tbody arrives
// server-rendered, so a null snapshot (no full tick yet) must leave it untouched.
function collapseSlashes(s) {
  return String(s || '').replace(/\/+/g, '/').replace(/(.+)\/$/, '$1') || '/';
}
function parseIndexFilter(pathname) {
  const p = pathname || '/';
  if (p === '/project' || p.indexOf('/project/') === 0) {
    let name = collapseSlashes(decodeURIComponent(p.slice('/project'.length))).replace(/^\/+|\/+$/g, '');
    if (!name || name.indexOf('/') >= 0) return null;
    return { kind: 'project', name: name, repo: (PROJECTS_DIR ? PROJECTS_DIR + '/' + name : null) };
  }
  if (p === '/repo' || p.indexOf('/repo/') === 0) {
    let rest = collapseSlashes(decodeURIComponent(p.slice('/repo'.length)));
    if (!rest || rest === '/') return null;
    if (rest.charAt(0) !== '/') rest = '/' + rest;
    return { kind: 'repo', repo: rest };
  }
  return null;
}
function repoFilterHref(repo) {
  const root = PROJECTS_DIR ? PROJECTS_DIR + '/' : null;
  if (root && repo.indexOf(root) === 0) {
    const name = repo.slice(root.length);
    if (name && name.indexOf('/') < 0) return '/project/' + encodeURIComponent(name);
  }
  // Keep slashes; encode other unsafe bytes (mirrors encode_repo_url_path).
  return '/repo' + String(repo).split('').map(ch => {
    if (/[A-Za-z0-9\-._~/]/.test(ch)) return ch;
    const hex = ch.charCodeAt(0).toString(16).toUpperCase();
    return '%' + (hex.length === 1 ? '0' + hex : hex);
  }).join('');
}
function isInternalRepo(repo) {
  return repo === '(image builds)' || repo === '(internal)';
}
function renderRepoJobs(sessions, nowUnix) {
  const body = document.getElementById('repos-body');
  if (!body || !sessions) return;
  nowUnix = nowUnix ?? (Date.now() / 1000);
  const filter = parseIndexFilter(location.pathname);
  const wantRepo = filter && filter.repo;
  const byRepo = {};
  Object.keys(OPEN_REPOS).forEach(r => {
    if (wantRepo && r !== wantRepo) return;
    byRepo[r] = [];
  });
  Object.keys(sessions).forEach(id => {
    const s = sessions[id];
    if (!s || !s.repo || isInternalRepo(s.repo)) return;
    if (wantRepo && s.repo !== wantRepo) return;
    (byRepo[s.repo] = byRepo[s.repo] || []).push(Object.assign({ id: id }, s));
  });
  const repos = Object.keys(byRepo).sort();
  if (!repos.length) {
    body.innerHTML = wantRepo
      ? '<tr><td colspan="2" class="dim">No jobs for this project or repository.</td></tr>'
      : '<tr><td colspan="2" class="dim">No jobs yet — open or create a project under Run.</td></tr>';
    return;
  }
  const repoLabel = (repo) => {
    const root = PROJECTS_DIR ? PROJECTS_DIR + '/' : null;
    if (root && repo.startsWith(root)) return 'project · ' + repo.slice(root.length);
    return repo;
  };
  // A job's "activity" moment: when it finished, or when it started if still going.
  const activity = (s) => s.ended_at || s.started_at || 0;
  const isRunning = (s) => sessionLifecycle(s, nowUnix).label === 'running';
  body.innerHTML = repos.map(repo => {
    const jobs = byRepo[repo] || [];
    let cells = '<span class="dim">no jobs yet</span>';
    if (jobs.length) {
      const groups = {};
      jobs.forEach(s => { const k = s.profile || 'default'; (groups[k] = groups[k] || []).push(s); });
      const ordered = Object.keys(groups).sort().map(k => {
        const g = groups[k];
        g.sort((a, b) => (isRunning(b) - isRunning(a)) || (activity(b) - activity(a)));
        return [k, g];
      });
      // Groups with something running come first, then by most recent activity.
      ordered.sort((a, b) => {
        const ar = a[1].some(isRunning), br = b[1].some(isRunning);
        if (ar !== br) return br - ar;
        return Math.max(...b[1].map(activity)) - Math.max(...a[1].map(activity));
      });
      cells = ordered.map(([task, g]) => {
        const links = g.map(s => {
          const lc = sessionLifecycle(s, nowUnix);
          return '<div class="repo-job"><span class="chamfer session-status ' + lc.class +
            '"><span>' + esc(lc.label) + '</span></span> <a class="job-id" href="/job/' + esc(s.id) + '">' + esc(s.id) +
            '</a> <span class="dim">' + esc(formatShortAge(nowUnix - activity(s))) + '</span></div>';
        }).join('');
        return '<div class="repo-jobgroup"><span class="repo-jobgroup-name">' + esc(task) + '</span>' + links + '</div>';
      }).join('');
    }
    return '<tr data-repo="' + esc(repo) + '"><td class="repo-path" title="' + esc(repo) +
      '"><a class="repo-filter-link" href="' + esc(repoFilterHref(repo)) + '">' + esc(repoLabel(repo)) +
      '</a></td><td>' + cells + '</td></tr>';
  }).join('');
}
function renderInternalJobs(sessions, nowUnix) {
  const panel = document.getElementById('tab-projects');
  if (!panel || !sessions) return;
  nowUnix = nowUnix ?? (Date.now() / 1000);
  if (parseIndexFilter(location.pathname)) {
    const existing = document.getElementById('internal-jobs-card');
    if (existing) existing.remove();
    return;
  }
  const jobs = [];
  Object.keys(sessions).forEach(id => {
    const s = sessions[id];
    if (s && s.repo && isInternalRepo(s.repo)) jobs.push(Object.assign({ id: id }, s));
  });
  let card = document.getElementById('internal-jobs-card');
  if (!jobs.length) {
    if (card) card.remove();
    return;
  }
  const activity = (s) => s.ended_at || s.started_at || 0;
  const isRunning = (s) => sessionLifecycle(s, nowUnix).label === 'running';
  const groups = {};
  jobs.forEach(s => { const k = s.profile || 'default'; (groups[k] = groups[k] || []).push(s); });
  const ordered = Object.keys(groups).sort().map(k => {
    const g = groups[k];
    g.sort((a, b) => (isRunning(b) - isRunning(a)) || (activity(b) - activity(a)));
    return [k, g];
  });
  ordered.sort((a, b) => {
    const ar = a[1].some(isRunning), br = b[1].some(isRunning);
    if (ar !== br) return br - ar;
    return Math.max(...b[1].map(activity)) - Math.max(...a[1].map(activity));
  });
  const body = ordered.map(([task, g]) => {
    const links = g.map(s => {
      const lc = sessionLifecycle(s, nowUnix);
      return '<div class="repo-job"><span class="chamfer session-status ' + lc.class +
        '"><span>' + esc(lc.label) + '</span></span> <a class="job-id" href="/job/' + esc(s.id) + '">' + esc(s.id) +
        '</a> <span class="dim">' + esc(formatShortAge(nowUnix - activity(s))) + '</span></div>';
    }).join('');
    return '<div class="repo-jobgroup"><span class="repo-jobgroup-name">' + esc(task) + '</span>' + links + '</div>';
  }).join('');
  if (!card) {
    card = document.createElement('div');
    card.className = 'card card--accent-left-purple';
    card.id = 'internal-jobs-card';
    card.innerHTML = '<p class="section-label">Internal</p>' +
      '<p class="dim">System jobs — image builds and annotate catch-up — not tied to a project or repository.</p>' +
      '<div id="internal-body"></div>';
    panel.appendChild(card);
  }
  const bodyEl = card.querySelector('#internal-body') || card;
  if (bodyEl.id === 'internal-body') bodyEl.innerHTML = body;
  else {
    let inner = card.querySelector('#internal-body');
    if (!inner) {
      inner = document.createElement('div');
      inner.id = 'internal-body';
      card.appendChild(inner);
    }
    inner.innerHTML = body;
  }
}
(function initReposPanel() {
  if (!document.getElementById('repo-path')) return;
  document.getElementById('repo-open')?.addEventListener('click', openRepo);
  document.getElementById('project-create')?.addEventListener('click', createProject);
  document.getElementById('project-name')?.addEventListener('keydown', (e) => { if (e.key === 'Enter') createProject(); });
  document.getElementById('repo-pick')?.addEventListener('click', pickRepo);
  document.getElementById('repo-path')?.addEventListener('keydown', (e) => { if (e.key === 'Enter') openRepo(); });
  // Seed the opened-repo set from the daemon so repos opened before this page load keep
  // their "no jobs yet" rows across live re-renders of the Projects table.
  fetch('/api/v1/repos').then(r => r.json()).then(resp => {
    (resp.repos || []).forEach(r => { if (r.path && !(r.path in OPEN_REPOS)) OPEN_REPOS[r.path] = { clean: r.clean }; });
    renderRepoJobs(liveSessions, Date.now() / 1000);
    renderInternalJobs(liveSessions, Date.now() / 1000);
  }).catch(() => {});
})();

// Keep copied job URLs anchored to the section currently being read, like Packdiff's
// scroll-addressable pages. replaceState avoids turning ordinary scrolling into Back-button
// history. The candidate list is rebuilt on each frame because workflow runs add proc rows live.
(function initJobScrollAddress() {
  if (!SESSION_ID) return;
  let queued = false;
  function syncHashToScroll() {
    queued = false;
    const marker = Math.min(window.innerHeight * 0.3, 260);
    const candidates = [];
    const graph = document.getElementById('workflow-graph');
    if (graph) candidates.push({ el: graph, hash: '#workflow-graph' });
    document.querySelectorAll('details.proc[data-index]').forEach(det => {
      const step = det.getAttribute('data-workflow-step');
      const hash = step
        ? '#task-' + encodeURIComponent(step)
        : '#proc-' + encodeURIComponent(det.getAttribute('data-index'));
      candidates.push({ el: det, hash: hash });
    });
    let current = null;
    candidates.forEach(candidate => {
      const rect = candidate.el.getBoundingClientRect();
      if (rect.top <= marker && rect.bottom > 0) current = candidate.hash;
    });
    if (!current || location.hash === current) return;
    history.replaceState(history.state, '', location.pathname + location.search + current);
  }
  window.addEventListener('scroll', () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(syncHashToScroll);
  }, { passive: true });
})();
"#
}
