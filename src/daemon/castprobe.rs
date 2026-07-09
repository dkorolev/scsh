//! Cheap, incremental probe of a growing asciicast recording's available duration.
//!
//! While a proc is running, the daemon wants to tell connected browsers how far its
//! recording has gotten so the player can offer a reload (or follow the tail in live
//! mode). The recording is NDJSON appended by asciinema inside the container, so the
//! probe must tolerate a truncated trailing line and must stay cheap enough for the
//! WebSocket tick path: it stats the file and reads only the bytes appended since the
//! last probe (the parse offset is cached per proc), never the whole file per tick.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::jsonio::cast_growth_json;
use super::model::{ProcStatus, Store};

/// Incremental parser state for one proc's asciicast file.
///
/// `scsh` records asciicast **v3** (asciinema CLI 3.x), where each event line is
/// `[<interval-seconds>, "o"|"i"|…, …]` — the available duration is the sum of intervals.
/// The header's `version` is still honored: a legacy v2 cast (absolute timestamps) takes the max.
pub(crate) struct CastProbe {
  /// Bytes parsed so far — always a complete-line boundary; the truncated tail (if any)
  /// is re-read on the next probe once asciinema finishes the line.
  offset: u64,
  /// asciicast format version from the header line: 2 = absolute times, 3 = intervals.
  version: u8,
  duration: f64,
  saw_event: bool,
  /// The duration last handed out by [`CastProbe::take_growth`] — growth is only
  /// announced when the duration advances past it.
  last_sent: Option<f64>,
}

impl Default for CastProbe {
  fn default() -> CastProbe {
    CastProbe { offset: 0, version: 3, duration: 0.0, saw_event: false, last_sent: None }
  }
}

impl CastProbe {
  /// Read and parse whatever the file gained since the last probe. Missing files and I/O
  /// errors are ignored (the recording may not have started yet); a file that shrank was
  /// rewritten, so parsing restarts from the top.
  pub(crate) fn probe(&mut self, path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    let len = meta.len();
    if len < self.offset {
      *self = CastProbe { last_sent: self.last_sent, ..CastProbe::default() };
    }
    if len == self.offset {
      return;
    }
    let Ok(mut file) = std::fs::File::open(path) else { return };
    if file.seek(SeekFrom::Start(self.offset)).is_err() {
      return;
    }
    let mut chunk = Vec::with_capacity((len - self.offset) as usize);
    if file.take(len - self.offset).read_to_end(&mut chunk).is_err() {
      return;
    }
    self.ingest(&chunk);
  }

  /// Parse a chunk that starts at `self.offset`. Only complete lines are consumed; bytes
  /// after the last newline (a partially-flushed event) stay unparsed for the next probe.
  fn ingest(&mut self, chunk: &[u8]) {
    let Some(last_newline) = chunk.iter().rposition(|&b| b == b'\n') else { return };
    let complete = &chunk[..last_newline + 1];
    for line in String::from_utf8_lossy(complete).lines() {
      let line = line.trim();
      if line.starts_with('{') {
        if let Some(v) = header_version(line) {
          self.version = v;
        }
      } else if let Some(t) = event_time(line) {
        self.saw_event = true;
        if self.version == 3 {
          self.duration += t;
        } else {
          self.duration = self.duration.max(t);
        }
      }
    }
    self.offset += complete.len() as u64;
  }

  /// The available duration when it advanced past the last value taken; `None` otherwise
  /// (including before the first complete event line).
  pub(crate) fn take_growth(&mut self) -> Option<f64> {
    if !self.saw_event || self.last_sent.is_some_and(|sent| self.duration <= sent) {
      return None;
    }
    self.last_sent = Some(self.duration);
    Some(self.duration)
  }

  pub(crate) fn duration(&self) -> f64 {
    self.duration
  }
}

/// The `version` field of an asciicast header line, via the crate's JSON parser.
fn header_version(line: &str) -> Option<u8> {
  match crate::json::parse(line).ok()? {
    crate::json::Value::Object(obj) => obj.iter().find(|(k, _)| k == "version").and_then(|(_, v)| match v {
      crate::json::Value::Number(n) => Some(*n as u8),
      _ => None,
    }),
    _ => None,
  }
}

/// The timestamp of one asciicast event line (`[<seconds>, "o", …]`), or `None` for
/// anything that is not an event line.
fn event_time(line: &str) -> Option<f64> {
  let rest = line.strip_prefix('[')?;
  rest.split([',', ']']).next()?.trim().parse::<f64>().ok()
}

/// One tracked proc: `(session id, proc index, cast path, still running)`.
pub(crate) type CastProcSnapshot = (String, usize, String, bool);

/// Snapshot every proc with a registered cast, taken under the store lock so the file
/// probing itself happens with the lock released.
pub(crate) fn cast_probe_snapshot(store: &Store) -> Vec<CastProcSnapshot> {
  let mut out = Vec::new();
  for (id, session) in &store.sessions {
    for proc in &session.procs {
      if let Some(cast_path) = &proc.cast_path {
        out.push((id.clone(), proc.index, cast_path.clone(), proc.status == ProcStatus::Running));
      }
    }
  }
  out
}

/// Probe the snapshot's casts and return the `cast_growth` WebSocket messages to
/// broadcast: one per running proc whose recording grew, plus one final
/// `"running": false` message when a tracked proc stops running (so clients end live
/// mode cleanly). Probes for procs that left the snapshot (evicted sessions) are dropped.
pub(crate) fn probe_growth_messages(
  procs: &[CastProcSnapshot], probes: &mut HashMap<(String, usize), CastProbe>,
) -> Vec<String> {
  let mut out = Vec::new();
  for (session, index, cast_path, running) in procs {
    let key = (session.clone(), *index);
    if *running {
      let probe = probes.entry(key).or_default();
      probe.probe(Path::new(cast_path));
      if let Some(duration) = probe.take_growth() {
        out.push(cast_growth_json(session, *index, duration, true));
      }
    } else if let Some(mut probe) = probes.remove(&key) {
      probe.probe(Path::new(cast_path));
      out.push(cast_growth_json(session, *index, probe.duration(), false));
    }
  }
  probes.retain(|(session, index), _| procs.iter().any(|(s, i, _, _)| s == session && i == index));
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

  fn temp_cast(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("scsh-castprobe-{name}-{}.cast", std::process::id()))
  }

  const HEADER_V3: &str = r#"{"version": 3, "term": {"cols": 200, "rows": 50}}"#;
  const HEADER_V2: &str = r#"{"version": 2, "width": 200, "height": 50}"#;

  #[test]
  fn probe_of_missing_or_empty_file_reports_no_growth() {
    let path = temp_cast("empty");
    let mut probe = CastProbe::default();
    probe.probe(&path); // missing file
    assert_eq!(probe.take_growth(), None);
    std::fs::write(&path, "").unwrap();
    probe.probe(&path); // empty file
    assert_eq!(probe.take_growth(), None);
    std::fs::write(&path, format!("{HEADER_V3}\n")).unwrap();
    probe.probe(&path); // header only — still no events
    assert_eq!(probe.take_growth(), None);
    std::fs::remove_file(&path).unwrap();
  }

  #[test]
  fn probe_tolerates_truncated_trailing_line_and_resumes_from_cached_offset() {
    let path = temp_cast("truncated");
    // v3 intervals: 0.5 then 1.25 → duration 1.75 once the truncated line completes; +3.75 → 5.5.
    std::fs::write(&path, format!("{HEADER_V3}\n[0.5, \"o\", \"a\"]\n[1.25, \"o\", \"tr")).unwrap();
    let mut probe = CastProbe::default();
    probe.probe(&path);
    // Only the complete lines count; the truncated tail is not parsed (and not consumed).
    assert_eq!(probe.take_growth(), Some(0.5));
    // Complete the line and append another; the probe resumes from the cached offset.
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(b"unc\"]\n[3.75, \"o\", \"b\"]\n").unwrap();
    drop(f);
    probe.probe(&path);
    assert_eq!(probe.take_growth(), Some(5.5));
    // No new bytes → no growth, and take_growth stays quiet after the value is taken.
    probe.probe(&path);
    assert_eq!(probe.take_growth(), None);
    std::fs::remove_file(&path).unwrap();
  }

  #[test]
  fn v3_sums_intervals_and_legacy_v2_uses_absolute_times() {
    // scsh records asciicast v3 (interval timestamps → duration is the sum)…
    let mut v3 = CastProbe::default();
    v3.ingest(format!("{HEADER_V3}\n[1.0, \"o\", \"a\"]\n[2.5, \"o\", \"b\"]\n").as_bytes());
    assert_eq!(v3.take_growth(), Some(3.5));
    // …and a legacy v2 header (absolute timestamps) is honored by taking the max.
    let mut v2 = CastProbe::default();
    v2.ingest(format!("{HEADER_V2}\n[1.0, \"o\", \"a\"]\n[2.5, \"o\", \"b\"]\n").as_bytes());
    assert_eq!(v2.take_growth(), Some(2.5));
  }

  #[test]
  fn rewritten_smaller_file_restarts_parsing_without_reannouncing() {
    let path = temp_cast("rewritten");
    std::fs::write(&path, format!("{HEADER_V3}\n[5.0, \"o\", \"a\"]\n")).unwrap();
    let mut probe = CastProbe::default();
    probe.probe(&path);
    assert_eq!(probe.take_growth(), Some(5.0));
    // The file was replaced by a shorter one: parsing restarts, but a smaller duration
    // is not re-announced as growth.
    std::fs::write(&path, format!("{HEADER_V3}\n[2.0, \"o\", \"a\"]\n")).unwrap();
    probe.probe(&path);
    assert_eq!(probe.take_growth(), None);
    std::fs::remove_file(&path).unwrap();
  }

  #[test]
  fn event_time_ignores_non_event_lines() {
    assert_eq!(event_time(r#"[1.5, "o", "x"]"#), Some(1.5));
    assert_eq!(event_time(r#"[ 2.25 , "o", "x"]"#), Some(2.25));
    assert_eq!(event_time(r#"{"version": 3}"#), None);
    assert_eq!(event_time("not json"), None);
    assert_eq!(event_time(""), None);
  }

  #[test]
  fn growth_messages_cover_running_growth_final_notice_and_eviction() {
    let path = temp_cast("messages");
    std::fs::write(&path, format!("{HEADER_V3}\n[0.5, \"o\", \"a\"]\n")).unwrap();
    let cast = path.to_string_lossy().to_string();
    let mut probes = HashMap::new();

    // Running proc with fresh content → one growth message, running:true.
    let snapshot = vec![("sess01".to_string(), 0usize, cast.clone(), true)];
    let msgs = probe_growth_messages(&snapshot, &mut probes);
    assert_eq!(msgs.len(), 1);
    assert_eq!(
      msgs[0],
      r#"{ "type": "cast_growth", "session": "sess01", "proc": 0, "duration": 0.5, "running": true }"#
    );

    // No new bytes → no message.
    assert!(probe_growth_messages(&snapshot, &mut probes).is_empty());

    // The proc finished: one final message with running:false, then the probe is dropped.
    let finished = vec![("sess01".to_string(), 0usize, cast.clone(), false)];
    let msgs = probe_growth_messages(&finished, &mut probes);
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].contains("\"running\": false"));
    assert!(probes.is_empty());
    assert!(probe_growth_messages(&finished, &mut probes).is_empty(), "no repeat final message");

    // A probe whose proc left the snapshot entirely (evicted session) is dropped too.
    let msgs = probe_growth_messages(&snapshot, &mut probes);
    assert_eq!(msgs.len(), 1, "re-tracked after final notice announces current duration");
    assert!(probe_growth_messages(&[], &mut probes).is_empty());
    assert!(probes.is_empty(), "eviction drops the probe");
    std::fs::remove_file(&path).unwrap();
  }

  #[test]
  fn growth_message_for_missing_file_is_suppressed_while_running() {
    let mut probes = HashMap::new();
    let snapshot = vec![("sess02".to_string(), 1usize, "/nonexistent/scsh.cast".to_string(), true)];
    assert!(probe_growth_messages(&snapshot, &mut probes).is_empty());
    assert_eq!(probes.len(), 1, "the probe waits for the file to appear");
  }
}
