//! Proc row snippets for the session detail page.

use super::escape::esc;
use super::format::{format_idle, line_count_label};
use crate::daemon::model::{ProcKind, ProcRecord, ProcStatus};

pub(crate) fn empty_output_label(status: ProcStatus) -> &'static str {
  match status {
    ProcStatus::Ok | ProcStatus::Fail => "No output.",
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

pub(crate) fn status_glyph(status: ProcStatus) -> &'static str {
  match status {
    ProcStatus::Waiting => "○",
    ProcStatus::Running => "◉",
    ProcStatus::Ok => "✓",
    ProcStatus::Fail => "✗",
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
