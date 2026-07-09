# `scsh-cast-player` — first-party asciicast player

A self-contained, dependency-free player for asciicast recordings (v1, v2, and v3),
written for scsh's session browser. It replaces the previously vendored
[asciinema-player](https://github.com/asciinema/asciinema-player) so the browser UI
carries **no third-party code and no third-party license**.

**Clean-room statement.** This component was written from scratch against public format
and protocol documentation only — the asciicast v1/v2/v3 format descriptions and the
standard ECMA-48 / xterm control-sequence references. No asciinema-player source code was
consulted, copied, or translated. Licensed MIT, the same as the rest of scsh
(`LICENSE.md` at the repo root).

## Layout — designed to be split out later

| File | Role |
| --- | --- |
| `vt.js` | **The portable core.** Asciicast parsing (v1/v2/v3) + a VT100/xterm-subset terminal emulator. Pure state machine: bytes in, screen snapshot out. No DOM, no timers, no globals — runs in a browser or Node unchanged. This is the part to port to Rust (→ WASM) when the component becomes its own module. |
| `player.js` | The thin DOM half: renderer (snapshot → HTML runs), playback clock (idle-time compression, speed), controls (play/pause, seek bar with chapter markers, keyboard shortcuts), and the public API. |
| `player.css` | Terminal palette + player chrome. All colors are CSS variables, themeable by the embedding page. |

The two JS files are concatenated at compile time (`include_str!` in
`src/daemon/html/cast.rs`) and served as one asset at `/assets/scsh-cast-player.js`.

## Public API

```js
const player = ScshCastPlayer.create({ data: castText }, mountElement, {
  fit: 'both',        // scale the terminal to the container width
  controls: true,     // render the control bar (default true)
  idleTimeLimit: 2,   // cap silent gaps at N seconds of playback time
  markers: [[t, 'label'], …],  // chapter ticks on the seek bar
  startAt: 12.5,      // seconds, or a 'mm:ss' string
});
player.play();
player.pause();
player.seek(t);            // seconds, or 'mm:ss' — always in RECORDING time
player.getCurrentTime();   // seconds, in RECORDING time
player.dispose();
```

**The time axis is always recording time.** Idle-time compression only affects pacing
(long silences play back at most `idleTimeLimit` seconds long); `seek`,
`getCurrentTime`, markers, and `#t=` deep links all use the recording's own clock, so
chapter sidecars and share-links stay aligned no matter the compression. (The previous
third-party player rewrote the timeline instead, which could drift against sidecars.)

Keyboard, when the player has focus: **space** play/pause · **←/→** seek ±5s ·
**< / >** speed down/up · **[ / ]** previous/next marker.

## Terminal emulation scope

The subset a tmux-hosted TUI actually exercises: cursor addressing (CUP/CUU/CUD/CUF/CUB/
CHA/VPA/CNL/CPL), erase (ED/EL/ECH), insert/delete (ICH/DCH/IL/DL), scroll (SU/SD, DECSTBM
scroll regions, IND/RI/NEL), SGR (16/256/true color, bold, dim, italic, underline, inverse,
strikethrough), alternate screen (`?1049`, `?47`), cursor visibility (`?25`), autowrap with
deferred wrap (`?7`), save/restore cursor (DECSC/DECRC, CSI s/u), DEC special graphics
(`ESC ( 0` line drawing, SO/SI), tab stops, OSC consumption (titles are parsed and ignored),
and v3 in-band resize events. Unrecognized sequences are consumed and ignored — never
rendered as text.

## Testing

`vt.js` self-tests run under Node from `cargo test` (`daemon::html::cast::tests`,
`vt_core_node_selftest`) — the test shells out to `node` and skips silently when Node is
not installed. Rust-side tests pin the served assets, the pages that reference them, and
the absence of any third-party license marker in the browser bundle.
