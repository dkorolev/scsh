# Vendored copy of the `beecast-player` crate

The three files in this directory — `vt.js` (the DOM-free asciicast parser + VT emulator +
pacing map), `player.js` (the renderer, playback clock, controls, and live-follow
`append`), and `player.css` — are a **byte-for-byte copy** of `player/src/` from the
`beecast-player` crate (github.com/dkorolev/beecast), which is the component's canonical
home. The JS globals are `BeeCastVT` and `BeeCastPlayer`.

This vendored copy exists only until `beecast-player` 0.4.0 is published to crates.io; the
planned follow-up replaces this directory with the crate's `PLAYER_JS` / `PLAYER_CSS`
constants and deletes these files. Do not edit them here — land player changes in beecast
and re-copy.

Clean-room, dependency-free, MIT — the session browser ships no third-party code. See the
crate's README for the API and the terminal-emulation scope; `daemon::html::cast::tests`
runs the DOM-free core's Node self-test against this copy.
