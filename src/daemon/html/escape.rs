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

/// Collapse runs of `/` to one and drop a trailing `/` (except when the result is exactly `/`).
pub(crate) fn collapse_slashes(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut prev_slash = false;
  for c in s.chars() {
    if c == '/' {
      if !prev_slash {
        out.push('/');
      }
      prev_slash = true;
    } else {
      out.push(c);
      prev_slash = false;
    }
  }
  if out.len() > 1 && out.ends_with('/') {
    out.pop();
  }
  out
}

/// Percent-decode `%XX` (and leave bare `%` alone). Used for `/project/` and `/repo/` URLs.
pub(crate) fn percent_decode(s: &str) -> String {
  let bytes = s.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%' && i + 2 < bytes.len() {
      let h = from_hex(bytes[i + 1]);
      let l = from_hex(bytes[i + 2]);
      if let (Some(h), Some(l)) = (h, l) {
        out.push((h << 4) | l);
        i += 3;
        continue;
      }
    }
    out.push(bytes[i]);
    i += 1;
  }
  String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
  match b {
    b'0'..=b'9' => Some(b - b'0'),
    b'a'..=b'f' => Some(b - b'a' + 10),
    b'A'..=b'F' => Some(b - b'A' + 10),
    _ => None,
  }
}

/// Encode a path for a `/repo/…` URL: keep `/` separators, percent-encode everything else unsafe.
pub(crate) fn encode_repo_url_path(abs_path: &str) -> String {
  let mut out = String::with_capacity(abs_path.len() + 8);
  for b in abs_path.as_bytes() {
    match *b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(*b as char),
      b => out.push_str(&format!("%{b:02X}")),
    }
  }
  out
}

#[cfg(test)]
mod path_tests {
  use super::*;

  #[test]
  fn collapse_slashes_handles_runs_and_trailing() {
    assert_eq!(collapse_slashes("/a//b///c/"), "/a/b/c");
    assert_eq!(collapse_slashes("///"), "/");
    assert_eq!(collapse_slashes("/"), "/");
    assert_eq!(collapse_slashes("a/b"), "a/b");
  }

  #[test]
  fn percent_roundtrip_for_spaces() {
    assert_eq!(percent_decode("my%20repo"), "my repo");
    assert_eq!(encode_repo_url_path("/tmp/my repo"), "/tmp/my%20repo");
  }
}
