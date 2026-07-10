// beecast-player: the DOM half (see the crate README). Renders BeeCastVT snapshots,
// drives the playback clock, and exposes the public BeeCastPlayer API.
//
// Clean-room implementation, MIT like the rest of BeeCast. The time axis is ALWAYS
// recording time: idle compression (idleTimeLimit) only changes pacing, never the clock
// the API speaks. The pacing map itself lives in the DOM-free core (vt.js).
'use strict';
(function (root) {

const VT = root.BeeCastVT;
const SEEK_STEP_SECS = 5;
const SPEEDS = [0.5, 1, 1.5, 2, 3, 5];

// ---- rendering -------------------------------------------------------------------------
const ATTR_CLASSES = [
  [VT.A_BOLD, 'sp-b'], [VT.A_DIM, 'sp-d'], [VT.A_ITALIC, 'sp-i'],
  [VT.A_UNDER, 'sp-u'], [VT.A_STRIKE, 'sp-s'],
];

function colorCss(c, bold) {
  if (c == null) return null;
  if (typeof c === 'string') return c; // '#rrggbb'
  // Bold brightens the 8 base colors — the classic terminal behavior TUIs count on.
  const idx = bold && c < 8 ? c + 8 : c;
  return idx < 16 ? 'var(--sp-c' + idx + ')' : VT.color256(idx);
}

function esc(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function runHtml(run, hasCursor, cursorCol) {
  // The cursor splits its run into up-to-three spans so only one cell inverts.
  if (hasCursor && run.text.length > 1) {
    const before = { text: run.text.slice(0, cursorCol), fg: run.fg, bg: run.bg, attrs: run.attrs };
    const at = { text: run.text[cursorCol] || ' ', fg: run.fg, bg: run.bg, attrs: run.attrs };
    const after = { text: run.text.slice(cursorCol + 1), fg: run.fg, bg: run.bg, attrs: run.attrs };
    return (before.text ? runHtml(before, false, 0) : '') + runHtml(at, true, 0) +
      (after.text ? runHtml(after, false, 0) : '');
  }
  const inverse = (run.attrs & VT.A_INVERSE) !== 0;
  const bold = (run.attrs & VT.A_BOLD) !== 0;
  let fg = colorCss(run.fg, bold);
  let bg = colorCss(run.bg, false);
  if (inverse) { const t = fg || 'var(--sp-fg)'; fg = bg || 'var(--sp-bg)'; bg = t; }
  const classes = [];
  for (const pair of ATTR_CLASSES) if (run.attrs & pair[0]) classes.push(pair[1]);
  if (hasCursor) classes.push('sp-cur');
  let style = '';
  if (fg) style += 'color:' + fg + ';';
  if (bg) style += 'background:' + bg + ';';
  if (!classes.length && !style) return esc(run.text);
  return '<span' + (classes.length ? ' class="' + classes.join(' ') + '"' : '') +
    (style ? ' style="' + style + '"' : '') + '>' + esc(run.text) + '</span>';
}

function screenHtml(snap) {
  const lines = [];
  for (let y = 0; y < snap.rows.length; y++) {
    let x = 0, html = '';
    for (const run of snap.rows[y]) {
      const cursorHere = snap.cursor.visible && snap.cursor.y === y &&
        snap.cursor.x >= x && snap.cursor.x < x + run.text.length;
      html += runHtml(run, cursorHere, snap.cursor.x - x);
      x += run.text.length;
    }
    lines.push(html);
  }
  return lines.join('\n');
}

function fmtClock(secs) {
  secs = Math.max(0, Math.floor(secs));
  const m = Math.floor(secs / 60), s = secs % 60;
  return m + ':' + String(s).padStart(2, '0');
}

function parseTime(v) {
  if (typeof v === 'number' && isFinite(v)) return v;
  const m = /^(\d+):(\d{1,2})$/.exec(String(v || '').trim());
  if (m) return Number(m[1]) * 60 + Number(m[2]);
  const n = parseFloat(v);
  return isFinite(n) ? n : 0;
}

// ---- player ----------------------------------------------------------------------------
function Player(src, mount, opts) {
  opts = opts || {};
  const cast = VT.parseCast(src && src.data);
  this.cast = cast;
  this.term = new VT.Term(cast.cols, cast.rows);
  this.pacing = VT.buildPacing(cast.events, cast.duration, opts.idleTimeLimit);
  this.markers = (opts.markers || []).map(function (m) { return { t: Number(m[0]) || 0, label: String(m[1] || '') }; });
  this.speed = SPEEDS.indexOf(Number(opts.speed)) >= 0 ? Number(opts.speed) : 1;
  this.playing = false;
  this.pacedPos = 0;   // the playback clock, in paced seconds
  this.eventIdx = 0;   // events [0, eventIdx) are applied to the term
  this.raf = null;
  this.lastTick = null;
  this.disposed = false;
  this.buildDom(mount, opts.controls !== false);
  if (this.speedBtn) this.speedBtn.textContent = String(this.speed).replace(/\.0$/, '') + '\u00d7';
  this.fit = opts.fit || null;
  this.applyEventsUpTo(0);
  if (opts.startAt != null) this.seek(parseTime(opts.startAt));
  this.render();
  this.layout();
  if (opts.autoPlay) this.play();
  const self = this;
  if (typeof ResizeObserver !== 'undefined') {
    this.resizeObs = new ResizeObserver(function () { self.layout(); });
    this.resizeObs.observe(this.root.parentNode || this.root);
  }
}

Player.prototype.buildDom = function (mount, controls) {
  const self = this;
  const root = document.createElement('div');
  root.className = 'beecast-player';
  root.tabIndex = 0;
  root.innerHTML =
    '<div class="sp-screen-box"><pre class="sp-screen"></pre></div>' +
    (controls
      ? '<div class="sp-bar">' +
        '<button class="sp-play" type="button" title="play/pause (space)">▶</button>' +
        '<span class="sp-time">0:00</span>' +
        '<div class="sp-seek"><div class="sp-fill"></div><div class="sp-markers"></div></div>' +
        '<span class="sp-dur">0:00</span>' +
        '<button class="sp-speed" type="button" title="speed (&lt; / &gt;)">1×</button>' +
        '</div>'
      : '');
  mount.appendChild(root);
  this.root = root;
  this.screenEl = root.querySelector('.sp-screen');
  this.playBtn = root.querySelector('.sp-play');
  this.timeEl = root.querySelector('.sp-time');
  this.durEl = root.querySelector('.sp-dur');
  this.seekEl = root.querySelector('.sp-seek');
  this.fillEl = root.querySelector('.sp-fill');
  this.speedBtn = root.querySelector('.sp-speed');
  if (this.durEl) this.durEl.textContent = fmtClock(this.cast.duration);
  if (this.playBtn) this.playBtn.addEventListener('click', function () { self.toggle(); });
  if (this.speedBtn) this.speedBtn.addEventListener('click', function () { self.cycleSpeed(1); });
  if (this.seekEl) {
    const seekTo = function (ev) {
      const r = self.seekEl.getBoundingClientRect();
      const frac = Math.min(1, Math.max(0, (ev.clientX - r.left) / (r.width || 1)));
      self.seek(frac * self.cast.duration);
    };
    this.seekEl.addEventListener('mousedown', function (ev) {
      seekTo(ev);
      const move = function (e) { seekTo(e); };
      const up = function () { document.removeEventListener('mousemove', move); document.removeEventListener('mouseup', up); };
      document.addEventListener('mousemove', move);
      document.addEventListener('mouseup', up);
    });
    this.marksEl = root.querySelector('.sp-markers');
    this.layoutMarkers();
  }
  this.keyHandler = function (ev) { self.onKey(ev); };
  root.addEventListener('keydown', this.keyHandler);
  root.addEventListener('click', function () { try { root.focus({ preventScroll: true }); } catch (_) { root.focus(); } });
};

// (Re)place the chapter ticks: their positions are percentages of the duration, so a
// growing live recording shifts them left as it lengthens.
Player.prototype.layoutMarkers = function () {
  if (!this.marksEl) return;
  this.marksEl.innerHTML = '';
  if (!(this.cast.duration > 0)) return;
  for (const m of this.markers) {
    const tick = document.createElement('div');
    tick.className = 'sp-marker';
    tick.style.left = Math.min(100, (m.t / this.cast.duration) * 100) + '%';
    tick.title = fmtClock(m.t) + (m.label ? ' ' + m.label : '');
    this.marksEl.appendChild(tick);
  }
};

Player.prototype.onKey = function (ev) {
  if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
  const k = ev.key;
  if (k === ' ') this.toggle();
  else if (k === 'ArrowLeft') this.seek(this.getCurrentTime() - SEEK_STEP_SECS);
  else if (k === 'ArrowRight') this.seek(this.getCurrentTime() + SEEK_STEP_SECS);
  else if (k === '<' || k === ',') this.cycleSpeed(-1);
  else if (k === '>' || k === '.') this.cycleSpeed(1);
  else if (k === '[') this.jumpMarker(-1);
  else if (k === ']') this.jumpMarker(1);
  else return;
  ev.preventDefault();
  ev.stopPropagation();
};

Player.prototype.jumpMarker = function (dir) {
  if (!this.markers.length) return;
  const now = this.getCurrentTime();
  let target = null;
  if (dir > 0) {
    for (const m of this.markers) if (m.t > now + 0.25) { target = m.t; break; }
  } else {
    for (const m of this.markers) if (m.t < now - 0.25) target = m.t;
    if (target == null) target = 0;
  }
  if (target != null) { this.seek(target); this.play(); }
};

Player.prototype.cycleSpeed = function (dir) {
  const i = SPEEDS.indexOf(this.speed);
  const next = SPEEDS[Math.min(SPEEDS.length - 1, Math.max(0, (i < 0 ? 1 : i) + dir))];
  this.speed = next;
  if (this.speedBtn) this.speedBtn.textContent = String(next).replace(/\.0$/, '') + '×';
};

// Apply events so that exactly those with recording time <= t are in the terminal.
// Forward from the current position when possible; a backward seek replays from zero
// (the recording is local text — replay is cheap and always exact).
Player.prototype.applyEventsUpTo = function (t) {
  const evs = this.cast.events;
  if (this.eventIdx > 0 && evs[this.eventIdx - 1].t > t) {
    this.term = new VT.Term(this.cast.cols, this.cast.rows);
    this.eventIdx = 0;
  }
  let applied = false;
  while (this.eventIdx < evs.length && evs[this.eventIdx].t <= t) {
    const ev = evs[this.eventIdx++];
    if (ev.type === 'o') this.term.write(ev.data);
    else if (ev.type === 'r') {
      const m = /^(\d+)x(\d+)$/.exec(ev.data.trim());
      if (m) { this.term.resize(Number(m[1]), Number(m[2])); this.layoutPending = true; }
    }
    applied = true;
  }
  return applied;
};

Player.prototype.render = function () {
  this.screenEl.innerHTML = screenHtml(this.term.snapshot());
  this.renderBar();
  if (this.layoutPending) { this.layoutPending = false; this.layout(); }
};

// The control bar alone — cheap enough for every live append even when the screen is not moving.
Player.prototype.renderBar = function () {
  const t = this.getCurrentTime();
  if (this.timeEl) this.timeEl.textContent = fmtClock(t);
  if (this.fillEl) this.fillEl.style.width = (this.cast.duration > 0 ? Math.min(100, (t / this.cast.duration) * 100) : 0) + '%';
};

// fit: scale the fixed-metric terminal down (never up) to the containing box's width —
// and, for fit:'both', also to the mount's height when the embedding page gives it one
// (a definite flex/viewport height; a content-sized mount never shrinks the terminal).
Player.prototype.layout = function () {
  if (!this.fit) return;
  const box = this.root.querySelector('.sp-screen-box');
  this.screenEl.style.transform = '';
  const rect = this.screenEl.getBoundingClientRect();
  const naturalW = rect.width, naturalH = rect.height;
  if (!(naturalW > 0 && naturalH > 0)) return;
  const availW = box.clientWidth;
  let scale = availW > 0 && naturalW > availW ? availW / naturalW : 1;
  if (this.fit === 'both' && this.root.parentNode) {
    const bar = this.root.querySelector('.sp-bar');
    const availH = this.root.parentNode.clientHeight - (bar ? bar.offsetHeight : 0);
    // The 2px slack keeps a content-sized mount (whose height IS the terminal's) stable.
    if (availH > 40 && naturalH * scale > availH + 2) scale = Math.min(scale, availH / naturalH);
  }
  this.screenEl.style.transform = scale < 1 ? 'scale(' + scale + ')' : '';
  box.style.height = naturalH * scale + 'px';
  // Center the (possibly scaled) terminal in the pane; the layout box keeps its unscaled
  // width, so flex centering would be off — compute the margin from the DISPLAY width.
  const displayW = naturalW * scale;
  this.screenEl.style.marginLeft = availW > displayW ? (availW - displayW) / 2 + 'px' : '';
};

Player.prototype.tick = function (nowMs) {
  if (this.disposed || !this.playing) return;
  const dt = this.lastTick == null ? 0 : (nowMs - this.lastTick) / 1000;
  this.lastTick = nowMs;
  this.pacedPos = Math.min(this.pacing.pacedDuration, this.pacedPos + dt * this.speed);
  const changed = this.applyEventsUpTo(this.getCurrentTime());
  if (changed || this.timeEl) this.render();
  if (this.pacedPos >= this.pacing.pacedDuration) { this.pause(); return; }
  const self = this;
  this.raf = requestAnimationFrame(function (ts) { self.tick(ts); });
};

Player.prototype.play = function () {
  if (this.disposed || this.playing) return;
  if (this.pacedPos >= this.pacing.pacedDuration) this.pacedPos = 0; // replay from the top
  this.playing = true;
  this.lastTick = null;
  if (this.playBtn) this.playBtn.textContent = '⏸';
  const self = this;
  this.raf = requestAnimationFrame(function (ts) { self.tick(ts); });
};

Player.prototype.pause = function () {
  this.playing = false;
  if (this.raf != null) { cancelAnimationFrame(this.raf); this.raf = null; }
  if (this.playBtn) this.playBtn.textContent = '▶';
};

Player.prototype.toggle = function () { if (this.playing) this.pause(); else this.play(); };

Player.prototype.seek = function (t) {
  t = Math.min(this.cast.duration, Math.max(0, parseTime(t)));
  this.pacedPos = VT.mapTime(this.pacing.rec, this.pacing.paced, t);
  this.applyEventsUpTo(t);
  this.render();
};

Player.prototype.getCurrentTime = function () {
  return VT.mapTime(this.pacing.paced, this.pacing.rec, this.pacedPos);
};

// Live-follow: feed newly produced cast lines (v2/v3 NDJSON) into a mounted player as the
// recording grows — how the data arrives (WebSocket, polling, a tailed file) is the
// caller's business. Chunk boundaries are free; a partial trailing line is buffered until
// its remainder arrives. The follow policy is positional, like `tail -f`: a playhead
// resting at the live edge stays pinned to it and renders each append immediately, while a
// viewer who paused earlier or seeked back is never yanked forward — they just watch the
// duration grow. A *playing* player keeps its own clock; the longer recording simply no
// longer auto-pauses it at the old end (and once playback catches the edge and parks,
// subsequent appends pick it up and follow).
Player.prototype.append = function (text) {
  if (this.disposed) return;
  const atEdge = !this.playing && this.pacedPos >= this.pacing.pacedDuration - 1e-9;
  const fromIdx = this.cast.events.length;
  const prevDuration = this.cast.duration;
  VT.appendCast(this.cast, text);
  if (this.cast.events.length === fromIdx && this.cast.duration === prevDuration) return;
  VT.extendPacing(this.pacing, this.cast.events, fromIdx, this.cast.duration);
  if (this.durEl) this.durEl.textContent = fmtClock(this.cast.duration);
  this.layoutMarkers();
  if (atEdge) {
    this.pacedPos = this.pacing.pacedDuration;
    this.applyEventsUpTo(this.getCurrentTime());
    this.render();
  } else {
    this.renderBar(); // same playhead, longer recording: only the bar's proportions move
  }
};

Player.prototype.dispose = function () {
  this.disposed = true;
  this.pause();
  if (this.resizeObs) { try { this.resizeObs.disconnect(); } catch (_) {} this.resizeObs = null; }
  if (this.root && this.root.parentNode) this.root.parentNode.removeChild(this.root);
};

root.BeeCastPlayer = {
  create: function (src, mount, opts) { return new Player(src, mount, opts); },
};

})(typeof window !== 'undefined' ? window : globalThis);
