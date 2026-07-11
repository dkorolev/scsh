//! Proc row snippets for the session detail page.

use super::escape::esc;
use super::format::{format_elapsed_clock, format_idle, line_count_label};
use crate::daemon::model::{ProcKind, ProcRecord, ProcStatus};

pub(crate) fn empty_output_label(status: ProcStatus) -> &'static str {
  match status {
    ProcStatus::Ok | ProcStatus::Fail | ProcStatus::Skipped => "No output.",
    ProcStatus::Waiting | ProcStatus::Running => "No output yet.",
  }
}

fn proc_is_live(status: ProcStatus) -> bool {
  matches!(status, ProcStatus::Running | ProcStatus::Waiting)
}

pub(crate) fn autoscroll_ctl_html(status: ProcStatus) -> String {
  if proc_is_live(status) {
    r#"<label class="autoscroll-ctl"><input type="checkbox" data-autoscroll checked> Auto-scroll to bottom</label>"#
      .to_string()
  } else {
    String::new()
  }
}

pub(crate) fn empty_output_html(status: ProcStatus) -> String {
  format!("<div class=\"dim\">{}</div>\n", empty_output_label(status))
}

/// Whether this proc has an asciinema recording to embed (skills always; builds when the
/// host recorded the image build under a PTY).
pub(crate) fn proc_has_cast(proc: &ProcRecord) -> bool {
  proc.cast_path.is_some()
}

/// Inline beecast-player embed for a proc's recording: a toolbar (`.cast` + self-contained
/// `.html` downloads) above an empty `.cast-player` box the client JS mounts the player into.
/// Playback chrome — play, chapters, speed, fullscreen, and **● Live** for still-running
/// casts — is the player's own. Works mid-run too: the cast endpoint serves the partial
/// file, and declared-live mode follows the growing tail until the viewer seeks back.
/// The `.html` export link starts hidden; the client JS unhides it once the recording has
/// frames (the export endpoint 404s on a frameless cast). Replaces the text-line output
/// for recorded procs.
pub(crate) fn cast_embed_html(session_id: &str, proc: &ProcRecord) -> String {
  let sid = esc(session_id);
  let idx = proc.index;
  // `data-ended` (unix seconds) tells the client how long ago a finished recording ended,
  // so the "chapters: summarizing…" poll only runs while annotation can still arrive.
  let ended = match (proc.started_at, proc.elapsed) {
    (Some(s), Some(e)) if !proc_is_live(proc.status) => format!(" data-ended=\"{}\"", s + e.round() as u64),
    _ => String::new(),
  };
  let export_label = if proc_is_live(proc.status) { "⬇ incomplete" } else { "⬇ Download run snapshot" };
  format!(
    r#"<div class="cast" data-cast-url="/cast/{sid}/{idx}" data-proc="{idx}" data-status="{status}"{ended}>
<div class="cast-toolbar">
<a href="/cast/{sid}/{idx}/export.html" data-cast-export download hidden>{export_label}</a>
<a href="/cast/{sid}/{idx}?dl=1" download>⬇ .cast</a>
<span class="cast-keys dim">space · ←/→ seek · &lt;/&gt; speed · [/] chapter · c chapters · f fullscreen</span>
</div>
<div class="cast-player"></div>
</div>
"#,
    status = proc.status.as_str(),
    export_label = export_label,
  )
}

pub(crate) fn status_glyph(status: ProcStatus) -> &'static str {
  match status {
    ProcStatus::Waiting => "○",
    ProcStatus::Running => "◉",
    ProcStatus::Ok => "✓",
    ProcStatus::Fail => "✗",
    ProcStatus::Skipped => "⊘",
  }
}

/// Collapsed-row duration phrase: "done in 18s", "running for 12s", "failed in 9s",
/// "stalled after 120s" (inactivity kill), "timed out after …", "waiting", "skipped".
pub(crate) fn elapsed_phrase(status: ProcStatus, elapsed: Option<f64>, fail_reason: Option<&str>) -> String {
  let clock = elapsed.map(format_elapsed_clock);
  match status {
    ProcStatus::Waiting => match clock {
      Some(c) => format!("waiting · {c}"),
      None => "waiting".into(),
    },
    ProcStatus::Running => match clock {
      Some(c) => format!("running for {c}"),
      None => "running".into(),
    },
    ProcStatus::Ok => match clock {
      Some(c) => format!("done in {c}"),
      None => "done".into(),
    },
    ProcStatus::Fail => {
      let prefix = match fail_reason {
        Some(r) if r == crate::failure::reason::FORCE_STOPPED => "force-stopped after",
        Some(r) if r == crate::failure::reason::CONTAINER_INACTIVE => "stalled after",
        Some(r) if r == crate::failure::reason::CONTAINER_TIMEOUT => "timed out after",
        _ => "failed in",
      };
      match clock {
        Some(c) => format!("{prefix} {c}"),
        None => match fail_reason {
          Some(r) if r == crate::failure::reason::FORCE_STOPPED => "force-stopped".into(),
          Some(r) if r == crate::failure::reason::CONTAINER_INACTIVE => "stalled".into(),
          Some(r) if r == crate::failure::reason::CONTAINER_TIMEOUT => "timed out".into(),
          _ => "failed".into(),
        },
      }
    }
    ProcStatus::Skipped => "skipped".into(),
  }
}

fn last_line_at(proc: &ProcRecord) -> f64 {
  proc.lines.iter().map(|l| l.at).fold(0.0, f64::max)
}

pub(crate) fn proc_elapsed_secs(proc: &ProcRecord, now: u64) -> Option<f64> {
  if let Some(e) = proc.elapsed {
    return Some(e);
  }
  if proc.status == ProcStatus::Running {
    return proc.started_at.map(|s| now.saturating_sub(s) as f64);
  }
  None
}

fn idle_since_line(proc: &ProcRecord, now: u64) -> Option<f64> {
  let elapsed = proc_elapsed_secs(proc, now)?;
  Some((elapsed - last_line_at(proc)).max(0.0))
}

pub(crate) fn summary_stats_html(proc: &ProcRecord, now: u64) -> String {
  let idle = idle_since_line(proc, now).map(format_idle).unwrap_or_default();
  format!(
    r#"<span class="proc-stat" data-proc-stat="{index}">
<span class="line-count">{}</span><span class="idle">{idle}</span></span>"#,
    line_count_label(proc.lines.len()),
    index = proc.index,
    idle = idle,
  )
}

pub(crate) fn proc_meta_html(proc: &ProcRecord) -> String {
  match proc.kind {
    ProcKind::Build => {
      let Some(harness) = proc.harness.as_deref().filter(|h| !h.is_empty()) else {
        return String::new();
      };
      format!(
        r#"<div class="proc-meta">
<span><strong>harness</strong> {harness}</span>
<span class="dim">image build</span></div>"#,
        harness = esc(harness)
      )
    }
    ProcKind::Skill => {
      let mut skill_name = proc.skill_name.clone();
      let mut harness = proc.harness.clone();
      if skill_name.is_none() || harness.is_none() {
        if let Some((h, n)) = proc.label.split_once(':') {
          if harness.is_none() {
            harness = Some(h.trim().to_string());
          }
          if skill_name.is_none() {
            skill_name = Some(n.trim().to_string());
          }
        }
      }
      let mut parts = Vec::new();
      if let Some(name) = skill_name {
        parts.push(format!(r#"<span><strong>skill</strong> <code>{name}</code></span>"#, name = esc(&name)));
      }
      if let Some(h) = harness {
        parts.push(format!(r#"<span><strong>harness</strong> {harness}</span>"#, harness = esc(&h)));
      }
      let model = proc
        .model
        .as_deref()
        .map(|m| esc(m))
        .unwrap_or_else(|| r#"<span class="dim">(harness default)</span>"#.to_string());
      parts.push(format!(r#"<span><strong>model</strong> {model}</span>"#));
      if let Some(r) = proc.fail_reason.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!(r#"<span><strong>fail reason</strong> <code>{r}</code></span>"#));
      }
      format!(r#"<div class="proc-meta">{parts}</div>"#, parts = parts.join(" · "))
    }
  }
}

#[cfg(test)]
mod elapsed_phrase_tests {
  use super::elapsed_phrase;
  use crate::daemon::model::ProcStatus;
  use crate::failure::reason;

  #[test]
  fn phrases_match_status() {
    assert_eq!(elapsed_phrase(ProcStatus::Ok, Some(18.0), None), "done in 18s");
    assert_eq!(elapsed_phrase(ProcStatus::Running, Some(12.0), None), "running for 12s");
    assert_eq!(elapsed_phrase(ProcStatus::Fail, Some(9.0), None), "failed in 9s");
    assert_eq!(elapsed_phrase(ProcStatus::Fail, Some(45.0), Some(reason::FORCE_STOPPED)), "force-stopped after 45s");
    assert_eq!(elapsed_phrase(ProcStatus::Fail, None, Some(reason::FORCE_STOPPED)), "force-stopped");
    assert_eq!(elapsed_phrase(ProcStatus::Fail, Some(120.0), Some(reason::CONTAINER_INACTIVE)), "stalled after 120s");
    assert_eq!(elapsed_phrase(ProcStatus::Fail, Some(60.0), Some(reason::CONTAINER_TIMEOUT)), "timed out after 60s");
    assert_eq!(elapsed_phrase(ProcStatus::Waiting, None, None), "waiting");
    assert_eq!(elapsed_phrase(ProcStatus::Skipped, None, None), "skipped");
  }
}
