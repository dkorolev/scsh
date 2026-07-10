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
  return '<span class="chamfer session-status ' + esc(lifecycle.class) + '"><span>' +
    esc(lifecycle.label) + '</span></span>';
}
function setBtnLabel(btn, text) {
  const span = btn.querySelector(':scope > span');
  if (span) span.textContent = text;
  else btn.textContent = text;
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
    const done = (p.status === 'ok' || p.status === 'fail' || p.status === 'skipped');
    const skill = p.skill_name || p.label || '';
    const base = p.harness + ' · ' + skill;
    let tip = base, runningAttr = '';
    if (p.status === 'running' && p.started_at) runningAttr = ' data-tip-running="' + esc(String(p.started_at)) + '"';
    else if (p.status === 'running') tip = base + '\nrunning';
    else if (p.status === 'waiting') tip = base + '\nwaiting';
    else if (p.status === 'ok') tip = base + '\ndone';
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
  return '<tr data-session-id="' + esc(id) + '"><td><a class="job-id" href="/session/' + esc(id) + '">' + esc(id) + '</a></td>' +
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
function emptyOutputLabel(status) {
  return (status === 'ok' || status === 'fail') ? 'No output.' : 'No output yet.';
}
function emptyOutputHtml(status) {
  return '<div class="dim">' + emptyOutputLabel(status) + '</div>';
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
  if (status === 'skipped') return 'skipped';
  if (status === 'fail') {
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
      '<tr><td colspan="7" class="dim">No jobs yet — run <code>scsh run</code> to start one.</td></tr>';
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
  // The per-proc kill button only makes sense while the proc still runs.
  const killEl = det.querySelector('button[data-proc-stop]');
  if (killEl && p.status !== 'running' && p.status !== 'waiting') killEl.remove();
  // A step whose commits were integrated gains its "⇄ commits diff" chip. Integration
  // (and the packdiff pack) happens after the step finished, so this lands on a late tick.
  if (p.diff_path && !det.querySelector('a[data-proc-diff]')) {
    const summary = det.querySelector('summary');
    if (summary) {
      summary.insertAdjacentHTML('beforeend', procDiffBtnHtml(p));
      wireProcDiff(summary.querySelector('a[data-proc-diff]'));
    }
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
    // On finish, reload once so the player has the complete recording, not the partial
    // one; keep the viewer's position, end live mode cleanly, and hide the Live toggle.
    const wasRunning = castEl.dataset.status === 'running' || castEl.dataset.status === 'waiting';
    castEl.dataset.status = p.status;
    const liveBtn = castEl.querySelector('[data-cast-live]');
    if (liveBtn) liveBtn.hidden = p.status !== 'running';
    if (wasRunning && (p.status === 'ok' || p.status === 'fail')) {
      castEl.dataset.ended = String(Math.round(Date.now() / 1000));
      if (castEl._live) setCastLive(castEl, false);
      createCastPlayer(castEl, castEl._player ? castEl._player.getCurrentTime() : null);
    }
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
// Mirrors proc_diff_btn_html in session.rs.
function procDiffBtnHtml(p) {
  return '<a class="proc-diff" data-proc-diff href="/diff/' + encodeURIComponent(SESSION_ID) + '/' + p.index +
    '" title="Browse the commits this step brought into your branch — one self-contained review page">⇄ commits diff</a>';
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
    '<button type="button" data-cast-live' + (p.status === 'running' ? '' : ' hidden') + '>● Live</button>' +
    '<a href="' + esc(base) + '?dl=1" download>⬇ .cast</a>' +
    '<a href="' + esc(base) + '/export.html" data-cast-export download hidden>⬇ Download run snapshot</a>' +
    '<span class="cast-keys dim">space · ←/→ seek · &lt;/&gt; speed · [/] chapter · c chapters · f fullscreen</span>' +
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
    // The player owns live-mode state (a rewind drops it): mirror it onto the toggle.
    box.addEventListener('beecast-livechange', (e) => {
      box._live = !!(e.detail && e.detail.live);
      const b = box.querySelector('[data-cast-live]');
      if (b) b.classList.toggle('on', box._live);
    });
    // A still-running recording is LIVE by default: parked at the growing edge, the bar
    // pinned full-width in live green — not a playhead chasing a moving duration.
    if (box.dataset.status === 'running') { box._live = true; createCastPlayer(box, 'end'); }
    else createCastPlayer(box);
    box.querySelector('[data-cast-live]').addEventListener('click', () => setCastLive(box, !box._live));
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
    const exportLink = box.querySelector('[data-cast-export]');
    if (exportLink) exportLink.hidden = !stats.events;
    if (!stats.events) {
      mount.innerHTML = castPlaceholderHtml(box.dataset.status);
      return;
    }
    mount.innerHTML = '';
    const chapters = (meta.chapters || []).filter(c => typeof c.t === 'number');
    // Chapters are player chrome (the ☰ panel + seek-bar ticks + [/] keys): markers are
    // ALL the wiring they need. scsh renders only the one-line summary above the player.
    const markers = chapters.map(c => [c.t, String(c.title || '')]);
    // fullscreenEl: the player's ⛶ button and `f` key fullscreen the whole cast box, so
    // scsh's chrome (the summary line, the toolbar) rides along.
    const opts = { fit: 'both', controls: true, idleTimeLimit: 2, markers, fullscreenEl: box, accessibility: 'snapshot' };
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
    const det = box.closest('details');
    const active = document.activeElement;
    if ((!det || det.open) && (!active || active === document.body || box.contains(active))) focusCastPlayer(box);
    if (box._live) setCastLive(box, true);
    renderCastSummary(box, meta.summary);
    // Chapters are written by the annotation pass AFTER the run ends; a finished cast with
    // none yet shows a clear "summarizing…" element and swaps the chapters in live when the
    // sidecar lands — no browser refresh. (Polling stops quietly if annotation never comes,
    // e.g. no cursor-agent on the host.)
    if (!chapters.length && box.dataset.status !== 'running') pollForChapters(box, chaptersUrl);
  });
}
function focusCastPlayer(box) {
  const root = box.querySelector('.beecast-player');
  if (!root) return;
  try { root.focus({ preventScroll: true }); } catch (_) { try { root.focus(); } catch (_) {} }
}
// The annotation pass starts right after the run ends, so chapters land within minutes or
// never (no annotator on the host, or a recording from before annotation existed). Show the
// indicator and poll only inside that window — an old cast gets neither.
const CHAPTERS_WAIT_SECS = 300;
function pollForChapters(box, chaptersUrl) {
  if (box._chapPoll) return;
  const endedAt = Number(box.dataset.ended || 0);
  const sinceEnd = () => Date.now() / 1000 - endedAt;
  if (!endedAt || sinceEnd() > CHAPTERS_WAIT_SECS) return;
  const bar = box.querySelector('.cast-toolbar');
  const pending = document.createElement('span');
  pending.className = 'dim chap-pending';
  pending.textContent = '⏳ chapters: summarizing…';
  if (bar) bar.appendChild(pending);
  box._chapPoll = setInterval(() => {
    if (sinceEnd() > CHAPTERS_WAIT_SECS) { clearInterval(box._chapPoll); box._chapPoll = null; pending.remove(); return; }
    fetch(chaptersUrl).then(r => r.ok ? r.json() : {}).then(meta => {
      const chapters = (meta.chapters || []).filter(c => typeof c.t === 'number');
      if (!chapters.length) return;
      clearInterval(box._chapPoll);
      box._chapPoll = null;
      pending.remove();
      // Re-create at the same position so the timeline gains its markers too.
      createCastPlayer(box, box._player ? box._player.getCurrentTime() : null);
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
  const btn = box.querySelector('[data-cast-live]');
  if (btn) btn.classList.toggle('on', box._live);
  // Declared-live (player.setLive): parked at the growing edge, appends pinned
  // unconditionally, the bar full-width in live green. The player drops it itself on a
  // rewind, and the livechange listener above re-syncs the toggle.
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
  const body = hasCast(p)
    ? castEmbedHtml(p)
    : autoscrollCtlHtml(p) + '<div class="output">' + ((p.lines || []).map(l => lineHtml(l)).join('') || emptyOutputHtml(p.status)) + '</div>';
  const elapsedText = elapsedPhrase(p.status, procElapsed(p, nowUnix), p.fail_reason);
  const summaryOpen = '<details class="proc ' + esc(p.status) + '" data-index="' + esc(String(p.index)) + '"' +
    (isOpen ? ' open' : '') + '><summary>' +
    '<span class="triangle" aria-hidden="true"></span> ' +
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
    const session = liveSessions[SESSION_ID];
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
  root.querySelectorAll('.output').forEach(setupOutputScroll);
  root.querySelectorAll('details.proc').forEach(det => {
    const cb = det.querySelector('[data-autoscroll]');
    if (cb) autoScrollByProc.set(det.dataset.index, cb.checked);
  });
  applyAutoScrollAll(root);
  initCasts(root);
  initSessionStop();
  initProcKills(root);
  initProcDiffs(root);
  initHarnessStops();
  initFleetJumps();
})();
function initFleetJumps() {
  document.querySelectorAll('.fleet-jump').forEach((btn) => {
    btn.addEventListener('click', (ev) => {
      ev.preventDefault();
      const idx = btn.getAttribute('data-proc');
      if (idx == null) return;
      const det = document.querySelector('details.proc[data-index="' + idx + '"]');
      if (!det) return;
      det.open = true;
      det.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    });
  });
}
function syncSessionStopButton(session) {
  const btn = document.getElementById('session-stop');
  if (!btn) return;
  if (session && session.ended_at) {
    // The run is over: the button goes away and the resting status badge (green
    // “completed”, red “failed”, …) follows the heading — the same place the server
    // renders it for an ended session.
    btn.remove();
    const kind = document.querySelector('.session-kind');
    if (kind && !kind.querySelector('.session-status')) {
      kind.insertAdjacentHTML('beforeend', ' ' + sessionStatusBadge(sessionLifecycle(session, Date.now() / 1000)));
    }
  }
}
async function forceStopSession(btn) {
  const id = btn.getAttribute('data-session') || SESSION_ID;
  if (!id) return;
  if (!confirm('Force-stop this job? Running containers will be killed.')) return;
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
      alert(data.error || ('stop failed (HTTP ' + resp.status + ')'));
      return;
    }
    setBtnLabel(btn, data.already_ended ? 'Already ended' : 'Stopped');
  } catch (e) {
    btn.disabled = false;
    setBtnLabel(btn, 'Force stop');
    alert(String(e));
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
  if (!session || Number.isNaN(proc)) return;
  if (!confirm('Force-stop this container? Only this run stops; the rest of the job continues.')) return;
  btn.disabled = true;
  btn.textContent = 'stopping\u2026';
  try {
    const resp = await fetch('/api/v1/proc/stop', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ session: session, proc: proc }),
    });
    const data = await resp.json().catch(() => ({}));
    if (!resp.ok || data.ok !== true) {
      btn.disabled = false;
      btn.textContent = '\u2715 Force stop';
      alert(data.error || ('stop failed (HTTP ' + resp.status + ')'));
      return;
    }
    btn.textContent = data.already_ended ? 'already ended' : 'stopped';
  } catch (e) {
    btn.disabled = false;
    btn.textContent = '\u2715 Force stop';
    alert(String(e));
  }
}
// ---- stop-all-of-a-harness (index page) ----
async function stopHarness(btn) {
  const harness = btn.getAttribute('data-harness-stop');
  if (!harness) return;
  if (!confirm('Stop ALL running ' + harness + ' containers, in every job?')) return;
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
      alert(data.error || ('stop failed (HTTP ' + resp.status + ')'));
      return;
    }
    setBtnLabel(btn, 'stopped ' + (data.stopped || 0));
  } catch (e) {
    btn.disabled = false;
    setBtnLabel(btn, '\u2715 stop all ' + harness);
    alert(String(e));
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
// ---- images panel (index page only) ----
function imageStatusBadge(img) {
  if (!img.exists) return '<span class="chamfer session-status failed"><span>missing</span></span>';
  if (!img.up_to_date) return '<span class="chamfer session-status cancelled"><span>stale</span></span>';
  return '<span class="chamfer session-status completed"><span>up to date</span></span>';
}
function imageCheckingBadge() {
  return '<span class="chamfer session-status checking"><span>checking…</span></span>';
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
function renderImages(data) {
  const body = document.getElementById('images-body');
  if (!body) return;
  const note = document.getElementById('images-note');
  if (data.error) {
    // Keep the known image list; surface the failure in the note, not by emptying the table.
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
  renderRuntimeSelector(data);
  wireImageSelectButtons(body);
  wireImageBuildButtons(body);
}
function refreshImages() {
  markImagesChecking();
  const url = '/api/v1/images' + (IMAGES_RUNTIME ? '?runtime=' + encodeURIComponent(IMAGES_RUNTIME) : '');
  fetch(url).then(r => r.json()).then(renderImages).catch(() => {
    renderImages({ error: 'images unavailable (daemon error)' });
  });
}
let IMAGES_RUNTIME = ''; // '' = the host default; set by the selector in the panel
function renderRuntimeSelector(data) {
  const box = document.getElementById('images-runtimes');
  if (!box) return;
  const available = data.available || [];
  if (available.length < 2) { box.innerHTML = ''; return; }
  // Apple `container` and docker/podman keep SEPARATE image stores — this segmented
  // control says which world the whole tab (table + Build buttons) talks to.
  const label = (r) => r === 'container' ? 'Apple Containers' : r === 'docker' ? 'Docker' : r === 'podman' ? 'Podman' : r;
  box.innerHTML = '<span class="seg" data-tip="Each runtime keeps its own image store — the table and the Build buttons apply to the selected one">' +
    available.map(r =>
      '<button type="button" class="seg-opt' + (r === data.runtime ? ' active' : '') +
      '" data-runtime="' + esc(r) + '">' + esc(label(r)) + '</button>').join('') +
    '</span>';
  box.querySelectorAll('[data-runtime]').forEach(b => b.addEventListener('click', () => {
    if (b.classList.contains('active')) return;
    IMAGES_RUNTIME = b.dataset.runtime;
    refreshImages();
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
      window.location.href = '/session/' + resp.session;
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
(function initImagesPanel() {
  if (!document.getElementById('images-body')) return;
  refreshImages();
  document.getElementById('images-build-selected')?.addEventListener('click', () => startImagesBuild(false));
  document.getElementById('images-build-all')?.addEventListener('click', () => startImagesBuild(true));
  document.getElementById('images-refresh')?.addEventListener('click', (e) => { e.preventDefault(); refreshImages(); });
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
(function initTabs() {
  const tabs = document.querySelectorAll('.tab');
  if (!tabs.length) return;
  tabs.forEach(t => t.addEventListener('click', () => {
    const id = t.dataset.tab;
    document.querySelectorAll('.tab').forEach(x => x.classList.toggle('active', x === t));
    document.querySelectorAll('.tab-panel').forEach(p => p.classList.toggle('active', p.id === 'tab-' + id));
    if (id === 'images' && typeof refreshImages === 'function') refreshImages();
  }));
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
function createProject() {
  const input = document.getElementById('project-name');
  const note = document.getElementById('repo-note');
  const name = (input?.value || '').trim();
  if (!name) { if (note) note.textContent = 'enter a project name'; return; }
  if (note) note.textContent = 'creating…';
  fetch('/api/v1/projects/create', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: name }),
  }).then(r => r.json()).then(resp => {
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
    if (panel) { panel.hidden = false; panel.scrollIntoView({ behavior: 'smooth', block: 'start' }); }
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
  form.scrollIntoView({ behavior: 'smooth', block: 'start' });
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
    if (resp.ok && resp.session) { window.location.href = '/session/' + resp.session; }
    else if (note) note.textContent = resp.error || 'could not start job';
  }).catch(() => { if (note) note.textContent = 'could not start job'; });
}
// Mirrors repo_jobs_rows in index.rs — keep the markup identical. The tbody arrives
// server-rendered, so a null snapshot (no full tick yet) must leave it untouched.
function renderRepoJobs(sessions, nowUnix) {
  const body = document.getElementById('repos-body');
  if (!body || !sessions) return;
  nowUnix = nowUnix ?? (Date.now() / 1000);
  const byRepo = {};
  Object.keys(OPEN_REPOS).forEach(r => { byRepo[r] = []; });
  Object.keys(sessions).forEach(id => {
    const s = sessions[id];
    if (!s || !s.repo || s.repo === '(image builds)') return;
    (byRepo[s.repo] = byRepo[s.repo] || []).push(Object.assign({ id: id }, s));
  });
  const repos = Object.keys(byRepo).sort();
  if (!repos.length) {
    body.innerHTML = '<tr><td colspan="2" class="dim">No jobs yet — open or create a project under “New job”.</td></tr>';
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
            '"><span>' + esc(lc.label) + '</span></span> <a class="job-id" href="/session/' + esc(s.id) + '">' + esc(s.id) +
            '</a> <span class="dim">' + esc(formatShortAge(nowUnix - activity(s))) + '</span></div>';
        }).join('');
        return '<div class="repo-jobgroup"><span class="repo-jobgroup-name">' + esc(task) + '</span>' + links + '</div>';
      }).join('');
    }
    return '<tr><td class="repo-path" title="' + esc(repo) + '">' + esc(repoLabel(repo)) + '</td><td>' + cells + '</td></tr>';
  }).join('');
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
  }).catch(() => {});
})();
"#
}
