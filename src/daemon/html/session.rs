//! Session detail page with per-proc output panels.

use super::escape::esc;
use super::fleet::fleet_sections_html;
use super::layout::wrap_page;
use super::proc::{
  autoscroll_ctl_html, cast_embed_html, elapsed_phrase, empty_output_html, proc_elapsed_secs, proc_has_cast,
  proc_meta_html, summary_stats_html,
};
use super::workflow::{proc_task_attrs, workflow_graph_html};
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
      r#"<details class="proc {status_class}" data-index="{index}"{task_attrs}>
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
      task_attrs = proc_task_attrs(session, proc),
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
  let session_meta = session_meta_html(session, now);
  let lifecycle = session.lifecycle_status(now);
  // Export sits left of Force stop. While the job is still running the label is
  // "incomplete" so a mid-run download is obviously a partial snapshot.
  let export_btn = if session.procs.iter().any(proc_has_cast) {
    let label = if lifecycle == SessionLifecycle::Running { "incomplete ⬇" } else { "job snapshot ⬇" };
    format!(
      "<a class=\"chamfer btn btn--cyan btn--sm session-export\" href=\"/session/{id}/export.html\" download title=\"Offline HTML snapshot of this job\"><span>{label}</span></a>\n",
      id = id,
      label = label,
    )
  } else {
    String::new()
  };
  // Force stop always occupies its slot (WEB-UI §2): enabled while running, otherwise
  // grayed with an explanation. The resting lifecycle badge still follows the heading.
  let status_chip = if lifecycle == SessionLifecycle::Running {
    String::new()
  } else {
    format!(
      " <span class=\"chamfer session-status {}\"><span>{}</span></span>",
      lifecycle.css_class(),
      esc(lifecycle.label())
    )
  };
  let stop_enabled = lifecycle == SessionLifecycle::Running;
  let stop_btn = format!(
    "<button type=\"button\" class=\"chamfer btn btn--red btn--sm\" id=\"session-stop\" data-session=\"{id}\"{disabled} title=\"{title}\"><span>Force stop</span></button>\n",
    id = id,
    disabled = if stop_enabled { "" } else { " disabled" },
    title = if stop_enabled {
      "Force-stop this job? Running containers will be killed."
    } else {
      // Keep the control visible so it does not read as a missing feature.
      match lifecycle {
        SessionLifecycle::Terminated => "Job terminated abruptly — nothing left to stop",
        SessionLifecycle::Completed => "Job already completed",
        SessionLifecycle::Failed => "Job already failed",
        SessionLifecycle::Cancelled => "Job already cancelled",
        SessionLifecycle::Running => "",
      }
    },
  );
  // The location breadcrumb lives in the top island (see `wrap_page`); the purple island
  // opens with what the session IS — its kind and name — with the controls top-right.
  let kind = session.kind.as_deref().unwrap_or("profile");
  let workflow = workflow_graph_html(session, now);
  let fleets = fleet_sections_html(session);
  let n = session.procs.len();
  let lede = format!(
    "{kind} <strong>{profile}</strong> · {life} · {n} task{plural}.",
    kind = esc(kind),
    profile = esc(profile),
    life = esc(lifecycle.label()),
    n = n,
    plural = if n == 1 { "" } else { "s" },
  );
  let body = format!(
    "<div class=\"card card--accent-left-purple\"><div class=\"session-actions\">{export_btn}{stop_btn}</div>\
<p class=\"session-kind\">{kind} <strong>{profile}</strong>{status_chip}</p>{session_meta}\n{skills}</div>\n\
{workflow}{fleets}<div class=\"procs\" id=\"session-procs\">\n{procs}</div>",
    kind = esc(kind),
    profile = esc(profile),
    export_btn = export_btn,
    status_chip = status_chip,
    stop_btn = stop_btn,
    session_meta = session_meta,
    skills = skills_html,
    workflow = workflow,
    fleets = fleets,
    procs = procs_html,
  );
  Some(wrap_page(&format!("job {session_id}"), port, Some(session_id), &lede, &body))
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

/// A small per-proc "Force stop" button. Always rendered (WEB-UI §2): enabled only while
/// that proc still runs on a live session; otherwise grayed with an explanation so the
/// control does not vanish mid-job.
fn proc_kill_btn_html(session: &Session, now: u64, proc: &crate::daemon::model::ProcRecord) -> String {
  use crate::daemon::model::{ProcStatus, SessionLifecycle};
  let live_session = session.lifecycle_status(now) == SessionLifecycle::Running;
  let live_proc = matches!(proc.status, ProcStatus::Running | ProcStatus::Waiting);
  let enabled = live_session && live_proc;
  let title = if enabled {
    "Force-stop this container only — the rest of the job continues"
  } else if !live_session {
    "Job is no longer running — nothing left to stop"
  } else {
    "This step already finished"
  };
  format!(
    "<button type=\"button\" class=\"proc-kill\" data-proc-stop=\"{index}\" data-session=\"{id}\"{disabled} title=\"{title}\">Force stop</button>",
    index = proc.index,
    id = esc(&session.id),
    disabled = if enabled { "" } else { " disabled" },
    title = esc(title),
  )
}

fn session_meta_html(session: &Session, now: u64) -> String {
  use super::format::format_duration_secs;
  let lifecycle = session.lifecycle_status(now);
  let started = format!("{} UTC", crate::runtime::format_utc_timestamp(session.started_at));
  let ended = match (session.ended_at, lifecycle) {
    (Some(t), _) => format!("{} UTC", crate::runtime::format_utc_timestamp(t)),
    (None, SessionLifecycle::Running) => "still running".into(),
    (None, SessionLifecycle::Terminated) => SessionLifecycle::Terminated.label().into(),
    (None, _) => "—".into(),
  };
  let duration = session.duration_secs(now).map(format_duration_secs).unwrap_or_else(|| "—".into());
  let last_seen = session.last_seen_at;
  format!(
    r#"<dl class="session-meta" id="session-meta"
 data-started="{started_at}" data-ended="{ended_at}" data-last-seen="{last_seen}"
 data-repo="{repo}" data-branch="{branch}">
<dt>Started</dt><dd data-session-started>{started}</dd>
<dt>Ended</dt><dd data-session-ended>{ended}</dd>
<dt>Duration</dt><dd data-session-duration>{duration}</dd>
<dt>Branch</dt><dd data-session-branch><code>{branch}</code></dd>
<dt>Repo</dt><dd data-session-repo><code class="repo-path">{repo}</code></dd>
</dl>"#,
    started_at = session.started_at,
    ended_at = session.ended_at.map(|t| t.to_string()).unwrap_or_default(),
    last_seen = last_seen,
    repo = esc(&session.repo),
    branch = esc(&session.branch),
    started = esc(&started),
    ended = esc(&ended),
    duration = esc(&duration),
  )
}
