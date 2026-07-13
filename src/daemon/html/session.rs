//! Session detail page with per-proc output panels.

use super::escape::esc;
use super::fleet::fleet_sections_html;
use super::layout::wrap_page;
use super::proc::{
  cast_embed_html, elapsed_phrase, proc_elapsed_secs, proc_has_cast, proc_meta_html, summary_stats_html,
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
    // build fallbacks (no asciinema on PATH) keep the timestamped output (sticky follow).
    // A proc with neither a recording nor a single log line — annotate rows without a
    // recording are the canonical case — stays a slim summary-only row: terminal chrome
    // belongs to procs that actually stream output.
    let body_html = if proc_has_cast(proc) {
      cast_embed_html(&session.id, proc)
    } else if proc.lines.is_empty() {
      String::new()
    } else {
      let mut lines_html = String::new();
      for line in &proc.lines {
        lines_html.push_str(&format!(
          "<div class=\"line\"><span class=\"at\">+{at:.1}s</span> {text}</div>\n",
          at = line.at,
          text = esc(&line.text)
        ));
      }
      format!("<div class=\"output\">{lines_html}</div>")
    };
    let snapshot_btn = proc_snapshot_btn_html(&session.id, proc);
    let diff_btn = proc_diff_btn_html(&session.id, proc);
    let annotation_target = annotation_target_link_html(session, proc);
    procs_html.push_str(&format!(
      r#"<details class="proc {status_class}" id="proc-{index}" data-index="{index}"{task_attrs}>
<div class="proc-actions">{diff_btn}{snapshot_btn}{kill_btn}</div>
<summary>
<span class="triangle" aria-hidden="true"></span>
<span class="label">{label}</span> {proc_stat}
<span class="meta" data-proc-elapsed="{index}">{elapsed}</span>
<span class="note dim">{note}</span>
{annotation_target}
</summary>
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
      diff_btn = diff_btn,
      annotation_target = annotation_target,
      snapshot_btn = snapshot_btn,
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
  let id = esc(&session.id);
  let session_meta = session_meta_html(session, now);
  let lifecycle = session.lifecycle_status(now);
  let pending = chapters_pending_count(session);
  // Snapshot sits above Force stop in the island’s top-right. Mid-run → incomplete;
  // finished but chapters still landing → chapters pending; else job snapshot.
  let label = session_export_label(lifecycle, pending);
  let export_btn = format!(
    "<a class=\"chamfer btn btn--cyan btn--sm session-export\" href=\"/job/{id}/export.html\" download title=\"Offline HTML snapshot of this entire job\"><span>{label}</span></a>\n",
    id = id,
    label = label,
  );
  // Force stop only while the job is running — hide it otherwise. A control that can
  // never act again is noise, not a missing feature: the lifecycle badge already says
  // completed / failed / cancelled. (Deliberate departure from the WEB-UI §2 gray-in-place
  // rule; the offline export strips the whole actions island the same way.)
  let stop_btn = if lifecycle == SessionLifecycle::Running {
    format!(
      "<button type=\"button\" class=\"chamfer btn btn--red btn--sm\" id=\"session-stop\" data-session=\"{id}\" title=\"Force-stop this job? Running containers will be killed.\"><span>Force stop</span></button>\n",
      id = id,
    )
  } else {
    String::new()
  };
  let workflow = workflow_graph_html(session, now);
  let fleets = fleet_sections_html(session);
  let lede = session_lede_html(session, lifecycle);
  let chapters_pending = chapters_pending_html(pending);
  let job_diff_btn = if session.procs.iter().any(|proc| proc.diff_path.is_some()) {
    format!(
      "<a class=\"chamfer btn btn--purple btn--sm job-diff\" data-job-diff href=\"/diff/{}/all\" title=\"Browse the entire end-to-end commits diff\"><span>⇄ all commits</span></a>",
      esc(&session.id)
    )
  } else {
    String::new()
  };
  let body = format!(
    "<div class=\"card card--accent-left-purple\"><div class=\"session-actions\">{job_diff_btn}{export_btn}{stop_btn}</div>\
{session_meta}\n{chapters_pending}</div>\n\
{workflow}{fleets}<div class=\"procs\" id=\"session-procs\">\n{procs}</div>",
    export_btn = export_btn,
    job_diff_btn = job_diff_btn,
    stop_btn = stop_btn,
    session_meta = session_meta,
    chapters_pending = chapters_pending,
    workflow = workflow,
    fleets = fleets,
    procs = procs_html,
  );
  Some(wrap_page(&format!("job {session_id}"), port, Some(session_id), &lede, &body))
}

fn annotation_target_link_html(
  session: &crate::daemon::model::Session, proc: &crate::daemon::model::ProcRecord,
) -> String {
  if proc.kind != crate::daemon::model::ProcKind::Annotate {
    return String::new();
  }
  let Some(target) = proc.annotate_target.as_deref() else {
    return String::new();
  };
  let target_name = std::path::Path::new(target).file_name().and_then(|s| s.to_str()).unwrap_or("source recording");
  let href = session
    .procs
    .iter()
    .find(|candidate| candidate.cast_path.as_deref() == Some(target))
    .map(|candidate| format!("/job/{}#proc-{}", esc(&session.id), candidate.index))
    .or_else(|| session.parent_session.as_ref().map(|parent| format!("/job/{}", esc(parent))))
    .unwrap_or_else(|| format!("/job/{}", esc(&session.id)));
  format!(
    "<a class=\"annotation-target\" href=\"{href}\" title=\"Recording being annotated: {target}\">↩ source run</a>",
    target = esc(target_name)
  )
}

/// The one-line page lede shared by the live job page and the offline export: kind,
/// profile, lifecycle, and task count — enough to tell at a glance what ran and whether
/// it succeeded.
pub(crate) fn session_lede_html(session: &Session, lifecycle: SessionLifecycle) -> String {
  let kind = session.kind.as_deref().unwrap_or("profile");
  let profile = session.profile.as_deref().unwrap_or("default");
  let n = session.procs.len();
  format!(
    "{kind} <strong>{profile}</strong> · {life} · {n} task{plural}.",
    kind = esc(kind),
    profile = esc(profile),
    life = esc(lifecycle.label()),
    n = n,
    plural = if n == 1 { "" } else { "s" },
  )
}

/// The human "Ended" cell shared by the live meta and the offline export: the wall-clock
/// end when known, "still running" while live, and the last heartbeat for a terminated
/// (heartbeat-stale) session — when we last heard from the run is its effective end.
pub(crate) fn session_ended_text(session: &Session, lifecycle: SessionLifecycle) -> String {
  match (session.ended_at, lifecycle) {
    (Some(t), _) => format!("{} UTC", crate::runtime::format_utc_timestamp(t)),
    (None, SessionLifecycle::Running) => "still running".into(),
    (None, SessionLifecycle::Terminated) => {
      format!("{} UTC", crate::runtime::format_utc_timestamp(session.last_seen_at))
    }
    (None, _) => "—".into(),
  }
}

/// Non-annotate procs that have a cast but no chapters sidecar yet.
pub(crate) fn chapters_pending_count(session: &Session) -> usize {
  use crate::daemon::model::ProcKind;
  session
    .procs
    .iter()
    .filter(|p| {
      if p.kind == ProcKind::Annotate {
        return false;
      }
      let Some(cast) = p.cast_path.as_deref() else {
        return false;
      };
      match crate::daemon::chapters_sidecar_path(cast) {
        Some(path) => !path.exists(),
        None => false,
      }
    })
    .count()
}

fn session_export_label(lifecycle: SessionLifecycle, pending: usize) -> &'static str {
  if lifecycle == SessionLifecycle::Running {
    "Incomplete job ⬇"
  } else if pending > 0 {
    "Chapters pending ⬇"
  } else {
    "Job snapshot ⬇"
  }
}

fn chapters_pending_html(pending: usize) -> String {
  if pending == 0 {
    return String::new();
  }
  format!(
    r#"<p class="chapters-pending dim" id="chapters-pending" data-pending="{n}">{n} cast{plural} finalizing chapters</p>"#,
    n = pending,
    plural = if pending == 1 { "" } else { "s" },
  )
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
    "<a class=\"chamfer btn btn--purple btn--sm proc-diff\" data-proc-diff href=\"/diff/{id}/{index}\" title=\"Browse the commits this step brought into your branch — one self-contained review page\"><span>⇄ commits diff</span></a>",
    index = proc.index,
    id = esc(session_id),
  )
}

/// Per-proc offline snapshot link (top-right of the proc island, above Force stop).
/// Hidden until the recording has frames — the export endpoint 404s on a frameless cast.
fn proc_snapshot_btn_html(session_id: &str, proc: &crate::daemon::model::ProcRecord) -> String {
  if !proc_has_cast(proc) {
    return String::new();
  }
  let live = matches!(proc.status, ProcStatus::Running | ProcStatus::Waiting);
  let label = if live { "Incomplete run ⬇" } else { "Run snapshot ⬇" };
  format!(
    r#"<a class="chamfer btn btn--cyan btn--sm proc-snapshot" href="/cast/{sid}/{idx}/export.html" data-cast-export download hidden title="Offline HTML snapshot of this run"><span>{label}</span></a>"#,
    sid = esc(session_id),
    idx = proc.index,
    label = label,
  )
}

/// A small per-proc "Force stop" button. Only rendered while that proc still runs on a
/// live session — finished/zombie steps omit it (no grayed-out stub).
fn proc_kill_btn_html(session: &Session, now: u64, proc: &crate::daemon::model::ProcRecord) -> String {
  use crate::daemon::model::{ProcStatus, SessionLifecycle};
  let live_session = session.lifecycle_status(now) == SessionLifecycle::Running;
  let live_proc = matches!(proc.status, ProcStatus::Running | ProcStatus::Waiting);
  if !(live_session && live_proc) {
    return String::new();
  }
  format!(
    "<button type=\"button\" class=\"chamfer btn btn--red btn--sm proc-kill\" data-proc-stop=\"{index}\" data-session=\"{id}\" title=\"Force-stop this container only — the rest of the job continues\"><span>Force stop</span></button>",
    index = proc.index,
    id = esc(&session.id),
  )
}

fn session_meta_html(session: &Session, now: u64) -> String {
  use super::format::format_duration_secs;
  let lifecycle = session.lifecycle_status(now);
  let started = format!("{} UTC", crate::runtime::format_utc_timestamp(session.started_at));
  let ended = session_ended_text(session, lifecycle);
  let duration = session.duration_secs(now).map(format_duration_secs).unwrap_or_else(|| "—".into());
  let last_seen = session.last_seen_at;
  format!(
    r#"<dl class="session-meta" id="session-meta"
 data-started="{started_at}" data-ended="{ended_at}" data-last-seen="{last_seen}"
 data-repo="{repo}" data-branch="{branch}">
<dt>Started</dt><dd data-session-started>{started}</dd>
<dt>Ended</dt><dd data-session-ended>{ended}</dd>
<dt>Duration</dt><dd data-session-duration>{duration}</dd>
<dt>Repo</dt><dd data-session-repo><code class="repo-path">{repo}</code></dd>
<dt>Branch</dt><dd data-session-branch><code>{branch}</code></dd>
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
