//! Session index table page.

use super::escape::esc;
use super::format::{format_duration_secs, format_relative_age};
use super::layout::wrap_page;
use crate::daemon::model::{sessions_for_index, Session, SessionLifecycle, Store};
use crate::daemon::paths::{base_url, now_unix_secs};

pub fn index_page(store: &Store) -> String {
  let port = store.port;
  let now = now_unix_secs();
  let sessions = sessions_for_index(&store.sessions, now);
  let mut rows = String::new();
  for session in sessions {
    rows.push_str(&index_session_row(session, now));
  }
  if rows.is_empty() {
    rows = "<tr><td colspan=\"7\" class=\"dim\">No sessions yet — run <code>scsh run</code> to create one.</td></tr>\n"
      .to_string();
  }
  let body = format!(
    "<h1>scsh session browser</h1>\n\
<p class=\"dim\">Daemon on {url} · mode {mode}</p>\n\
<div class=\"table-scroll\"><table>\n\
<thead><tr><th>Session</th><th>Status</th><th>Started</th><th>Duration</th>\
<th>Profile</th><th>Procs</th><th>Repo</th></tr></thead>\n\
<tbody id=\"sessions-body\">\n{rows}</tbody>\n</table></div>",
    url = base_url(port),
    mode = store.mode.as_str(),
    rows = rows
  );
  wrap_page("scsh sessions", port, None, &body)
}

fn index_session_row(session: &Session, now: u64) -> String {
  let lifecycle = session.lifecycle_status(now);
  let id = esc(&session.id);
  let profile = esc(session.profile.as_deref().unwrap_or("default"));
  let n_procs = session.procs.len();
  let status = format!(r#"<span class="session-status {}">{}</span>"#, lifecycle.css_class(), esc(lifecycle.label()));
  let started_rel = format_relative_age(now.saturating_sub(session.started_at));
  let started = format!(
    "<span class=\"session-started\" data-started=\"{}\">\
<span class=\"session-started-abs\">…</span><br>\
<span class=\"dim session-started-rel\">{}</span></span>",
    session.started_at,
    esc(&started_rel)
  );
  let duration = index_duration_label(session, now, lifecycle);
  format!(
    "<tr data-session-id=\"{id}\"><td><a href=\"/session/{id}\">{id}</a></td>\
<td class=\"session-status-cell\">{status}</td>\
<td class=\"session-started-cell\">{started}</td>\
<td class=\"session-duration-cell\">{duration}</td>\
<td>{profile}</td><td>{n_procs}</td><td class=\"dim repo-path\">{repo}</td></tr>\n",
    id = id,
    status = status,
    started = started,
    duration = esc(&duration),
    profile = profile,
    n_procs = n_procs,
    repo = esc(&session.repo),
  )
}

fn index_duration_label(session: &Session, now: u64, lifecycle: SessionLifecycle) -> String {
  match session.duration_secs(now) {
    Some(secs) if lifecycle == SessionLifecycle::Running => format!("{} so far", format_duration_secs(secs)),
    Some(secs) => format_duration_secs(secs),
    None => "—".to_string(),
  }
}
