//! Session detail page with per-proc output panels.

use super::escape::esc;
use super::fleet::fleet_sections_html;
use super::layout::wrap_page;
use super::proc::{
  autoscroll_ctl_html, cast_embed_html, elapsed_phrase, empty_output_html, proc_elapsed_secs, proc_has_cast,
  proc_meta_html, summary_stats_html,
};
use crate::daemon::model::{ProcStatus, Session, SessionLifecycle, Store};
use crate::daemon::paths::now_unix_secs;

pub fn session_page(store: &Store, session_id: &str) -> Option<String> {
  let session = store.sessions.get(session_id)?;
  let port = store.port;
  let now = now_unix_secs();
  let mut procs_html = String::new();
  for proc in &session.procs {
    let detail = proc.detail.as_deref().unwrap_or("");
    let elapsed = elapsed_phrase(proc.status, proc_elapsed_secs(proc, now), proc.fail_reason.as_deref());
    // The collapsed row's trailing text: once the proc FINISHED we know its answer (the
    // finish detail — a result message like "2 + 3 = 5"), so show that; the transient
    // "<harness> run…" note is only for rows still working. A bare artifact path is SYSTEM
    // info and renders as code, so the eye can tell it from an agent's prose answer.
    let finished = !matches!(proc.status, ProcStatus::Running | ProcStatus::Waiting);
    let note = if finished && !detail.is_empty() { detail } else { proc.note.as_deref().unwrap_or("") };
    let note_html =
      if finished && looks_like_artifact_path(note) { format!("<code>{}</code>", esc(note)) } else { esc(note) };
    let container = proc.container_name.as_deref().unwrap_or("");
    // Recorded procs (skills and TUI image builds) show the inline cast player; text-only
    // build fallbacks (no asciinema on PATH) keep the timestamped output with auto-scroll.
    let body_html = if proc_has_cast(proc) {
      cast_embed_html(&session.id, proc)
    } else {
      let mut lines_html = String::new();
      for line in &proc.lines {
        lines_html.push_str(&format!(
          "<div class=\"line\"><span class=\"at\">+{at:.1}s</span> {text}</div>\n",
          at = line.at,
          text = esc(&line.text)
        ));
      }
      if lines_html.is_empty() {
        lines_html = empty_output_html(proc.status);
      }
      format!("{}<div class=\"output\">{lines_html}</div>", autoscroll_ctl_html(proc.status))
    };
    procs_html.push_str(&format!(
      r#"<details class="proc {status_class}" data-index="{index}">
<summary>
<span class="triangle" aria-hidden="true"></span>
<span class="label">{label}</span> {proc_stat}
<span class="meta" data-proc-elapsed="{index}">{elapsed}</span>
<span class="note dim">{note}</span>
{diff_btn}{kill_btn}</summary>
{proc_meta}
<div class="detail">{detail}</div>
{container_line}
{body_html}
</details>
"#,
      status_class = proc.status.as_str(),
      index = proc.index,
      label = esc(&proc.label),
      proc_stat = summary_stats_html(proc, now),
      diff_btn = proc_diff_btn_html(&session.id, proc),
      kill_btn = proc_kill_btn_html(session, now, proc),
      proc_meta = proc_meta_html(proc),
      elapsed = esc(&elapsed),
      note = note_html,
      detail = esc(detail),
      container_line = if container.is_empty() {
        String::new()
      } else {
        format!("<div class=\"container dim\">container: {c}</div>\n", c = esc(container))
      },
      body_html = body_html
    ));
  }
  let profile = session.profile.as_deref().unwrap_or("default");
  let skills_html = if session.skills.is_empty() {
    String::new()
  } else {
    let items: Vec<String> = session
      .skills
      .iter()
      .map(|sk| format!("<li><code>{}</code> <span class=\"dim\">({})</span></li>", esc(&sk.name), esc(&sk.harness)))
      .collect();
    format!("<ul class=\"skills\">{}</ul>\n", items.join(""))
  };
  let id = esc(&session.id);
  let session_meta = session_meta_placeholder(session);
  // Same style as the per-run "⬇ Download run snapshot" in the cast toolbar (.dl-snap):
  // the two downloads read as one family, not a chamfered button next to a plain link.
  let export_btn = if session.procs.iter().any(proc_has_cast) {
    format!(
      "<a class=\"dl-snap session-export\" href=\"/session/{id}/export.html\" download>⬇ Download job snapshot</a>\n",
      id = id
    )
  } else {
    String::new()
  };
  // While the session genuinely runs, offer Force stop top-right; once it is over (ended,
  // or a dead client's Terminated zombie), the resting lifecycle badge follows the heading
  // instead — the kind/name stays flush-left with the meta labels below it, the status
  // reads right after it, and neither sits beside the taller download button.
  let lifecycle = session.lifecycle_status(now);
  let (status_chip, stop_btn) = if lifecycle == SessionLifecycle::Running {
    (
      String::new(),
      format!(
        "<button type=\"button\" class=\"chamfer btn btn--red btn--sm\" id=\"session-stop\" data-session=\"{id}\"><span>Force stop</span></button>\n",
        id = id
      ),
    )
  } else {
    (
      format!(
        " <span class=\"chamfer session-status {}\"><span>{}</span></span>",
        lifecycle.css_class(),
        esc(lifecycle.label())
      ),
      String::new(),
    )
  };
  // The location breadcrumb lives in the top island (see `wrap_page`); the purple island
  // opens with what the session IS — its kind and name — with the controls top-right.
  let kind = session.kind.as_deref().unwrap_or("profile");
  let fleets = fleet_sections_html(session);
  let body = format!(
    "<div class=\"card card--accent-left-purple\"><div class=\"session-actions\">{stop_btn}{export_btn}</div>\
<p class=\"session-kind\">{kind} <strong>{profile}</strong>{status_chip}</p>{session_meta}\n{skills}</div>\n\
{fleets}<div class=\"procs\" id=\"session-procs\">\n{procs}</div>",
    kind = esc(kind),
    profile = esc(profile),
    export_btn = export_btn,
    status_chip = status_chip,
    stop_btn = stop_btn,
    session_meta = session_meta,
    skills = skills_html,
    fleets = fleets,
    procs = procs_html,
  );
  Some(wrap_page(&format!("job {session_id}"), port, Some(session_id), &body))
}

/// A bare repo-relative artifact path (`tmp/scsh/<id>/add.json`-shaped) — system info, not
/// an agent's prose. Mirrored by `looksLikeArtifactPath` in the client JS.
fn looks_like_artifact_path(text: &str) -> bool {
  !text.is_empty()
    && (text.starts_with('/') || text.starts_with("tmp/") || text.starts_with(".harness/"))
    && !text.contains(char::is_whitespace)
}

/// A "⇄ commits diff" chip on a step's summary row, shown once the run integrated this
/// step's commits into the caller's branch and packed them (packdiff) into a review page.
/// Navigates to `/diff/<session>/<proc>` in THIS tab (cmd/ctrl+click for a new one — no
/// `target` override); the page is one self-contained HTML file. Mirrored by
/// `procDiffBtnHtml` in the client JS (the chip appears live: integration happens after
/// the step finished, so it lands on a late tick of the run).
fn proc_diff_btn_html(session_id: &str, proc: &crate::daemon::model::ProcRecord) -> String {
  if proc.diff_path.is_none() {
    return String::new();
  }
  format!(
    "<a class=\"proc-diff\" data-proc-diff href=\"/diff/{id}/{index}\" title=\"Browse the commits this step brought into your branch — one self-contained review page\">⇄ commits diff</a>",
    index = proc.index,
    id = esc(session_id),
  )
}

/// A small per-proc "✕ Force stop" button, shown only while that proc still runs: it stops
/// just this container (`POST /api/v1/proc/stop`) — unlike the job-level Force stop, the
/// rest of the job keeps going.
fn proc_kill_btn_html(session: &Session, now: u64, proc: &crate::daemon::model::ProcRecord) -> String {
  use crate::daemon::model::{ProcStatus, SessionLifecycle};
  // A dead client's session keeps its procs "running" forever; only a genuinely live
  // session (recent ping) has containers a kill button could reach.
  if session.lifecycle_status(now) != SessionLifecycle::Running {
    return String::new();
  }
  if proc.status != ProcStatus::Running && proc.status != ProcStatus::Waiting {
    return String::new();
  }
  format!(
    "<button type=\"button\" class=\"proc-kill\" data-proc-stop=\"{index}\" data-session=\"{id}\" title=\"Force-stop this container only — the rest of the job continues\">✕ Force stop</button>",
    index = proc.index,
    id = esc(&session.id),
  )
}

fn session_meta_placeholder(session: &Session) -> String {
  let ended = session.ended_at.map(|t| t.to_string()).unwrap_or_default();
  format!(
    r#"<dl class="session-meta" id="session-meta"
 data-started="{started}" data-ended="{ended}"
 data-repo="{repo}" data-branch="{branch}"></dl>"#,
    started = session.started_at,
    ended = esc(&ended),
    repo = esc(&session.repo),
    branch = esc(&session.branch),
  )
}
