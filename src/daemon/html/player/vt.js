// beecast-player: the portable, DOM-free core (see the crate README).
//
// Clean-room implementation against public documentation only: the asciicast v1/v2/v3
// format descriptions and ECMA-48 / xterm control-sequence references. MIT, like the
// rest of BeeCast.
//
// The exports (attached to `BeeCastVT`, plus CommonJS for the Node self-test):
//   parseCast(text)         -> { cols, rows, events, duration, … }  (times in recording seconds)
//   appendCast(cast, text)  -> grow a parsed cast with newly produced lines (live-follow)
//   buildPacing / extendPacing / mapTime -> the recording-time ↔ paced-time map
//   new Term(cols, rows)    -> .write(text), .resize(c, r), .snapshot()
'use strict';
(function (root) {

// ---- asciicast parsing ----------------------------------------------------------------
// v1: one JSON document: {version:1, width, height, stdout:[[delay, text], …]} — delays
//     are intervals between events.
// v2: NDJSON: {version:2, width, height} header, then [absolute_seconds, "o"|…, data].
// v3: NDJSON: {version:3, term:{cols, rows}} header, `#` comment lines allowed, then
//     [interval_seconds, type, data] — "o" output, "r" resize ("COLSxROWS"), "m" marker.
function parseCast(text) {
  const src = String(text || '');
  const trimmed = src.trimStart();
  if (trimmed.startsWith('{') && trimmed.includes('"stdout"')) {
    const v1 = tryJson(trimmed);
    if (v1 && Array.isArray(v1.stdout)) {
      let t = 0;
      const events = [];
      for (const pair of v1.stdout) {
        if (!Array.isArray(pair) || pair.length < 2) continue;
        t += Number(pair[0]) || 0;
        events.push({ t: t, type: 'o', data: String(pair[1]) });
      }
      return { cols: num(v1.width, 80), rows: num(v1.height, 24), events: events, duration: t, version: 1, tail: '' };
    }
  }
  let cols = 80, rows = 24, version = 3, abs = 0;
  const events = [];
  // A live producer can hand over a prefix cut mid-line: when the text ends in a partial
  // line that does not parse, hold it back as `tail` so `appendCast` can complete it.
  let body = src, tail = '';
  const cut = src.lastIndexOf('\n');
  const last = src.slice(cut + 1).trim();
  if (last && !tryJson(last)) { body = src.slice(0, cut + 1); tail = src.slice(cut + 1); }
  for (const raw of body.split('\n')) {
    const line = raw.trim();
    if (!line || line[0] === '#') continue;
    if (line[0] === '{') {
      const h = tryJson(line);
      if (h) {
        version = num(h.version, 3);
        if (h.term) { cols = num(h.term.cols, cols); rows = num(h.term.rows, rows); }
        cols = num(h.width, cols);
        rows = num(h.height, rows);
      }
      continue;
    }
    if (line[0] !== '[') continue;
    const ev = tryJson(line);
    if (!Array.isArray(ev) || ev.length < 2 || typeof ev[0] !== 'number') continue;
    abs = version >= 3 ? abs + ev[0] : Math.max(abs, ev[0]);
    const type = String(ev[1]);
    if (type !== 'o' && type !== 'r' && type !== 'm') continue; // input/exit don't render
    events.push({ t: abs, type: type, data: ev.length > 2 ? String(ev[2]) : '' });
  }
  return { cols: cols, rows: rows, events: events, duration: abs, version: version, tail: tail };
}
function tryJson(s) { try { return JSON.parse(s); } catch (_) { return null; } }
function num(v, dflt) { const n = Number(v); return Number.isFinite(n) && n > 0 ? Math.floor(n) : dflt; }

// Live-follow: append newly produced NDJSON lines to a cast returned by `parseCast`.
// Chunk boundaries are free — only complete (newline-terminated) lines are consumed, and
// a trailing partial line is buffered on `cast.tail` until its remainder arrives. Event
// times read exactly as at load (v2 absolute, v3 intervals); stray header or `#` comment
// lines are skipped. A v1 cast is one JSON document with no line to append to, so it
// never grows. Returns the number of renderable events appended.
function appendCast(cast, text) {
  const buf = (cast.tail || '') + String(text == null ? '' : text);
  if (cast.version === 1) { cast.tail = ''; return 0; }
  const cut = buf.lastIndexOf('\n');
  if (cut < 0) { cast.tail = buf; return 0; }
  cast.tail = buf.slice(cut + 1);
  let added = 0;
  for (const raw of buf.slice(0, cut).split('\n')) {
    const line = raw.trim();
    if (!line || line[0] !== '[') continue; // headers, comments, and noise all skip
    const ev = tryJson(line);
    if (!Array.isArray(ev) || ev.length < 2 || typeof ev[0] !== 'number') continue;
    cast.duration = cast.version >= 3 ? cast.duration + ev[0] : Math.max(cast.duration, ev[0]);
    const type = String(ev[1]);
    if (type !== 'o' && type !== 'r' && type !== 'm') continue; // input/exit don't render
    cast.events.push({ t: cast.duration, type: type, data: ev.length > 2 ? String(ev[2]) : '' });
    added++;
  }
  return added;
}

// ---- pacing map ------------------------------------------------------------------------
// Playback runs on a "paced" clock where every silent gap longer than `limit` is shortened
// to exactly `limit`. Both directions of the piecewise-linear map recording-time ↔ paced-
// time derive from the event times: built once at load, extended in place as a live
// recording grows (`extendPacing` from the first new event after each `appendCast`).
function buildPacing(events, duration, limit) {
  const pacing = { rec: [0], paced: [0], limit: limit == null ? null : limit, pacedDuration: 0 };
  extendPacing(pacing, events, 0, duration);
  return pacing;
}
function extendPacing(pacing, events, fromIdx, duration) {
  let lastRec = pacing.rec[pacing.rec.length - 1];
  let lastPaced = pacing.paced[pacing.paced.length - 1];
  const push = function (t) {
    if (t <= lastRec) return;
    const gap = t - lastRec;
    lastPaced += pacing.limit != null && gap > pacing.limit ? pacing.limit : gap;
    lastRec = t;
    pacing.rec.push(lastRec);
    pacing.paced.push(lastPaced);
  };
  for (let i = fromIdx; i < events.length; i++) push(events[i].t);
  push(duration);
  pacing.pacedDuration = lastPaced;
}
function mapTime(from, to, t) {
  if (t <= 0) return 0;
  let lo = 0, hi = from.length - 1;
  if (t >= from[hi]) return to[hi] + (t - from[hi]);
  while (lo + 1 < hi) { const mid = (lo + hi) >> 1; if (from[mid] <= t) lo = mid; else hi = mid; }
  const span = from[hi] - from[lo];
  const frac = span > 0 ? (t - from[lo]) / span : 0;
  return to[lo] + frac * (to[hi] - to[lo]);
}

// ---- terminal emulator ----------------------------------------------------------------
// Cell colors: null = default, 0..255 = indexed, '#rrggbb' = truecolor.
// Attribute bits on each cell:
const A_BOLD = 1, A_DIM = 2, A_ITALIC = 4, A_UNDER = 8, A_INVERSE = 16, A_STRIKE = 32;

// DEC special graphics (ESC ( 0): the line-drawing set tmux borders use.
const DEC_GRAPHICS = {
  'j': '┘', 'k': '┐', 'l': '┌', 'm': '└', 'n': '┼', 'q': '─', 't': '├', 'u': '┤',
  'v': '┴', 'w': '┬', 'x': '│', 'a': '▒', '`': '◆', 'f': '°', 'g': '±', 'o': '⎺',
  'p': '⎻', 'r': '⎼', 's': '⎽', '~': '·', 'y': '≤', 'z': '≥', '{': 'π', '|': '≠',
  '}': '£', '.': '▼', ',': '←', '+': '→', '-': '↑', '0': '█', 'h': '▒', 'i': '␋',
};

function blankCell() { return { ch: ' ', fg: null, bg: null, attrs: 0 }; }
function blankRow(cols) { const r = new Array(cols); for (let x = 0; x < cols; x++) r[x] = blankCell(); return r; }

// Parser states for the escape-sequence state machine.
const GROUND = 0, ESC = 1, CSI = 2, OSC = 3, CHARSET = 4, ESC_IGNORE_ONE = 5;

function Term(cols, rows) {
  this.cols = Math.max(1, cols | 0);
  this.rows = Math.max(1, rows | 0);
  this.reset();
}

Term.prototype.reset = function () {
  this.screen = [];
  for (let y = 0; y < this.rows; y++) this.screen.push(blankRow(this.cols));
  this.altScreen = null;      // saved primary screen while the alternate is active
  this.x = 0;
  this.y = 0;
  this.pen = { fg: null, bg: null, attrs: 0 };
  this.cursorVisible = true;
  this.wrapPending = false;   // deferred wrap: set after printing in the last column
  this.autowrap = true;
  this.scrollTop = 0;                 // inclusive
  this.scrollBottom = this.rows - 1;  // inclusive
  this.savedCursor = null;    // DECSC: {x, y, pen, charset…}
  this.charsets = ['B', 'B']; // G0, G1 ('B' = ASCII, '0' = DEC graphics)
  this.charsetIdx = 0;        // SI selects G0, SO selects G1
  this.state = GROUND;
  this.params = '';
  this.oscBuf = '';
  this.charsetSlot = 0;
};

Term.prototype.resize = function (cols, rows) {
  cols = Math.max(1, cols | 0);
  rows = Math.max(1, rows | 0);
  const next = [];
  for (let y = 0; y < rows; y++) {
    const row = blankRow(cols);
    if (y < this.rows) for (let x = 0; x < Math.min(cols, this.cols); x++) row[x] = this.screen[y][x];
    next.push(row);
  }
  this.screen = next;
  this.cols = cols;
  this.rows = rows;
  this.x = Math.min(this.x, cols - 1);
  this.y = Math.min(this.y, rows - 1);
  this.scrollTop = 0;
  this.scrollBottom = rows - 1;
  this.wrapPending = false;
  if (this.altScreen) this.altScreen = null; // a resize mid-alt is rare; drop the stash
};

// Feed a chunk of recorded output through the state machine.
Term.prototype.write = function (text) {
  const s = String(text);
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    switch (this.state) {
      case GROUND: this.ground(ch); break;
      case ESC: this.escState(ch); break;
      case CSI:
        // Parameter/intermediate bytes accumulate; a final byte (0x40–0x7E) dispatches.
        if (ch >= '@' && ch <= '~') { this.csi(ch); this.state = GROUND; }
        else this.params += ch;
        break;
      case OSC:
        // OSC … BEL or OSC … ESC \ — consumed, never rendered.
        if (ch === '\x07') this.state = GROUND;
        else if (ch === '\x1b') this.state = ESC_IGNORE_ONE;
        else this.oscBuf += ch;
        break;
      case ESC_IGNORE_ONE: this.state = GROUND; break; // the `\` of ESC \ (or any ST tail)
      case CHARSET:
        this.charsets[this.charsetSlot] = ch;
        this.state = GROUND;
        break;
    }
  }
};

Term.prototype.ground = function (ch) {
  const code = ch.charCodeAt(0);
  if (ch === '\x1b') { this.state = ESC; return; }
  if (ch === '\r') { this.x = 0; this.wrapPending = false; return; }
  if (ch === '\n' || ch === '\x0b' || ch === '\x0c') { this.lineFeed(); return; }
  if (ch === '\x08') { if (this.x > 0) this.x--; this.wrapPending = false; return; }
  if (ch === '\t') { this.x = Math.min(this.cols - 1, (Math.floor(this.x / 8) + 1) * 8); this.wrapPending = false; return; }
  if (ch === '\x0e') { this.charsetIdx = 1; return; } // SO → G1
  if (ch === '\x0f') { this.charsetIdx = 0; return; } // SI → G0
  if (code < 0x20 || code === 0x7f) return;           // other C0 controls + DEL: ignore
  this.print(ch);
};

Term.prototype.print = function (ch) {
  if (this.charsets[this.charsetIdx] === '0' && DEC_GRAPHICS[ch]) ch = DEC_GRAPHICS[ch];
  if (this.wrapPending) {
    if (this.autowrap) { this.x = 0; this.lineFeed(); }
    this.wrapPending = false;
  }
  const cell = this.screen[this.y][this.x];
  cell.ch = ch;
  cell.fg = this.pen.fg;
  cell.bg = this.pen.bg;
  cell.attrs = this.pen.attrs;
  if (this.x === this.cols - 1) this.wrapPending = true;
  else this.x++;
};

Term.prototype.lineFeed = function () {
  this.wrapPending = false;
  if (this.y === this.scrollBottom) this.scrollUp(1);
  else if (this.y < this.rows - 1) this.y++;
};

Term.prototype.scrollUp = function (n) {
  for (let k = 0; k < n; k++) {
    this.screen.splice(this.scrollTop, 1);
    this.screen.splice(this.scrollBottom, 0, blankRow(this.cols));
  }
};

Term.prototype.scrollDown = function (n) {
  for (let k = 0; k < n; k++) {
    this.screen.splice(this.scrollBottom, 1);
    this.screen.splice(this.scrollTop, 0, blankRow(this.cols));
  }
};

Term.prototype.escState = function (ch) {
  switch (ch) {
    case '[': this.state = CSI; this.params = ''; return;
    case ']': this.state = OSC; this.oscBuf = ''; return;
    case '(': this.state = CHARSET; this.charsetSlot = 0; return;
    case ')': this.state = CHARSET; this.charsetSlot = 1; return;
    case 'D': this.lineFeed(); break;                                  // IND
    case 'E': this.x = 0; this.lineFeed(); break;                      // NEL
    case 'M':                                                          // RI (reverse index)
      this.wrapPending = false;
      if (this.y === this.scrollTop) this.scrollDown(1);
      else if (this.y > 0) this.y--;
      break;
    case '7': this.saveCursor(); break;                                // DECSC
    case '8': this.restoreCursor(); break;                             // DECRC
    case 'c': { const c = this.cols, r = this.rows; this.reset(); this.cols = c; this.rows = r; // RIS
      this.screen = []; for (let y = 0; y < r; y++) this.screen.push(blankRow(c));
      this.scrollBottom = r - 1; break; }
    case 'P': case 'X': case '^': case '_': this.state = OSC; this.oscBuf = ''; return; // DCS/SOS/PM/APC: consume to ST
    case '=': case '>': break;                                         // keypad modes: ignore
    default: break;                                                    // unknown ESC final: ignore
  }
  if (this.state === ESC) this.state = GROUND;
};

Term.prototype.saveCursor = function () {
  this.savedCursor = {
    x: this.x, y: this.y,
    pen: { fg: this.pen.fg, bg: this.pen.bg, attrs: this.pen.attrs },
    charsets: this.charsets.slice(), charsetIdx: this.charsetIdx,
  };
};

Term.prototype.restoreCursor = function () {
  const s = this.savedCursor;
  if (!s) { this.x = 0; this.y = 0; return; }
  this.x = Math.min(s.x, this.cols - 1);
  this.y = Math.min(s.y, this.rows - 1);
  this.pen = { fg: s.pen.fg, bg: s.pen.bg, attrs: s.pen.attrs };
  this.charsets = s.charsets.slice();
  this.charsetIdx = s.charsetIdx;
  this.wrapPending = false;
};

Term.prototype.csi = function (final) {
  const priv = this.params.startsWith('?');
  const raw = priv ? this.params.slice(1) : this.params;
  // Colon sub-parameters (SGR 38:2:…) read the same as semicolons for our subset.
  const parts = raw.replace(/:/g, ';').split(';');
  const p = [];
  for (const part of parts) p.push(part === '' ? 0 : parseInt(part, 10) || 0);
  const n = Math.max(1, p[0] || 0);
  this.wrapPending = false;
  if (priv) { this.decMode(final, p); return; }
  switch (final) {
    case 'A': this.y = Math.max(this.scrollRegionTopFor(this.y), this.y - n); break;      // CUU
    case 'B': case 'e': this.y = Math.min(this.scrollRegionBottomFor(this.y), this.y + n); break; // CUD/VPR
    case 'C': case 'a': this.x = Math.min(this.cols - 1, this.x + n); break;              // CUF/HPR
    case 'D': this.x = Math.max(0, this.x - n); break;                                    // CUB
    case 'E': this.x = 0; this.y = Math.min(this.rows - 1, this.y + n); break;            // CNL
    case 'F': this.x = 0; this.y = Math.max(0, this.y - n); break;                        // CPL
    case 'G': case '`': this.x = clamp((p[0] || 1) - 1, 0, this.cols - 1); break;         // CHA/HPA
    case 'd': this.y = clamp((p[0] || 1) - 1, 0, this.rows - 1); break;                   // VPA
    case 'H': case 'f':                                                                    // CUP/HVP
      this.y = clamp((p[0] || 1) - 1, 0, this.rows - 1);
      this.x = clamp((p[1] || 1) - 1, 0, this.cols - 1);
      break;
    case 'J': this.eraseDisplay(p[0] || 0); break;                                        // ED
    case 'K': this.eraseLine(p[0] || 0); break;                                           // EL
    case 'L': if (this.inScrollRegion()) this.insertLines(n); break;                      // IL
    case 'M': if (this.inScrollRegion()) this.deleteLines(n); break;                      // DL
    case 'P': this.deleteChars(n); break;                                                 // DCH
    case '@': this.insertChars(n); break;                                                 // ICH
    case 'X': this.eraseChars(n); break;                                                  // ECH
    case 'S': this.scrollUp(n); break;                                                    // SU
    case 'T': this.scrollDown(n); break;                                                  // SD
    case 'r':                                                                             // DECSTBM
      this.scrollTop = clamp((p[0] || 1) - 1, 0, this.rows - 1);
      this.scrollBottom = clamp((p[1] || this.rows) - 1, this.scrollTop, this.rows - 1);
      this.x = 0; this.y = this.scrollTop;
      break;
    case 'm': this.sgr(p); break;
    case 's': this.saveCursor(); break;
    case 'u': this.restoreCursor(); break;
    case 'h': case 'l': case 'n': case 't': case 'c': case 'g': case 'q': break;          // consumed, ignored
    default: break;
  }
};

Term.prototype.inScrollRegion = function () { return this.y >= this.scrollTop && this.y <= this.scrollBottom; };
Term.prototype.scrollRegionTopFor = function (y) { return y >= this.scrollTop ? this.scrollTop : 0; };
Term.prototype.scrollRegionBottomFor = function (y) { return y <= this.scrollBottom ? this.scrollBottom : this.rows - 1; };

Term.prototype.decMode = function (final, p) {
  const set = final === 'h';
  for (const mode of p) {
    switch (mode) {
      case 25: this.cursorVisible = set; break;
      case 7: this.autowrap = set; break;
      case 47: case 1047: this.switchAltScreen(set, false); break;
      case 1049: this.switchAltScreen(set, true); break;
      case 1048: if (set) this.saveCursor(); else this.restoreCursor(); break;
      default: break; // mouse/bracketed-paste/etc: playback has no input, ignore
    }
  }
};

Term.prototype.switchAltScreen = function (on, withCursor) {
  if (on && !this.altScreen) {
    if (withCursor) this.saveCursor();
    this.altScreen = this.screen;
    this.screen = [];
    for (let y = 0; y < this.rows; y++) this.screen.push(blankRow(this.cols));
  } else if (!on && this.altScreen) {
    this.screen = this.altScreen;
    this.altScreen = null;
    if (withCursor) this.restoreCursor();
  }
};

Term.prototype.eraseDisplay = function (mode) {
  if (mode === 2 || mode === 3) {
    for (let y = 0; y < this.rows; y++) this.clearRowRange(y, 0, this.cols);
  } else if (mode === 1) {
    for (let y = 0; y < this.y; y++) this.clearRowRange(y, 0, this.cols);
    this.clearRowRange(this.y, 0, this.x + 1);
  } else {
    this.clearRowRange(this.y, this.x, this.cols);
    for (let y = this.y + 1; y < this.rows; y++) this.clearRowRange(y, 0, this.cols);
  }
};

Term.prototype.eraseLine = function (mode) {
  if (mode === 2) this.clearRowRange(this.y, 0, this.cols);
  else if (mode === 1) this.clearRowRange(this.y, 0, this.x + 1);
  else this.clearRowRange(this.y, this.x, this.cols);
};

// Erased cells keep the pen's background (BCE) — TUIs rely on it for colored panes.
Term.prototype.clearRowRange = function (y, from, to) {
  const row = this.screen[y];
  for (let x = from; x < Math.min(to, this.cols); x++) {
    row[x] = { ch: ' ', fg: null, bg: this.pen.bg, attrs: 0 };
  }
};

Term.prototype.insertLines = function (n) {
  for (let k = 0; k < n && this.y <= this.scrollBottom; k++) {
    this.screen.splice(this.scrollBottom, 1);
    this.screen.splice(this.y, 0, blankRow(this.cols));
  }
};

Term.prototype.deleteLines = function (n) {
  for (let k = 0; k < n && this.y <= this.scrollBottom; k++) {
    this.screen.splice(this.y, 1);
    this.screen.splice(this.scrollBottom, 0, blankRow(this.cols));
  }
};

Term.prototype.deleteChars = function (n) {
  const row = this.screen[this.y];
  row.splice(this.x, Math.min(n, this.cols - this.x));
  while (row.length < this.cols) row.push({ ch: ' ', fg: null, bg: this.pen.bg, attrs: 0 });
};

Term.prototype.insertChars = function (n) {
  const row = this.screen[this.y];
  for (let k = 0; k < n; k++) row.splice(this.x, 0, { ch: ' ', fg: null, bg: this.pen.bg, attrs: 0 });
  row.length = this.cols;
};

Term.prototype.eraseChars = function (n) {
  this.clearRowRange(this.y, this.x, this.x + n);
};

Term.prototype.sgr = function (p) {
  if (p.length === 0) p = [0];
  for (let i = 0; i < p.length; i++) {
    const v = p[i];
    if (v === 0) this.pen = { fg: null, bg: null, attrs: 0 };
    else if (v === 1) this.pen.attrs |= A_BOLD;
    else if (v === 2) this.pen.attrs |= A_DIM;
    else if (v === 3) this.pen.attrs |= A_ITALIC;
    else if (v === 4) this.pen.attrs |= A_UNDER;
    else if (v === 7) this.pen.attrs |= A_INVERSE;
    else if (v === 9) this.pen.attrs |= A_STRIKE;
    else if (v === 21 || v === 22) this.pen.attrs &= ~(A_BOLD | A_DIM);
    else if (v === 23) this.pen.attrs &= ~A_ITALIC;
    else if (v === 24) this.pen.attrs &= ~A_UNDER;
    else if (v === 27) this.pen.attrs &= ~A_INVERSE;
    else if (v === 29) this.pen.attrs &= ~A_STRIKE;
    else if (v >= 30 && v <= 37) this.pen.fg = v - 30;
    else if (v >= 90 && v <= 97) this.pen.fg = v - 90 + 8;
    else if (v === 39) this.pen.fg = null;
    else if (v >= 40 && v <= 47) this.pen.bg = v - 40;
    else if (v >= 100 && v <= 107) this.pen.bg = v - 100 + 8;
    else if (v === 49) this.pen.bg = null;
    else if (v === 38 || v === 48) {
      const isFg = v === 38;
      if (p[i + 1] === 5) {
        const idx = clamp(p[i + 2] || 0, 0, 255);
        if (isFg) this.pen.fg = idx; else this.pen.bg = idx;
        i += 2;
      } else if (p[i + 1] === 2) {
        const hex = '#' + hex2(p[i + 2]) + hex2(p[i + 3]) + hex2(p[i + 4]);
        if (isFg) this.pen.fg = hex; else this.pen.bg = hex;
        i += 4;
      }
    }
  }
};

// The visible screen as rows of style-merged runs, plus the cursor — everything a
// renderer needs, nothing tied to any renderer.
Term.prototype.snapshot = function () {
  const rows = [];
  for (let y = 0; y < this.rows; y++) {
    const runs = [];
    let cur = null;
    for (let x = 0; x < this.cols; x++) {
      const c = this.screen[y][x];
      if (cur && cur.fg === c.fg && cur.bg === c.bg && cur.attrs === c.attrs) cur.text += c.ch;
      else {
        cur = { text: c.ch, fg: c.fg, bg: c.bg, attrs: c.attrs };
        runs.push(cur);
      }
    }
    rows.push(runs);
  }
  return {
    cols: this.cols,
    rows: rows,
    cursor: { x: Math.min(this.x, this.cols - 1), y: this.y, visible: this.cursorVisible },
  };
};

// A plain-text dump (no styling), one string per row — for tests and transcripts.
Term.prototype.textLines = function () {
  const out = [];
  for (let y = 0; y < this.rows; y++) {
    let line = '';
    for (let x = 0; x < this.cols; x++) line += this.screen[y][x].ch;
    out.push(line.replace(/\s+$/, ''));
  }
  return out;
};

function clamp(v, lo, hi) { return v < lo ? lo : v > hi ? hi : v; }
function hex2(v) { return (clamp(v || 0, 0, 255)).toString(16).padStart(2, '0'); }

// The xterm 256-color palette for indexed cells ≥ 16 (0–15 come from CSS variables so
// the embedding page can theme them; see player.js/player.css).
function color256(idx) {
  if (idx < 16) return null; // themed via CSS
  if (idx >= 232) { const g = 8 + (idx - 232) * 10; return '#' + hex2(g) + hex2(g) + hex2(g); }
  const v = idx - 16;
  const lv = [0, 95, 135, 175, 215, 255];
  return '#' + hex2(lv[Math.floor(v / 36)]) + hex2(lv[Math.floor(v / 6) % 6]) + hex2(lv[v % 6]);
}

const api = {
  parseCast: parseCast,
  appendCast: appendCast,
  buildPacing: buildPacing,
  extendPacing: extendPacing,
  mapTime: mapTime,
  Term: Term,
  color256: color256,
  A_BOLD: A_BOLD, A_DIM: A_DIM, A_ITALIC: A_ITALIC, A_UNDER: A_UNDER, A_INVERSE: A_INVERSE, A_STRIKE: A_STRIKE,
};
root.BeeCastVT = api;
if (typeof module !== 'undefined' && module.exports) module.exports = api;

})(typeof window !== 'undefined' ? window : globalThis);
