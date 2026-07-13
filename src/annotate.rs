//! Cast annotation: turn an asciicast recording into a one-sentence summary and a handful
//! of timestamped chapters, using Codex on the fast GPT-5.4 Mini route.
//!
//! Flow: render the asciicast NDJSON to a compact timestamped transcript, hand it to
//! Codex (prefer host tmux + asciinema so the annotate proc has a visual cast;
//! fall back to `codex exec` headless), validate the reply, and write it as the
//! cast's `.chapters.json` sidecar. Best-effort throughout — annotation never fails a run.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::json::{self, Value};

/// Hard cap on one Codex annotation call. Annotation is a known-fast job (seconds),
/// so this bounds a hang well under the §9 five-minute default — a hung external tool must
/// never hang the run.
pub const ANNOTATE_TIMEOUT: Duration = Duration::from_secs(180);

/// Result of a successful annotate: sidecar next to the skill cast, plus an optional
/// recording of the annotate session itself (when tmux + asciinema ran).
#[derive(Debug, Clone)]
pub struct AnnotateResult {
  pub sidecar: PathBuf,
  pub cast_path: Option<PathBuf>,
}

/// Sanity ceilings on an accepted model reply. The prompt asks for 3-8 chapters and terse
/// titles, so a reply blowing past these is malformed (or a runaway model), not a real
/// annotation — [`parse_annotation`] treats it as a parse failure rather than writing a
/// bloated sidecar the player would choke on.
const MAX_CHAPTERS: usize = 100;
/// Byte cap on the summary and each chapter title (a few KB is already absurd for a
/// one-sentence summary or a 3-6 word title).
const MAX_TEXT_BYTES: usize = 4096;

/// Why one annotation attempt produced no sidecar. Annotation stays best-effort (it never
/// fails a run), but the reason is threaded into the daemon Fail row and the CLI output so
/// a user can tell an unreadable cast from a hung model. `Display` gives the `✗` reason;
/// [`AnnotateError::hint`] gives the paired `→` fix.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotateError {
  /// The browser stopped this annotation; the source cast stays untouched.
  Cancelled,
  /// The cast file could not be read from disk.
  UnreadableCast(String),
  /// The recording rendered to an empty transcript — nothing visible to annotate.
  EmptyTranscript,
  /// Codex produced no reply (spawn/exit failure, or a timeout even after a retry).
  ModelFailed(String),
  /// Codex exceeded the annotation wall-clock cap on both attempts.
  ModelTimedOut(String),
  /// The model reply did not contain a valid annotation object.
  UnparseableReply,
  /// The reply parsed, but every chapter was invalid (or none were given).
  NoValidChapters,
  /// The sidecar path could not be derived or written next to the cast.
  WriteFailed(String),
}

impl std::fmt::Display for AnnotateError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      AnnotateError::Cancelled => write!(f, "annotation was stopped; the recording is unchanged"),
      AnnotateError::UnreadableCast(e) => write!(f, "cannot read the cast file ({e})"),
      AnnotateError::EmptyTranscript => write!(f, "the recording has no visible output to annotate"),
      AnnotateError::ModelFailed(detail) => write!(f, "{detail}"),
      AnnotateError::ModelTimedOut(detail) => write!(f, "{detail}"),
      AnnotateError::UnparseableReply => write!(f, "the model reply was not a valid annotation JSON object"),
      AnnotateError::NoValidChapters => write!(f, "the model reply contained no valid chapters"),
      AnnotateError::WriteFailed(e) => write!(f, "cannot write the chapters sidecar ({e})"),
    }
  }
}

impl AnnotateError {
  pub fn failure_reason(&self) -> &'static str {
    match self {
      Self::Cancelled => crate::failure::reason::FORCE_STOPPED,
      Self::ModelTimedOut(_) => crate::failure::reason::ANNOTATION_TIMED_OUT,
      _ => "annotate_failed",
    }
  }

  /// The `→ how to fix` line paired with the `✗` reason in human-facing CLI output.
  pub fn hint(&self) -> &'static str {
    match self {
      AnnotateError::Cancelled => "run `scsh annotate-cast <cast>` again if you want annotations later",
      AnnotateError::UnreadableCast(_) => "check the path and permissions, then re-run `scsh annotate-cast <cast>`",
      AnnotateError::EmptyTranscript => "record a session that produces visible output; an empty cast has no chapters",
      AnnotateError::ModelFailed(_) | AnnotateError::ModelTimedOut(_) => {
        "check `codex` login and network, then re-run `scsh annotate-cast <cast>`"
      }
      AnnotateError::UnparseableReply | AnnotateError::NoValidChapters => {
        "re-run `scsh annotate-cast <cast>`; if it persists, try another model via SCSH_ANNOTATE_MODEL"
      }
      AnnotateError::WriteFailed(_) => "check write permissions in the directory next to the cast file",
    }
  }
}

/// Why one Codex invocation produced no reply. Timeouts are separated from other
/// failures because only a watchdog kill earns a retry (the CLI occasionally stalls
/// on startup and then answers a fresh call within seconds — same policy as seecast).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunFailure {
  /// The child ran past the timeout and was killed.
  TimedOut,
  /// The child failed to spawn, exited non-zero, or could not be waited on.
  Failed,
}

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
  // A non-finite float would render as the bare token `inf`/`NaN`, corrupting the sidecar
  // JSON (the web client's JSON.parse would reject the whole file). Validation upstream
  // rejects non-finite times; this is the last line of defense so `to_sidecar_json` can
  // never emit invalid JSON.
  if !t.is_finite() || t < 0.0 {
    return "0".to_string();
  }
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

/// Render an asciicast recording (NDJSON v2 or v3) into a compact timestamped transcript:
/// `[<secs>s] visible text`, one line per change, deduped and downsampled to `max_lines`.
/// TUI redraws produce repetitive frames, so consecutive identical lines are collapsed.
///
/// Event times are absolute wall-clock seconds from the start of the recording: v2 stamps
/// are already absolute; v3 stamps are intervals and are summed as they are read.
pub fn cast_transcript(cast_ndjson: &str, max_lines: usize) -> String {
  let mut events: Vec<(f64, String)> = Vec::new();
  let mut last = String::new();
  let mut version = 3u8;
  let mut abs_t = 0.0;
  for line in cast_ndjson.lines() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    if line.starts_with('{') {
      if let Ok(Value::Object(obj)) = json::parse(line) {
        if let Some(Value::Number(n)) = obj.iter().find(|(k, _)| k == "version").map(|(_, v)| v) {
          version = *n as u8;
        }
      }
      continue;
    }
    let Ok(Value::Array(items)) = json::parse(line) else { continue };
    let (Some(Value::Number(t)), Some(Value::String(code)), Some(Value::String(data))) =
      (items.first(), items.get(1), items.get(2))
    else {
      continue;
    };
    if code != "o" {
      // Still advance the clock for non-output events so chapter times stay aligned.
      if version == 3 {
        abs_t += *t;
      } else {
        abs_t = abs_t.max(*t);
      }
      continue;
    }
    if version == 3 {
      abs_t += *t;
    } else {
      abs_t = *t;
    }
    for raw in strip_ansi(data).split('\n') {
      let text: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
      if text.is_empty() || text == last {
        continue;
      }
      last = text.clone();
      let clipped: String = text.chars().take(200).collect();
      events.push((abs_t, clipped));
    }
  }
  // Downsample evenly to at most `max_lines` while keeping chronological order. Timestamps
  // are fractional seconds (not `mm:ss`) so the model can place chapters on sub-second
  // boundaries — chapter `t` is a float.
  let step = if events.len() > max_lines { events.len().div_ceil(max_lines) } else { 1 };
  events.iter().step_by(step).map(|(t, text)| format!("[{t:.1}s] {text}")).collect::<Vec<_>>().join("\n")
}

/// True when `fields` repeats a key. Our hand-rolled parser keeps duplicates verbatim and
/// `.find` would silently take the first, so a reply with duplicate keys is ambiguous —
/// callers reject it outright as a parse failure rather than guessing which value wins.
fn has_duplicate_keys(fields: &[(String, Value)]) -> bool {
  fields.iter().enumerate().any(|(i, (k, _))| fields[..i].iter().any(|(prev, _)| prev == k))
}

/// Extract and validate a [`CastAnnotation`] from a model reply (which may wrap the
/// JSON in prose or a code fence). Takes the first `{`..last `}` slice and parses it.
/// Replies with duplicate keys, more than [`MAX_CHAPTERS`] chapters, or summary/title
/// strings over [`MAX_TEXT_BYTES`] are rejected as parse failures.
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
  if has_duplicate_keys(&obj) {
    return None;
  }
  let summary = obj.iter().find(|(k, _)| k == "summary").and_then(|(_, v)| match v {
    Value::String(s) => Some(s.trim().to_string()),
    _ => None,
  })?;
  if summary.is_empty() || summary.len() > MAX_TEXT_BYTES {
    return None;
  }
  let chapters_val = obj.iter().find(|(k, _)| k == "chapters").map(|(_, v)| v);
  let mut chapters = Vec::new();
  if let Some(Value::Array(arr)) = chapters_val {
    // A chapter count wildly past the prompt's 3-8 request is a runaway reply, not a real
    // annotation — reject the whole reply rather than truncating a fabrication.
    if arr.len() > MAX_CHAPTERS {
      return None;
    }
    for item in arr {
      let Value::Object(fields) = item else { continue };
      if has_duplicate_keys(fields) {
        return None;
      }
      let t = fields.iter().find(|(k, _)| k == "t").and_then(|(_, v)| match v {
        Value::Number(n) => Some(*n),
        _ => None,
      });
      let title = fields.iter().find(|(k, _)| k == "title").and_then(|(_, v)| match v {
        Value::String(s) => Some(s.trim().to_string()),
        _ => None,
      });
      if let (Some(t), Some(title)) = (t, title) {
        if title.len() > MAX_TEXT_BYTES {
          return None;
        }
        // `t` must be finite (matching beecast's dto validator): our JSON parser accepts
        // overflow literals like 1e400 as infinity, and a non-finite time would corrupt
        // the sidecar when serialized.
        if !title.is_empty() && t.is_finite() && t >= 0.0 {
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
  // The pin (or a sloppy model) can leave tied timekeys, but the shared sidecar schema
  // requires strictly ascending times — collapse each tie to its first chapter, the same
  // normalization policy as seecast's validator.
  chapters.dedup_by(|later, earlier| later.t <= earlier.t);
  Some(CastAnnotation { summary, chapters })
}

/// The prompt handed to Codex, embedding the transcript. The
/// prompt asks for 3-8 chapters and terse titles; [`parse_annotation`] enforces generous
/// ceilings on top ([`MAX_CHAPTERS`] chapters, [`MAX_TEXT_BYTES`] bytes per summary/title)
/// so a runaway reply is rejected instead of written to disk.
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

/// Whether the host can run Codex annotation: the CLI is on PATH and host auth is ready.
pub fn host_can_annotate() -> bool {
  crate::runtime::which("codex").is_some() && crate::runtime::codex_container_auth_ready()
}

/// Host can record annotate under tmux + asciinema (visual cast for the annotate proc).
pub fn can_record_annotate() -> bool {
  crate::runtime::which("tmux").is_some() && crate::runtime::asciinema_available()
}

/// Whether `cast` still needs annotation given its `sidecar` path: true when no sidecar
/// exists yet, or when the cast was modified after the sidecar was written (a re-recorded
/// cast must not keep the stale annotation of its previous take). Unreadable mtimes fall
/// back to "sidecar exists → already annotated" so a flaky filesystem never loops.
pub fn sidecar_is_stale(cast: &Path, sidecar: &Path) -> bool {
  if !sidecar.exists() {
    return true;
  }
  match (std::fs::metadata(cast).and_then(|m| m.modified()), std::fs::metadata(sidecar).and_then(|m| m.modified())) {
    (Ok(cast_mtime), Ok(sidecar_mtime)) => cast_mtime > sidecar_mtime,
    _ => false,
  }
}

/// Durable opt-out left beside a recording whose run or annotation was force-stopped.
pub fn suppression_marker(cast: &Path) -> std::path::PathBuf {
  let mut marker = cast.as_os_str().to_os_string();
  marker.push(".annotation-suppressed");
  std::path::PathBuf::from(marker)
}

pub fn suppress_automatic_annotation(cast: &Path) {
  let _ = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(suppression_marker(cast));
}

pub fn automatic_annotation_suppressed(cast: &Path) -> bool {
  suppression_marker(cast).exists()
}

/// Annotate one cast file: render → Codex → validated sidecar written next
/// to the cast. Returns the sidecar path on success, or the reason nothing was written —
/// annotation stays best-effort (callers never fail a run over it), but the reason feeds
/// the daemon Fail row and CLI output. A watchdog-killed call is retried ONCE (see
/// [`RunFailure`]). `run` invokes Codex so tests can stub it; production passes
/// [`run_codex`].
pub fn annotate_cast_with<R>(cast_path: &Path, model: &str, mut run: R) -> Result<std::path::PathBuf, AnnotateError>
where
  R: FnMut(&str, &str) -> Result<String, RunFailure>,
{
  let ndjson = std::fs::read_to_string(cast_path).map_err(|e| AnnotateError::UnreadableCast(e.to_string()))?;
  let transcript = cast_transcript(&ndjson, 120);
  if transcript.trim().is_empty() {
    return Err(AnnotateError::EmptyTranscript);
  }
  let prompt = annotation_prompt(&transcript);
  let reply = match run(model, &prompt) {
    Ok(reply) => reply,
    Err(RunFailure::Failed) => return Err(AnnotateError::ModelFailed("Codex exited without a reply".into())),
    // Retry once after a timeout kill: the model CLI occasionally stalls on startup and
    // then answers a fresh call within seconds, so one retry turns a flaky external into
    // a reliable step. Both deaths stay visible in the failure reason.
    Err(RunFailure::TimedOut) => match run(model, &prompt) {
      Ok(reply) => reply,
      Err(RunFailure::TimedOut) => {
        return Err(AnnotateError::ModelTimedOut(format!(
          "Codex hit the {}s timeout twice (retried once after the first kill)",
          ANNOTATE_TIMEOUT.as_secs()
        )));
      }
      Err(RunFailure::Failed) => {
        return Err(AnnotateError::ModelFailed("Codex timed out, and the retry exited without a reply".into()));
      }
    },
  };
  let annotation = parse_annotation(&reply).ok_or(AnnotateError::UnparseableReply)?;
  if annotation.chapters.is_empty() {
    return Err(AnnotateError::NoValidChapters);
  }
  if automatic_annotation_suppressed(cast_path) {
    return Err(AnnotateError::Cancelled);
  }
  let sidecar = crate::daemon::chapters_sidecar_path(&cast_path.to_string_lossy())
    .ok_or_else(|| AnnotateError::WriteFailed("not a .cast path, cannot derive the sidecar name".into()))?;
  crate::atomic_write(&sidecar, annotation.to_sidecar_json().as_bytes())
    .map_err(|e| AnnotateError::WriteFailed(e.to_string()))?;
  Ok(sidecar)
}

/// Production annotate: prefer recorded interactive Codex (tmux + asciinema) when
/// `record_cast` is set and the host can record; otherwise (or when the recorded attempt
/// yields no valid annotation) fall back to headless `-p` with the usual retry-once
/// semantics of [`annotate_cast_with`]. On success the result carries the sidecar and the
/// annotate recording path when one was produced — even a recording of a failed interactive
/// attempt is discarded: a successful fallback must not make a failed recording look
/// like a successful annotator run.
pub fn annotate_cast(
  cast_path: &Path, model: &str, record_cast: Option<&Path>,
) -> Result<AnnotateResult, AnnotateError> {
  let recorded_reply = match record_cast {
    Some(out) if can_record_annotate() => {
      let ndjson = std::fs::read_to_string(cast_path).map_err(|e| AnnotateError::UnreadableCast(e.to_string()))?;
      let transcript = cast_transcript(&ndjson, 120);
      if transcript.trim().is_empty() {
        return Err(AnnotateError::EmptyTranscript);
      }
      run_codex_recorded(model, &annotation_prompt(&transcript), out)
    }
    _ => None,
  };
  if let Some(reply) = recorded_reply {
    if let Some(annotation) = parse_annotation(&reply).filter(|a| !a.chapters.is_empty()) {
      if automatic_annotation_suppressed(cast_path) {
        return Err(AnnotateError::Cancelled);
      }
      let sidecar = crate::daemon::chapters_sidecar_path(&cast_path.to_string_lossy())
        .ok_or_else(|| AnnotateError::WriteFailed("not a .cast path, cannot derive the sidecar name".into()))?;
      crate::atomic_write(&sidecar, annotation.to_sidecar_json().as_bytes())
        .map_err(|e| AnnotateError::WriteFailed(e.to_string()))?;
      return Ok(AnnotateResult { sidecar, cast_path: record_cast.filter(|p| p.is_file()).map(|p| p.to_path_buf()) });
    }
  }
  if let Some(failed_recording) = record_cast {
    let _ = std::fs::remove_file(failed_recording);
  }
  let sidecar = annotate_cast_with(cast_path, model, run_codex)?;
  Ok(AnnotateResult { sidecar, cast_path: None })
}

/// Run Codex headless on the host with `prompt`, returning its final response on success.
/// Runs in an empty temp dir (the prompt is self-contained) and is killed if it runs past
/// [`ANNOTATE_TIMEOUT`] — a hung annotation never stalls the run (§9).
pub fn run_codex(model: &str, prompt: &str) -> Result<String, RunFailure> {
  let dir = std::env::temp_dir().join(format!("scsh-annotate-{}", crate::runtime::random_nonce_6()));
  std::fs::create_dir_all(&dir).map_err(|_| RunFailure::Failed)?;
  let reply_path = dir.join("reply.json");
  let child = Command::new("codex")
    .current_dir(&dir)
    .args([
      "exec",
      "--skip-git-repo-check",
      "--ephemeral",
      "--dangerously-bypass-approvals-and-sandbox",
      "--model",
      model,
      "--output-last-message",
      &reply_path.to_string_lossy(),
      prompt,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn();
  let status = match child {
    Ok(c) => wait_capped(c, ANNOTATE_TIMEOUT).map(|_| ()),
    Err(_) => Err(RunFailure::Failed),
  };
  let result = match status {
    Ok(()) => std::fs::read_to_string(&reply_path).map_err(|_| RunFailure::Failed),
    Err(e) => Err(e),
  };
  let _ = std::fs::remove_dir_all(&dir);
  result
}

/// Non-interactive Codex under host tmux + asciinema. The CLI writes its final response
/// directly to `annotation.json`; no TUI trust or approval prompt can block the harness.
pub fn run_codex_recorded(model: &str, prompt: &str, cast_out: &Path) -> Option<String> {
  if let Some(parent) = cast_out.parent() {
    std::fs::create_dir_all(parent).ok()?;
  }
  let dir = std::env::temp_dir().join(format!("scsh-annotate-rec-{}", crate::runtime::random_nonce_6()));
  std::fs::create_dir_all(&dir).ok()?;
  let term = crate::config::Terminal::default();
  // Keep the name outside the shell script so Rust can always tear the tmux session down,
  // including when `wait_capped_status` has to kill a wedged recorder shell. Killing only
  // that shell is insufficient: tmux is a server and its Codex child outlives it.
  let session = format!("scsh-ann-{}", crate::runtime::random_nonce_6());
  // Shell-quote the full prompt so embedded " / ' survive.
  let agent = recorded_agent_command(model, prompt);
  let script = format!(
    r#"set -eu
cd {dir}
result=annotation.json
rm -f "$result"
session={session}
# Recorded non-interactive execution — the final-response file is the completion signal.
tmux -f /dev/null new-session -d -x {cols} -y {rows} -s "$session" {agent_q}
(
  i=0
  while [ "$i" -lt {secs} ]; do
    if [ -f "$result" ]; then
      sleep 2
      tmux send-keys -t "$session" C-c 2>/dev/null || true
      sleep 1
      tmux send-keys -t "$session" C-c 2>/dev/null || true
      sleep 2
      tmux kill-session -t "$session" 2>/dev/null || true
      exit 0
    fi
    tmux has-session -t "$session" 2>/dev/null || exit 0
    sleep 1
    i=$((i+1))
  done
  tmux kill-session -t "$session" 2>/dev/null || true
) >/dev/null 2>&1 &
asciinema rec -q --overwrite --return --headless -f asciicast-v3 \
  --window-size {cols}x{rows} -c "tmux attach -r -t $session" {cast}
wait || true
tmux kill-session -t "$session" 2>/dev/null || true
"#,
    dir = crate::runtime::shell_quote(&dir.to_string_lossy()),
    session = crate::runtime::shell_quote(&session),
    cols = term.cols,
    rows = term.rows,
    agent_q = crate::runtime::shell_quote(&agent),
    secs = ANNOTATE_TIMEOUT.as_secs(),
    cast = crate::runtime::shell_quote(&cast_out.to_string_lossy()),
  );
  let child =
    Command::new("sh").arg("-c").arg(&script).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
  let ok = child.ok().and_then(|c| wait_capped_status(c, ANNOTATE_TIMEOUT + Duration::from_secs(30)));
  // Belt-and-suspenders cleanup for the timeout/error path. The ordinary watcher already
  // kills this session, so the command is intentionally idempotent and quiet.
  let _ =
    Command::new("tmux").args(["kill-session", "-t", &session]).stdout(Stdio::null()).stderr(Stdio::null()).status();
  let result_path = dir.join("annotation.json");
  let reply = if result_path.is_file() { std::fs::read_to_string(&result_path).ok() } else { None };
  let _ = std::fs::remove_dir_all(&dir);
  if ok.is_none() && reply.is_none() {
    return None;
  }
  reply
}

fn recorded_agent_command(model: &str, prompt: &str) -> String {
  let codex = format!(
    "codex exec --skip-git-repo-check --ephemeral --dangerously-bypass-approvals-and-sandbox \
--model {} --output-last-message annotation.json {}",
    crate::runtime::shell_quote(model),
    crate::runtime::shell_quote(prompt),
  );
  // Codex can think quietly for a while. Keep both the recording and a human observer
  // visibly alive without touching the model's stdin or final-response file.
  let heartbeat = format!(
    r#"{codex} & child=$!; while kill -0 "$child" 2>/dev/null; do printf 'scsh: annotation in progress\n'; sleep 10; done; wait "$child""#
  );
  let agent = format!("sh -c {}", crate::runtime::shell_quote(&heartbeat));
  agent
}

/// Wait for `child`, capturing its stdout, but kill it and report [`RunFailure::TimedOut`]
/// if it runs past `timeout` — the distinction matters because only a timeout kill earns a
/// retry. stdout is drained on a thread so a full pipe buffer can't deadlock the wait.
fn wait_capped(mut child: Child, timeout: Duration) -> Result<String, RunFailure> {
  let mut stdout = child.stdout.take().ok_or(RunFailure::Failed)?;
  let reader = std::thread::spawn(move || {
    let mut buf = String::new();
    let _ = stdout.read_to_string(&mut buf);
    buf
  });
  let deadline = Instant::now() + timeout;
  loop {
    match child.try_wait() {
      Ok(Some(status)) if status.success() => return Ok(reader.join().unwrap_or_default()),
      Ok(Some(_)) => return Err(RunFailure::Failed),
      Ok(None) if Instant::now() >= deadline => {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunFailure::TimedOut);
      }
      Ok(None) => std::thread::sleep(Duration::from_millis(100)),
      Err(_) => return Err(RunFailure::Failed),
    }
  }
}

/// Like [`wait_capped`] but ignores stdout and returns `Some(())` when the process exits
/// (success or not) before the timeout — used for the outer recorder shell.
fn wait_capped_status(mut child: Child, timeout: Duration) -> Option<()> {
  let deadline = Instant::now() + timeout;
  loop {
    match child.try_wait() {
      Ok(Some(_)) => return Some(()),
      Ok(None) if Instant::now() >= deadline => {
        let _ = child.kill();
        let _ = child.wait();
        return None;
      }
      Ok(None) => std::thread::sleep(Duration::from_millis(100)),
      Err(_) => return None,
    }
  }
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
    // v3: intervals sum to absolute times shown in the transcript.
    let cast = "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n\
[0.5, \"o\", \"\\u001b[2Jhello\\r\\n\"]\n\
[0.5, \"o\", \"hello\\r\\n\"]\n\
[64.0, \"o\", \"done\\r\\n\"]\n";
    let t = cast_transcript(cast, 120);
    // Fractional-second timestamps so chapters can be floats.
    assert!(t.contains("[0.5s] hello"), "got: {t}");
    assert!(!t.contains("[1.0s] hello"), "consecutive duplicate dropped: {t}");
    assert!(t.contains("[65.0s] done"), "got: {t}");
  }

  #[test]
  fn cast_transcript_still_reads_legacy_v2_absolute_times() {
    let cast = "{\"version\":2,\"width\":80,\"height\":24}\n\
[0.5, \"o\", \"hello\\r\\n\"]\n\
[65.0, \"o\", \"done\\r\\n\"]\n";
    let t = cast_transcript(cast, 120);
    assert!(t.contains("[0.5s] hello"), "got: {t}");
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
  fn parse_annotation_rejects_non_finite_times_and_sidecar_stays_valid_json() {
    // Our JSON parser accepts overflow literals like 1e400 as f64 infinity; such a chapter
    // must be dropped, never serialized (a bare `inf` token is invalid JSON).
    let reply = "{\"summary\": \"Ran.\", \"chapters\": [\
{\"t\": 1e400, \"title\": \"Overflow\"}, \
{\"t\": -1e400, \"title\": \"NegOverflow\"}, \
{\"t\": 3.5, \"title\": \"Real\"}]}";
    let a = parse_annotation(reply).unwrap();
    assert_eq!(a.chapters.len(), 1);
    assert_eq!(a.chapters[0].title, "Real");
    assert!(json::parse(&a.to_sidecar_json()).is_ok(), "sidecar must be valid JSON: {}", a.to_sidecar_json());
    // Defense in depth: even a hand-built annotation with NaN/infinity times serializes to
    // valid JSON (fmt_secs never emits a non-finite token).
    let bad = CastAnnotation {
      summary: "s".into(),
      chapters: vec![Chapter { t: f64::NAN, title: "a".into() }, Chapter { t: f64::INFINITY, title: "b".into() }],
    };
    let json_text = bad.to_sidecar_json();
    assert!(json::parse(&json_text).is_ok(), "sidecar must be valid JSON: {json_text}");
    assert!(!json_text.contains("inf") && !json_text.contains("NaN"), "got: {json_text}");
  }

  #[test]
  fn parse_annotation_collapses_tied_times_to_strictly_ascending() {
    // Ties from the model (5, 5) and ties created by the t=0 pin (0 vs the sorted first)
    // both collapse to the FIRST chapter of the tie — same policy as seecast's validator.
    let reply = "{\"summary\": \"Ran.\", \"chapters\": [\
{\"t\": 0, \"title\": \"A\"}, {\"t\": 0, \"title\": \"B\"}, \
{\"t\": 5, \"title\": \"C\"}, {\"t\": 5, \"title\": \"D\"}, {\"t\": 7, \"title\": \"E\"}]}";
    let a = parse_annotation(reply).unwrap();
    let titles: Vec<&str> = a.chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, vec!["A", "C", "E"]);
    for pair in a.chapters.windows(2) {
      assert!(pair[0].t < pair[1].t, "times must be strictly ascending: {:?}", a.chapters);
    }
  }

  #[test]
  fn parse_annotation_rejects_duplicate_keys() {
    // Top-level duplicate: `.find` would silently pick the first — ambiguous, so rejected.
    assert!(parse_annotation("{\"summary\": \"a\", \"summary\": \"b\", \"chapters\": []}").is_none());
    // Per-chapter duplicate is rejected the same way.
    let dup_chapter = "{\"summary\": \"s\", \"chapters\": [{\"t\": 1, \"title\": \"x\", \"t\": 2, \"title\": \"y\"}]}";
    assert!(parse_annotation(dup_chapter).is_none());
  }

  #[test]
  fn parse_annotation_rejects_oversized_replies() {
    // More chapters than any sane annotation → parse failure, not truncation.
    let many: Vec<String> = (0..=MAX_CHAPTERS).map(|i| format!("{{\"t\": {i}, \"title\": \"c{i}\"}}")).collect();
    let reply = format!("{{\"summary\": \"s\", \"chapters\": [{}]}}", many.join(", "));
    assert!(parse_annotation(&reply).is_none());
    // Oversized summary and oversized title are parse failures too.
    let long = "x".repeat(MAX_TEXT_BYTES + 1);
    assert!(parse_annotation(&format!("{{\"summary\": \"{long}\", \"chapters\": []}}")).is_none());
    assert!(parse_annotation(&format!("{{\"summary\": \"s\", \"chapters\": [{{\"t\": 0, \"title\": \"{long}\"}}]}}"))
      .is_none());
  }

  #[test]
  fn wait_capped_returns_output_and_kills_on_timeout() {
    // Completes in time → captured stdout.
    let quick =
      Command::new("sh").args(["-c", "printf hello"]).stdin(Stdio::null()).stdout(Stdio::piped()).spawn().unwrap();
    assert_eq!(wait_capped(quick, Duration::from_secs(10)).as_deref().ok(), Some("hello"));
    // A non-zero exit is a plain failure, not a timeout.
    let fails = Command::new("sh").args(["-c", "exit 3"]).stdin(Stdio::null()).stdout(Stdio::piped()).spawn().unwrap();
    assert_eq!(wait_capped(fails, Duration::from_secs(10)), Err(RunFailure::Failed));
    // Runs past the cap → killed, `TimedOut`, and the call returns promptly (not after `sleep 30`).
    let slow = Command::new("sh").args(["-c", "sleep 30"]).stdin(Stdio::null()).stdout(Stdio::piped()).spawn().unwrap();
    let start = Instant::now();
    assert_eq!(wait_capped(slow, Duration::from_millis(300)), Err(RunFailure::TimedOut));
    assert!(start.elapsed() < Duration::from_secs(5), "timed-out child must be killed promptly");
  }

  #[test]
  fn annotate_cast_with_stubbed_runner_writes_sidecar() {
    let dir = std::env::temp_dir().join(format!("scsh-annotate-test-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("rec.cast");
    std::fs::write(&cast, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.1, \"o\", \"working\\r\\n\"]\n")
      .unwrap();
    let stub =
      |_m: &str, _p: &str| Ok("{\"summary\":\"Did work.\",\"chapters\":[{\"t\":0,\"title\":\"Start\"}]}".to_string());
    let side = annotate_cast_with(&cast, "composer-2.5-fast", stub).unwrap();
    assert_eq!(side.file_name().unwrap().to_string_lossy(), "rec.chapters.json");
    let written = std::fs::read_to_string(&side).unwrap();
    assert!(written.contains("\"summary\": \"Did work.\""), "got: {written}");
    assert!(written.contains("\"title\": \"Start\""), "got: {written}");
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn stopped_annotation_never_writes_a_sidecar() {
    let dir = std::env::temp_dir().join(format!("scsh-annotate-stop-test-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("rec.cast");
    std::fs::write(&cast, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.1, \"o\", \"working\\r\\n\"]\n")
      .unwrap();
    let target = cast.clone();
    let reply = move |_m: &str, _p: &str| {
      suppress_automatic_annotation(&target);
      Ok("{\"summary\":\"Did work.\",\"chapters\":[{\"t\":0,\"title\":\"Start\"}]}".to_string())
    };
    assert_eq!(annotate_cast_with(&cast, "m", reply), Err(AnnotateError::Cancelled));
    assert!(automatic_annotation_suppressed(&cast));
    assert!(!dir.join("rec.chapters.json").exists(), "cancellation leaves the recording without annotations");
    assert!(cast.is_file(), "the source recording is untouched");
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn wait_capped_status_returns_and_times_out() {
    let quick = Command::new("sh").args(["-c", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap();
    assert_eq!(wait_capped_status(quick, Duration::from_secs(10)), Some(()));
    let slow = Command::new("sh").args(["-c", "sleep 30"]).stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap();
    let start = Instant::now();
    assert_eq!(wait_capped_status(slow, Duration::from_millis(300)), None);
    assert!(start.elapsed() < Duration::from_secs(5));
  }

  #[test]
  fn recorded_annotation_emits_a_quiet_work_heartbeat() {
    let command = recorded_agent_command("gpt-5.4-mini", "summarize");
    assert!(command.contains("scsh: annotation in progress"), "{command}");
    assert!(command.contains("sleep 10"), "heartbeat stays well inside the daemon's 30s stale window: {command}");
    assert!(command.contains("wait \"$child\""), "heartbeat wrapper preserves the Codex exit status: {command}");
  }

  #[test]
  fn annotate_cast_with_reports_distinct_failure_reasons() {
    let dir = std::env::temp_dir().join(format!("scsh-annotate-test-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let ok_reply = |_m: &str, _p: &str| Ok("irrelevant".to_string());
    // Unreadable cast.
    let missing = dir.join("missing.cast");
    assert!(matches!(annotate_cast_with(&missing, "m", ok_reply), Err(AnnotateError::UnreadableCast(_))));
    // Empty transcript: a header-only cast renders to nothing.
    let empty = dir.join("empty.cast");
    std::fs::write(&empty, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n").unwrap();
    assert_eq!(annotate_cast_with(&empty, "m", ok_reply), Err(AnnotateError::EmptyTranscript));
    // Unparseable reply and a reply with zero valid chapters are distinct reasons.
    let cast = dir.join("rec.cast");
    std::fs::write(&cast, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.1, \"o\", \"working\\r\\n\"]\n")
      .unwrap();
    let prose = |_m: &str, _p: &str| Ok("no json at all".to_string());
    assert_eq!(annotate_cast_with(&cast, "m", prose), Err(AnnotateError::UnparseableReply));
    let no_chapters = |_m: &str, _p: &str| Ok("{\"summary\": \"s\", \"chapters\": []}".to_string());
    assert_eq!(annotate_cast_with(&cast, "m", no_chapters), Err(AnnotateError::NoValidChapters));
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn annotate_cast_with_retries_once_after_a_timeout_kill() {
    let dir = std::env::temp_dir().join(format!("scsh-annotate-test-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("rec.cast");
    std::fs::write(&cast, "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.1, \"o\", \"working\\r\\n\"]\n")
      .unwrap();
    // First call times out, the retry answers — annotation succeeds on exactly two calls.
    let mut calls = 0;
    let flaky = |_m: &str, _p: &str| {
      calls += 1;
      if calls == 1 {
        Err(RunFailure::TimedOut)
      } else {
        Ok("{\"summary\":\"s\",\"chapters\":[{\"t\":0,\"title\":\"Start\"}]}".to_string())
      }
    };
    assert!(annotate_cast_with(&cast, "m", flaky).is_ok());
    assert_eq!(calls, 2, "a timeout kill must be retried exactly once");
    // Both attempts time out — the failure reason says the retry happened.
    let mut dead_calls = 0;
    let dead = |_m: &str, _p: &str| {
      dead_calls += 1;
      Err(RunFailure::TimedOut)
    };
    let err = annotate_cast_with(&cast, "m", dead).unwrap_err();
    assert_eq!(dead_calls, 2);
    assert!(matches!(&err, AnnotateError::ModelTimedOut(d) if d.contains("retried once")), "got: {err}");
    assert_eq!(err.failure_reason(), crate::failure::reason::ANNOTATION_TIMED_OUT);
    // A plain (non-timeout) failure is NOT retried.
    let mut plain_calls = 0;
    let plain = |_m: &str, _p: &str| {
      plain_calls += 1;
      Err(RunFailure::Failed)
    };
    assert!(annotate_cast_with(&cast, "m", plain).is_err());
    assert_eq!(plain_calls, 1, "a non-timeout failure must not be retried");
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn sidecar_is_stale_tracks_missing_and_outdated_sidecars() {
    let dir = std::env::temp_dir().join(format!("scsh-annotate-test-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("rec.cast");
    let sidecar = dir.join("rec.chapters.json");
    std::fs::write(&cast, "cast").unwrap();
    // No sidecar yet → needs annotation.
    assert!(sidecar_is_stale(&cast, &sidecar));
    // Sidecar written after the cast → fresh.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&sidecar, "{}").unwrap();
    assert!(!sidecar_is_stale(&cast, &sidecar));
    // Cast re-recorded after the sidecar → the annotation is stale again.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&cast, "cast v2").unwrap();
    assert!(sidecar_is_stale(&cast, &sidecar));
    let _ = std::fs::remove_dir_all(&dir);
  }
}
