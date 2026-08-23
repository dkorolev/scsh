//! Reading a claude usage limit off the harness's own screen.
//!
//! When the account's limit stops a claude session mid-task, the TUI does not exit and does not
//! fail — it parks. From scsh's side that looks exactly like a wedged harness: the screen stops
//! changing, [`crate::ui::screen::NoveltyWatch`] sees nothing novel (it erases digits, so even a
//! ticking countdown reads as static), and the inactivity watchdog kills the container half an
//! hour later. The route is then retried from a fresh clone with a fresh session, so everything
//! the agent had figured out is gone — and the retry backoff tops out long before a limit that
//! resets at breakfast.
//!
//! Claude Code can wait the limit out and continue the SAME conversation
//! (`autoContinueAtUsageLimit`, armed by [`crate::quota::container_settings_json`]). This module
//! is how scsh finds out whether that is happening, so it can hold its watchdogs off instead of
//! killing a run that is about to resume — and so it can tell the difference between a session
//! that will come back and one that never will.
//!
//! The screen is the only channel. Claude writes no machine-readable marker for any of this, so
//! the needles below are its literal TUI prose. They are matched case-insensitively, and the LAST
//! match in one scan wins. The supervisor consumes each scan batch after classifying it, so an old
//! banner cannot outlive later ordinary work; see [`crate::ui::screen::NoveltyWatch`].

/// What the harness screen currently says about a usage limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitState {
  /// A limit stopped the session and claude has armed its own wait: it will continue this same
  /// conversation when the limit resets. Nothing to do but keep the container alive.
  Waiting,
  /// A limit stopped the session and NO wait is armed — the TUI is sitting on its
  /// `/rate-limit-options` dialog whose default choice is "Stop and wait for limit to reset".
  /// Nobody is at the keyboard, so it will sit there until a watchdog kills it.
  Blocked,
  /// The limit has reset and claude wants one keypress before it picks the task back up.
  NeedsEnter,
  /// Claude declined or abandoned the wait — the reset is more than a day out, the limit was hit
  /// again too many times, or the wait was cancelled. This session will not resume by itself.
  Refused,
  /// The wait is over and the agent is working again.
  Resumed,
}

impl LimitState {
  /// Whether this state means the run is parked on a limit rather than making progress. The
  /// watchdog clocks are frozen for exactly these states.
  pub fn is_parked(self) -> bool {
    matches!(self, LimitState::Waiting | LimitState::Blocked | LimitState::NeedsEnter)
  }

  /// Whether the screen says quota has stopped this session. A refusal is not parked because
  /// its clocks must not be frozen, but it still needs the same stillness guard before scsh acts
  /// on prose that an active task could merely be quoting.
  pub fn is_stopped(self) -> bool {
    !matches!(self, LimitState::Resumed)
  }

  /// The tmux key this state needs from the host, if any. Claude's own wait cannot arm itself
  /// from a dialog, and a reset session will not pick the task back up, without a keypress that
  /// only a human would otherwise supply — see [`crate::runtime::RUN_KEYS_DIR`].
  pub fn wants_key(self) -> Option<&'static str> {
    matches!(self, LimitState::Blocked | LimitState::NeedsEnter).then_some("Enter")
  }

  /// A short phrase for the session browser and the run log.
  pub fn label(self) -> &'static str {
    match self {
      LimitState::Waiting => "waiting for the usage limit to reset",
      LimitState::Blocked => "stopped by a usage limit with no wait armed",
      LimitState::NeedsEnter => "usage limit reset; nudging the session to continue",
      LimitState::Refused => "usage limit hit and the session will not resume on its own",
      LimitState::Resumed => "usage limit reset; the session resumed",
    }
  }
}

/// Needles per state, in the order a tie between two equally-late matches should be broken:
/// a terminal verdict outranks a hopeful one, so a screen still showing the armed banner under
/// a fresh "will not resume" line is read as refused.
const NEEDLES: [(LimitState, &[&str]); 5] = [
  (
    LimitState::Refused,
    &[
      "will not resume on its own",
      "automatic continue stopped",
      "automatic continue cancelled",
      "automatic continue did not run",
    ],
  ),
  // "reset" vs "has reset" vs "reached" is the whole distinction between these three, and the
  // needles have to stay disjoint: a substring shared with a neighbour would match at a later
  // offset inside the SAME line and hand that line to the wrong state.
  (LimitState::Resumed, &["usage limit reset"]),
  (LimitState::NeedsEnter, &["usage limit has reset"]),
  (LimitState::Waiting, &["usage limit reached"]),
  (
    LimitState::Blocked,
    &["hit your session limit", "hit your weekly limit", "hit your usage limit", "stop and wait for limit to reset"],
  ),
];

/// The limit state of a stretch of rendered terminal output, or `None` when it says nothing
/// about a limit at all.
///
/// `text` is expected to be ANSI-stripped screen content — see
/// [`crate::ptyrec::cast_output_text`].
pub fn detect(text: &str) -> Option<LimitState> {
  let hay = text.to_ascii_lowercase();
  let mut best: Option<(usize, usize, LimitState)> = None;
  for (rank, (state, needles)) in NEEDLES.iter().enumerate() {
    let Some(at) = needles.iter().filter_map(|n| hay.rfind(n)).max() else { continue };
    // Latest match wins; equal positions cannot happen across distinct needles, but a tie on
    // position falls back to declaration order, which puts the terminal verdicts first.
    if best.is_none_or(|(seen, seen_rank, _)| at > seen || (at == seen && rank < seen_rank)) {
      best = Some((at, rank, *state));
    }
  }
  best.map(|(_, _, state)| state)
}

#[cfg(test)]
mod tests {
  use super::*;

  // Verbatim from Claude Code's own bundle. If these ever drift, the wait silently stops being
  // detected and every limited run goes back to dying on the inactivity watchdog — so they are
  // asserted literally rather than paraphrased.
  const ARMED_AT: &str = "Usage limit reached \u{b7} continuing automatically at 8:50am \u{b7} esc to cancel";
  const ARMED_WHEN: &str = "Usage limit reached \u{b7} continuing automatically when it resets \u{b7} esc to cancel";
  const ARMED_SHORTLY: &str = "Usage limit reached \u{b7} continuing shortly \u{b7} esc to cancel";
  const ARMED_AGAIN: &str = "Usage limit reached again after you continued \u{b7} continuing automatically at 8:50am \u{b7} the automatic-continue setting no longer ends this wait (esc or /rate-limit-options still can)";
  const STALE: &str = "Your usage limit has reset \u{b7} press enter to continue";
  const RESUMED: &str = "Usage limit reset \u{b7} continuing automatically";
  const TOO_FAR: &str = "Automatic continue did not run \u{b7} the reset is more than 24 hours out, so this task will not resume on its own (/rate-limit-options to wait anyway)";
  const REPEATED: &str = "Automatic continue stopped after repeated usage-limit hits \u{b7} this task will not resume on its own (/rate-limit-options to try again)";
  const CANCELLED: &str = "Automatic continue cancelled \u{b7} /rate-limit-options to re-arm";
  const DIALOG: &str = "You've hit your session limit \u{b7} resets 8:50am (Europe/Stockholm)";

  #[test]
  fn armed_banners_read_as_waiting() {
    for s in [ARMED_AT, ARMED_WHEN, ARMED_SHORTLY, ARMED_AGAIN] {
      assert_eq!(detect(s), Some(LimitState::Waiting), "{s}");
    }
  }

  #[test]
  fn the_reset_prompt_asks_for_a_keypress() {
    assert_eq!(detect(STALE), Some(LimitState::NeedsEnter));
    assert_eq!(detect(STALE).unwrap().wants_key(), Some("Enter"));
    assert!(detect(STALE).unwrap().is_parked());
  }

  #[test]
  fn the_dialog_is_blocked_and_also_wants_a_keypress() {
    // Its default choice IS the wait, so one Enter converts a dead screen into an armed one.
    assert_eq!(detect(DIALOG), Some(LimitState::Blocked));
    assert_eq!(detect(DIALOG).unwrap().wants_key(), Some("Enter"));
    assert_eq!(detect("\u{203a} 1. Stop and wait for limit to reset"), Some(LimitState::Blocked));
  }

  #[test]
  fn giving_up_reads_as_refused() {
    for s in [TOO_FAR, REPEATED, CANCELLED] {
      assert_eq!(detect(s), Some(LimitState::Refused), "{s}");
      assert!(!detect(s).unwrap().is_parked());
      assert_eq!(detect(s).unwrap().wants_key(), None);
    }
  }

  #[test]
  fn the_newest_line_in_the_window_wins() {
    // The rolling window still holds the banner when the resume line lands; the run is working
    // again, not waiting, and its watchdog clocks must start back up.
    let screen = format!("{ARMED_AT}\n... an hour of nothing ...\n{RESUMED}\n");
    assert_eq!(detect(&screen), Some(LimitState::Resumed));
    assert!(!detect(&screen).unwrap().is_parked());
    // And the other way round: a fresh limit after a resume parks it again.
    assert_eq!(detect(&format!("{RESUMED}\n{ARMED_AT}")), Some(LimitState::Waiting));
    // A verdict under a stale banner is the verdict.
    assert_eq!(detect(&format!("{ARMED_AT}\n{REPEATED}")), Some(LimitState::Refused));
  }

  #[test]
  fn ordinary_output_says_nothing_about_limits() {
    assert_eq!(detect(""), None);
    assert_eq!(detect("running tests\n  47 passed\ncompiling scsh v1.42.0\n"), None);
    // `scsh quota`'s own summary line names limits without the session being stopped by one.
    assert_eq!(detect("claude (max): 5h session 3% \u{b7} weekly 57%"), None);
  }

  #[test]
  fn matching_ignores_case() {
    assert_eq!(detect(&DIALOG.to_uppercase()), Some(LimitState::Blocked));
    assert_eq!(detect(&ARMED_AT.to_lowercase()), Some(LimitState::Waiting));
  }
}
