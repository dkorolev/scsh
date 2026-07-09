//! Session detail page with per-proc output panels.

use super::escape::esc;
use super::format::format_elapsed_clock;
use super::layout::wrap_page;
use super::proc::{
  autoscroll_ctl_html, cast_embed_html, empty_output_html, proc_elapsed_secs, proc_has_cast, proc_meta_html,
  status_glyph, summary_stats_html,
};
use crate::daemon::model::{Session, Store};
use crate::daemon::paths::{now_unix_secs, session_url};

pub fn session_page(store: &Store, session_id: &str) -> Option<String> {
  let session = store.sessions.get(session_id)?;
  let port = store.port;
  let now = now_unix_secs();
  let mut procs_html = String::new();
  for proc in &session.procs {
    let glyph = status_glyph(proc.status);
    let detail = proc.detail.as_deref().unwrap_or("");
    let elapsed = proc_elapsed_secs(proc, now).map(|e| format_elapsed_clock(e)).unwrap_or_else(|| "—".to_string());
    let note = proc.note.as_deref().unwrap_or("");
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
<span class="triangle" aria-hidden="true"></span><span class="glyph">{glyph}</span>
<span class="label">{label}</span> {proc_stat}
<span class="meta" data-proc-elapsed="{index}">{elapsed}</span>
<span class="note dim">{note}</span>
</summary>
{proc_meta}
<div class="detail">{detail}</div>
{container_line}
{body_html}
</details>
"#,
      status_class = proc.status.as_str(),
      index = proc.index,
      glyph = glyph,
      label = esc(&proc.label),
      proc_stat = summary_stats_html(proc, now),
      proc_meta = proc_meta_html(proc),
      elapsed = elapsed,
      note = esc(note),
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
  let permalink = esc(&session_url(port, &session.id));
  let session_meta = session_meta_placeholder(session);
  let export_btn = if session.procs.iter().any(proc_has_cast) {
    format!(
      "<a class=\"chamfer btn btn--cyan btn--sm session-export\" href=\"/session/{id}/export.html\" download><span>⬇ session .html</span></a>\n",
      id = id
    )
  } else {
    String::new()
  };
  let stop_btn = if session.ended_at.is_none() {
    format!(
      "<button type=\"button\" class=\"chamfer btn btn--red btn--sm\" id=\"session-stop\" data-session=\"{id}\"><span>Force stop</span></button>\n",
      id = id
    )
  } else {
    String::new()
  };
  let body = format!(
    "<h1><a href=\"/\">scsh</a> › session <code>{id}</code></h1>\n\
<p class=\"subtitle\">profile {profile}</p>\n\
<div class=\"session-actions\">{stop_btn}{export_btn}</div>\n\
<div class=\"card card--accent-left-cyan\">{session_meta}\n{skills}</div>\n\
<div class=\"procs\" id=\"session-procs\">\n{procs}</div>\n\
<p class=\"permalink\">Deep link: <a href=\"/session/{id}\">{permalink}</a></p>",
    id = id,
    profile = esc(profile),
    export_btn = export_btn,
    stop_btn = stop_btn,
    session_meta = session_meta,
    skills = skills_html,
    procs = procs_html,
    permalink = permalink
  );
  Some(wrap_page(&format!("session {session_id}"), port, Some(session_id), &body))
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
