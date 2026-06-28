//! Live, interactive terminal UI for scsh's subprocess steps.
//!
//! scsh shells out to `git` (the clone) and a container runtime (the image build), then runs
//! every skill in its own container — all of which can be slow. On an attended terminal these
//! show up on a single **interactive live board**: the image build, then every skill, each a
//! collapsible row (a ▶/▼ triangle, a status glyph, the label, a smart elapsed clock, and a dim
//! note). **Click a row** to expand its captured output from the top — scroll (wheel, arrows,
//! PgUp/PgDn) to read the rest; **End** resumes following the fleet tail.
//! to plain `▶` / `✓` / `✗` lines so pipes and CI stay readable.
//!
//! The pieces, mirroring the design's "pure logic vs. side effects" split:
//!
//! * [`clock`]   — the smart elapsed clock and output-line cleanup (pure, tested).
//! * [`engine`]  — container-engine liveness + start-command advice (pure decision).
//! * [`live`]    — the board's model: layout, scrolling, expand/collapse (pure, tested).
//! * [`screen`]  — the terminal driver: raw mode, mouse, inline in-place redraw, proc handles.
//! * [`signals`] — process-group isolation + SIGINT/SIGTERM handling (restore term, kill kids).

pub mod clock;
pub mod demo;
pub mod engine;
pub mod live;
pub mod screen;
pub mod signals;

pub use engine::Os;

/// How often the live board animates (spinner + clock retick).
pub(crate) const TICK: std::time::Duration = std::time::Duration::from_millis(80);

/// Spinner frames — a smooth braille cycle.
pub(crate) const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
