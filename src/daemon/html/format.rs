//! Time and duration labels for HTML tables and proc headers.

pub(crate) fn format_duration_secs(secs: u64) -> String {
  if secs < 60 {
    return format!("{secs}s");
  }
  let m = secs / 60;
  let s = secs % 60;
  if secs < 3600 {
    return format!("{m}m {s}s");
  }
  let h = secs / 3600;
  format!("{h}h {}m {s}s", (secs % 3600) / 60)
}

/// Compact single-unit age for dense lists ("32s", "5m", "3h", "2d"). Mirrored by
/// `formatShortAge` in the client JS.
pub(crate) fn format_short_age(secs_ago: u64) -> String {
  if secs_ago < 60 {
    return format!("{secs_ago}s");
  }
  if secs_ago < 3600 {
    return format!("{}m", secs_ago / 60);
  }
  if secs_ago < 86400 {
    return format!("{}h", secs_ago / 3600);
  }
  format!("{}d", secs_ago / 86400)
}

pub(crate) fn format_relative_age(secs_ago: u64) -> String {
  if secs_ago < 60 {
    return format!("{secs_ago}s ago");
  }
  let m = secs_ago / 60;
  if secs_ago < 3600 {
    return format!("{m}m ago");
  }
  let h = secs_ago / 3600;
  format!("{h}h {}m ago", (secs_ago % 3600) / 60)
}

pub(crate) fn format_idle(secs: f64) -> String {
  if secs < 1.0 {
    return String::new();
  }
  format!(" · idle {}s", secs.floor() as u64)
}

pub(crate) fn format_elapsed_clock(secs: f64) -> String {
  format!("{}s", secs.floor() as u64)
}

pub(crate) fn line_count_label(n: usize) -> String {
  if n == 1 {
    "1 line".into()
  } else {
    format!("{n} lines")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn format_duration_secs_boundaries() {
    assert_eq!(format_duration_secs(5), "5s");
    assert_eq!(format_duration_secs(65), "1m 5s");
    assert_eq!(format_duration_secs(3723), "1h 2m 3s");
  }

  #[test]
  fn format_relative_age_boundaries() {
    assert_eq!(format_relative_age(30), "30s ago");
    assert_eq!(format_relative_age(90), "1m ago");
    assert_eq!(format_relative_age(3700), "1h 1m ago");
  }

  #[test]
  fn line_count_label_pluralizes() {
    assert_eq!(line_count_label(1), "1 line");
    assert_eq!(line_count_label(2), "2 lines");
  }

  #[test]
  fn format_idle_and_elapsed_clock() {
    assert_eq!(format_idle(0.5), "");
    assert_eq!(format_idle(2.7), " · idle 2s");
    assert_eq!(format_elapsed_clock(12.9), "12s");
  }
}
