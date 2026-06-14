//! Elapsed-time formatting and subprocess-output line cleanup.
//!
//! Both functions are pure and exhaustively unit-tested; the live runner in
//! [`super::runner`] is the only side-effecting part of the UI.

/// Format an elapsed duration (in seconds) the way scsh's clock reads:
///
/// * below 3 seconds → one decimal place: `0.0s`, `0.1s`, … `2.9s`
/// * 3s and up, under a minute → whole seconds: `3s`, `4s`, … `59s`
/// * a minute and up, under an hour → `1m 5s`, `12m 0s`, … `59m 59s`
/// * an hour and up → `1h 2m 10s`, `3h 0m 0s`, …
///
/// The decimal part is *truncated*, not rounded, so the display never jumps to
/// `3.0s` right before flipping to the whole-second `3s` regime.
pub fn format_elapsed(secs: f64) -> String {
  let secs = if secs.is_finite() && secs > 0.0 { secs } else { 0.0 };

  if secs < 3.0 {
    // Truncate to tenths: 2.99 → "2.9s", never "3.0s".
    let tenths = (secs * 10.0).floor() as u64;
    return format!("{}.{}s", tenths / 10, tenths % 10);
  }

  let total = secs as u64; // truncating cast: 3.7 → 3
  let hours = total / 3600;
  let mins = (total % 3600) / 60;
  let s = total % 60;

  if hours > 0 {
    format!("{hours}h {mins}m {s}s")
  } else if mins > 0 {
    format!("{mins}m {s}s")
  } else {
    format!("{s}s")
  }
}

/// Turn a raw line of subprocess output into the single, tidy line scsh displays:
///
/// * strip ANSI colour/escape codes,
/// * collapse carriage-return progress (`a\rb\rc` → keep only `c`),
/// * trim surrounding whitespace.
///
/// Returns an empty string for lines that were blank once cleaned.
pub fn clean_line(raw: &str) -> String {
  let no_ansi = console::strip_ansi_codes(raw);
  // Carriage returns rewrite the same terminal line; only the last segment is visible.
  let last_segment = no_ansi.rsplit('\r').next().unwrap_or("");
  last_segment.trim().to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sub_three_seconds_is_decimal_and_truncated() {
    assert_eq!(format_elapsed(0.0), "0.0s");
    assert_eq!(format_elapsed(0.1), "0.1s");
    assert_eq!(format_elapsed(1.25), "1.2s");
    assert_eq!(format_elapsed(2.9), "2.9s");
    // The important boundary: must NOT round up to "3.0s".
    assert_eq!(format_elapsed(2.99), "2.9s");
  }

  #[test]
  fn three_seconds_and_up_is_whole_seconds() {
    assert_eq!(format_elapsed(3.0), "3s");
    assert_eq!(format_elapsed(3.7), "3s");
    assert_eq!(format_elapsed(4.0), "4s");
    assert_eq!(format_elapsed(59.9), "59s");
  }

  #[test]
  fn minutes_and_hours() {
    assert_eq!(format_elapsed(60.0), "1m 0s");
    assert_eq!(format_elapsed(65.9), "1m 5s");
    assert_eq!(format_elapsed(125.0), "2m 5s");
    assert_eq!(format_elapsed(3600.0), "1h 0m 0s");
    assert_eq!(format_elapsed(3661.0), "1h 1m 1s");
    assert_eq!(format_elapsed(3730.0), "1h 2m 10s");
  }

  #[test]
  fn negative_or_nonfinite_clamps_to_zero() {
    assert_eq!(format_elapsed(-1.0), "0.0s");
    assert_eq!(format_elapsed(f64::NAN), "0.0s");
    assert_eq!(format_elapsed(f64::INFINITY), "0.0s");
  }

  #[test]
  fn clean_line_strips_ansi_cr_and_whitespace() {
    assert_eq!(clean_line("\x1b[31mred error\x1b[0m"), "red error");
    assert_eq!(clean_line("downloading...\rdownloading done"), "downloading done");
    assert_eq!(clean_line("   padded   "), "padded");
    assert_eq!(clean_line("\x1b[2K\rStep 3/5 : COPY . /app"), "Step 3/5 : COPY . /app");
    assert_eq!(clean_line("   "), "");
  }
}
