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
<tbody id=\"sessions-body\">\n{rows}</tbody>\n</table></div>\n{repos}{images}",
    url = base_url(port),
    mode = store.mode.as_str(),
    rows = rows,
    repos = repos_panel(),
    images = images_panel()
  );
  wrap_page("scsh sessions", port, None, &body)
}

/// The images panel: a table of every scsh image (populated by the client from
/// `GET /api/v1/images`) plus the Build buttons that POST `/api/v1/images/build` and
/// deep-link into the spawned `scsh build-images` session.
fn images_panel() -> &'static str {
  r##"<h2>images</h2>
<p class="dim">The container images scsh builds: the shared base, plus one per harness.
Stale means the image exists but no longer matches this scsh build's embedded Dockerfile.</p>
<div class="table-scroll"><table>
<thead><tr><th></th><th>Image</th><th>Status</th><th>Created</th><th>Size</th></tr></thead>
<tbody id="images-body"><tr><td colspan="5" class="dim">loading…</td></tr></tbody>
</table></div>
<div class="images-controls">
<button id="images-build-selected" disabled>Build selected</button>
<button id="images-build-all">Build all</button>
<label><input type="checkbox" id="images-rebuild-base"> also rebuild the base image (--no-cache)</label>
<label><input type="checkbox" id="images-force"> force rebuild even when up to date</label>
<a href="#" id="images-refresh">refresh</a>
<span id="images-note" class="dim"></span>
</div>
"##
}

/// The repositories panel: open a clean git repo (POST `/api/v1/repos/open`), pick one of the
/// harness definitions it returns, fill the rendered param form, and start a job (POST
/// `/api/v1/jobs/start`, deep-linking to the spawned session). The jobs-by-repository table is
/// grouped client-side from the live WebSocket session snapshot.
fn repos_panel() -> &'static str {
  r##"<h2>repositories</h2>
<p class="dim">Open a clean git repository to configure and start a harness-definition job in it —
the daemon runs it just like <code>scsh run</code>. One job per repository at a time.</p>
<div class="images-controls">
<input type="text" id="repo-path" placeholder="/path/to/a/git/repo" size="44">
<button id="repo-open">Open</button>
<span id="repo-note" class="dim"></span>
</div>
<div id="defs-panel" hidden>
<p class="dim">definitions in <code id="open-repo-path"></code></p>
<div id="defs-list"></div>
<div id="def-form"></div>
</div>
<h3>jobs by repository</h3>
<div class="table-scroll"><table>
<thead><tr><th>Repository</th><th>Jobs</th></tr></thead>
<tbody id="repos-body"><tr><td colspan="2" class="dim">No repositories open yet.</td></tr></tbody>
</table></div>
"##
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
