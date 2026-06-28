//! HTML and JavaScript string escaping for daemon pages.

/// Escape text for HTML. Single quotes are intentionally omitted — every attribute must use double quotes.
pub(crate) fn esc(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for c in s.chars() {
    match c {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      c => out.push(c),
    }
  }
  out
}

pub(crate) fn quote_js(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('\'');
  for c in s.chars() {
    match c {
      '\\' => out.push_str("\\\\"),
      '\'' => out.push_str("\\'"),
      '<' => out.push_str("\\x3c"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      c => out.push(c),
    }
  }
  out.push('\'');
  out
}
