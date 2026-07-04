//! Cast annotation: turn an asciicast recording into a one-sentence summary and a handful
//! of timestamped chapters, using cursor-agent on the Composer model.
//!
//! Flow: render the asciicast NDJSON to a compact timestamped transcript, hand it to
//! `cursor-agent -p` (headless) with a prompt that asks for strict JSON, validate the reply,
//! and write it as the cast's `.chapters.json` sidecar (see the daemon's chapters endpoint).
//! Best-effort throughout — annotation never fails a run, it just doesn't produce a sidecar.

use std::path::Path;
use std::process::Command;

use crate::json::{self, Value};

/// A validated annotation sidecar: a one-sentence summary and 3-8 ordered chapters.
#[derive(Debug, Clone, PartialEq)]
pub struct CastAnnotation {
  /// One-sentence description of what the recording shows.
  pub summary: String,
  /// Chapter markers, ascending by `t` (seconds into the recording).
  pub chapters: Vec<Chapter>,
}

/// One chapter marker: an offset into the recording and a short title.
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
  pub t: f64,
  pub title: String,
}

impl CastAnnotation {
  /// Serialize to the sidecar JSON the daemon serves and the player reads.
  pub fn to_sidecar_json(&self) -> String {
    let chapters: Vec<String> = self
      .chapters
      .iter()
      .map(|c| format!("{{ \"t\": {}, \"title\": {} }}", fmt_secs(c.t), json::quote(&c.title)))
      .collect();
    format!("{{\n  \"summary\": {},\n  \"chapters\": [{}]\n}}\n", json::quote(&self.summary), chapters.join(", "))
  }
}

fn fmt_secs(t: f64) -> String {
  if t.fract() == 0.0 {
    format!("{}", t as i64)
  } else {
    format!("{t:.1}")
  }
}

/// Strip ANSI/VT control sequences (CSI, OSC, and lone escapes) from `s`, leaving text.
pub fn strip_ansi(s: &str) -> String {
  let bytes = s.as_bytes();
  let mut out = String::with_capacity(s.len());
  let mut i = 0;
  while i < bytes.len() {
    let b = bytes[i];
    if b == 0x1b {
      match bytes.get(i + 1) {
        // CSI: ESC [ ... <final 0x40..0x7e>
        Some(b'[') => {
          i += 2;
          while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
            i += 1;
          }
          i += 1;
        }
        // OSC: ESC ] ... (BEL | ESC \)
        Some(b']') => {
          i += 2;
          while i < bytes.len() && bytes[i] != 0x07 && !(bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\')) {
            i += 1;
          }
          i += if bytes.get(i) == Some(&0x1b) { 2 } else { 1 };
        }
        // Other two-byte escape (charset selection, etc.).
        Some(_) => i += 2,
        None => i += 1,
      }
    } else if b == b'\r' {
      // Carriage returns rewrite the current line; treat as a newline for transcript purposes.
      out.push('\n');
      i += 1;
    } else if b < 0x20 && b != b'\n' && b != b'\t' {
      i += 1; // drop other control bytes
    } else {
      out.push(b as char);
      i += 1;
    }
  }
  out
}

/// Render an asciicast v2 recording (NDJSON) into a compact timestamped transcript:
/// `[<secs>s] visible text`, one line per change, deduped and downsampled to `max_lines`.
/// TUI redraws produce repetitive frames, so consecutive identical lines are collapsed.
pub fn cast_transcript(cast_ndjson: &str, max_lines: usize) -> String {
  let mut events: Vec<(f64, String)> = Vec::new();
  let mut last = String::new();
  for line in cast_ndjson.lines().skip(1) {
    let line = line.trim();
    if line.is_empty() {
      continue;
    }
    let Ok(Value::Array(items)) = json::parse(line) else { continue };
    let (Some(Value::Number(t)), Some(Value::String(code)), Some(Value::String(data))) =
      (items.first(), items.get(1), items.get(2))
    else {
      continue;
    };
    if code != "o" {
      continue;
    }
    for raw in strip_ansi(data).split('\n') {
      let text: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
      if text.is_empty() || text == last {
        continue;
      }
      last = text.clone();
      let clipped: String = text.chars().take(200).collect();
      events.push((*t, clipped));
    }
  }
  // Downsample evenly to at most `max_lines` while keeping chronological order. Timestamps
  // are fractional seconds (not `mm:ss`) so the model can place chapters on sub-second
  // boundaries — chapter `t` is a float.
  let step = if events.len() > max_lines { events.len().div_ceil(max_lines) } else { 1 };
  events.iter().step_by(step).map(|(t, text)| format!("[{t:.1}s] {text}")).collect::<Vec<_>>().join("\n")
}

/// Extract and validate a [`CastAnnotation`] from a cursor-agent reply (which may wrap the
/// JSON in prose or a code fence). Takes the first `{`..last `}` slice and parses it.
pub fn parse_annotation(reply: &str) -> Option<CastAnnotation> {
  let start = reply.find('{')?;
  let end = reply.rfind('}')?;
  if end < start {
    return None;
  }
  let obj = match json::parse(&reply[start..=end]).ok()? {
    Value::Object(o) => o,
    _ => return None,
  };
  let summary = obj.iter().find(|(k, _)| k == "summary").and_then(|(_, v)| match v {
    Value::String(s) => Some(s.trim().to_string()),
    _ => None,
  })?;
  if summary.is_empty() {
    return None;
  }
  let chapters_val = obj.iter().find(|(k, _)| k == "chapters").map(|(_, v)| v);
  let mut chapters = Vec::new();
  if let Some(Value::Array(arr)) = chapters_val {
    for item in arr {
      let Value::Object(fields) = item else { continue };
      let t = fields.iter().find(|(k, _)| k == "t").and_then(|(_, v)| match v {
        Value::Number(n) => Some(*n),
        _ => None,
      });
      let title = fields.iter().find(|(k, _)| k == "title").and_then(|(_, v)| match v {
        Value::String(s) => Some(s.trim().to_string()),
        _ => None,
      });
      if let (Some(t), Some(title)) = (t, title) {
        if !title.is_empty() && t >= 0.0 {
          chapters.push(Chapter { t, title });
        }
      }
    }
  }
  chapters.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
  // Like YouTube, the first chapter always starts at the very beginning, whatever the model
  // guessed for it — otherwise the opening segment has no marker.
  if let Some(first) = chapters.first_mut() {
    first.t = 0.0;
  }
  Some(CastAnnotation { summary, chapters })
}

/// The prompt handed to cursor-agent, embedding the transcript.
fn annotation_prompt(transcript: &str) -> String {
  format!(
    "Below is a timestamped transcript of a terminal-session screen recording (an AI coding \
agent working). Produce a JSON object describing it.\n\n\
Output ONLY the JSON — no prose, no markdown code fence. Schema:\n\
{{\"summary\": \"<one sentence, what the session did>\", \
\"chapters\": [{{\"t\": <seconds into the recording, may be fractional e.g. 12.5>, \"title\": \"<3-6 word phase name>\"}}]}}\n\n\
Use between 3 and 8 chapters, in ascending time order. The FIRST chapter MUST start at t=0 \
(the beginning). Each chapter marks a distinct phase; keep titles terse.\n\n\
TRANSCRIPT:\n{transcript}"
  )
}

/// Whether the host can run the cursor/Composer annotation: the `cursor-agent` binary is on
/// PATH and cursor container auth is configured (same credentials the harness uses).
pub fn host_can_annotate() -> bool {
  crate::runtime::which("cursor-agent").is_some() && crate::runtime::cursor_container_auth_ready()
}

/// Annotate one cast file: render → cursor-agent(Composer) → validated sidecar written next
/// to the cast. Returns the sidecar path on success. Best-effort; `None` on any failure.
/// `run` invokes cursor-agent so tests can stub it; production passes [`run_cursor_agent`].
pub fn annotate_cast_with<R>(cast_path: &Path, model: &str, run: R) -> Option<std::path::PathBuf>
where
  R: FnOnce(&str, &str) -> Option<String>,
{
  let ndjson = std::fs::read_to_string(cast_path).ok()?;
  let transcript = cast_transcript(&ndjson, 120);
  if transcript.trim().is_empty() {
    return None;
  }
  let reply = run(model, &annotation_prompt(&transcript))?;
  let annotation = parse_annotation(&reply)?;
  if annotation.chapters.is_empty() {
    return None;
  }
  let sidecar = crate::daemon::chapters_sidecar_path(&cast_path.to_string_lossy())?;
  std::fs::write(&sidecar, annotation.to_sidecar_json()).ok()?;
  Some(sidecar)
}

/// Run cursor-agent headless on the host with `prompt`, returning its stdout on success.
/// Runs in an empty temp dir (the prompt is self-contained) with a hard timeout via the OS.
pub fn run_cursor_agent(model: &str, prompt: &str) -> Option<String> {
  let dir = std::env::temp_dir().join(format!("scsh-annotate-{}", crate::runtime::random_nonce_6()));
  std::fs::create_dir_all(&dir).ok()?;
  let out = Command::new("cursor-agent")
    .current_dir(&dir)
    .args(["-p", "--force", "--output-format", "text", "--model", model, prompt])
    .output();
  let _ = std::fs::remove_dir_all(&dir);
  let out = out.ok()?;
  if !out.status.success() {
    return None;
  }
  Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn strip_ansi_removes_csi_osc_and_control() {
    assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
    assert_eq!(strip_ansi("a\x1b[Kb"), "ab");
    assert_eq!(strip_ansi("line1\r\nline2"), "line1\n\nline2");
  }

  #[test]
  fn cast_transcript_dedups_and_timestamps() {
    let cast = "{\"version\":2,\"width\":80,\"height\":24}\n\
[0.5, \"o\", \"\\u001b[2Jhello\\r\\n\"]\n\
[1.0, \"o\", \"hello\\r\\n\"]\n\
[65.0, \"o\", \"done\\r\\n\"]\n";
    let t = cast_transcript(cast, 120);
    // Fractional-second timestamps so chapters can be floats.
    assert!(t.contains("[0.5s] hello"), "got: {t}");
    assert!(!t.contains("[1.0s] hello"), "consecutive duplicate dropped: {t}");
    assert!(t.contains("[65.0s] done"), "got: {t}");
  }

  #[test]
  fn parse_annotation_sorts_pins_first_to_zero_and_keeps_floats() {
    let reply = "Sure:\n{\"summary\": \"Ran a build.\", \
\"chapters\": [{\"t\": 8.5, \"title\": \"Finish\"}, {\"t\": 2.3, \"title\": \"Start\"}]}\ndone";
    let a = parse_annotation(reply).unwrap();
    assert_eq!(a.summary, "Ran a build.");
    assert_eq!(a.chapters.len(), 2);
    assert_eq!(a.chapters[0].title, "Start"); // sorted ascending by t
    assert_eq!(a.chapters[0].t, 0.0); // first pinned to the beginning (YouTube-style)
    assert_eq!(a.chapters[1].t, 8.5); // fractional timekey preserved
                                      // Serialization keeps the float and prints 0 for the pinned first chapter.
    let json = a.to_sidecar_json();
    assert!(json.contains("\"t\": 0,"), "got: {json}");
    assert!(json.contains("\"t\": 8.5,"), "got: {json}");
  }

  #[test]
  fn parse_annotation_rejects_missing_summary() {
    assert!(parse_annotation("{\"chapters\": []}").is_none());
    assert!(parse_annotation("no json here").is_none());
  }

  #[test]
  fn annotate_cast_with_stubbed_runner_writes_sidecar() {
    let dir = std::env::temp_dir().join(format!("scsh-annotate-test-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("rec.cast");
    std::fs::write(&cast, "{\"version\":2,\"width\":80,\"height\":24}\n[0.1, \"o\", \"working\\r\\n\"]\n").unwrap();
    let stub =
      |_m: &str, _p: &str| Some("{\"summary\":\"Did work.\",\"chapters\":[{\"t\":0,\"title\":\"Start\"}]}".to_string());
    let side = annotate_cast_with(&cast, "composer-2.5-fast", stub).unwrap();
    assert_eq!(side.file_name().unwrap().to_string_lossy(), "rec.chapters.json");
    let written = std::fs::read_to_string(&side).unwrap();
    assert!(written.contains("\"summary\": \"Did work.\""), "got: {written}");
    assert!(written.contains("\"title\": \"Start\""), "got: {written}");
    let _ = std::fs::remove_dir_all(&dir);
  }
}
