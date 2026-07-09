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
    "<h1>scsh</h1>\n\
<p class=\"subtitle\">Daemon on {url} · mode {mode}</p>\n\
<nav class=\"tabs\">\
<button class=\"tab active\" data-tab=\"jobs\">Jobs</button>\
<button class=\"tab\" data-tab=\"dirs\">Directories</button>\
<button class=\"tab\" data-tab=\"start\">Start a job</button>\
<button class=\"tab\" data-tab=\"images\">Containers</button>\
</nav>\n\
<section class=\"tab-panel active\" id=\"tab-jobs\">\n\
<div class=\"card card--accent-left-cyan\">\n\
<p class=\"section-label\">Jobs</p>\n\
<div class=\"table-scroll\"><table>\n\
<thead><tr><th>Session</th><th>Status</th><th>Started</th><th>Duration</th>\
<th>Profile</th><th>Procs</th><th>Repo</th></tr></thead>\n\
<tbody id=\"sessions-body\">\n{rows}</tbody>\n</table></div>\n\
</div>\n\
</section>\n\
<section class=\"tab-panel\" id=\"tab-dirs\">\n{dirs}</section>\n\
<section class=\"tab-panel\" id=\"tab-start\">\n{start}</section>\n\
<section class=\"tab-panel\" id=\"tab-images\">\n{images}</section>\n",
    url = base_url(port),
    mode = store.mode.as_str(),
    rows = rows,
    dirs = dirs_panel(),
    start = start_panel(),
    images = images_panel()
  );
  wrap_page("scsh", port, None, &body)
}

/// The images panel: a table of every scsh image (populated by the client from
/// `GET /api/v1/images`) plus the Build buttons that POST `/api/v1/images/build` and
/// deep-link into the spawned `scsh build-images` session.
///
/// First paint already lists every known image in a `checking…` state (base + each harness).
/// That is deliberate: §13 of the eng principles forbids a limbo where the panel looks empty
/// while the (slow) runtime inspect is still in flight — the set of images is known a priori;
/// only their status is pending.
fn images_panel() -> String {
  let mut rows = String::new();
  rows.push_str(&images_skeleton_row("base", crate::runtime::BASE_IMAGE_TAG, false));
  for h in crate::config::Harness::ALL {
    rows.push_str(&images_skeleton_row(h.as_str(), &crate::runtime::image_tag(h), true));
  }
  format!(
    r##"<div class="card card--accent-left-orange">
<p class="section-label">Containers</p>
<p class="dim">The base container images scsh builds: the shared base, plus one per harness.
Stale means the image exists but no longer matches this scsh build's embedded Dockerfile —
rebuild it here.</p>
<div class="table-scroll"><table>
<thead><tr><th></th><th>Image</th><th>Status</th><th>Created</th><th>Size</th></tr></thead>
<tbody id="images-body">
{rows}</tbody>
</table></div>
<div class="images-controls">
<button type="button" class="chamfer btn btn--cyan btn--sm" id="images-build-selected" disabled><span>Build selected</span></button>
<button type="button" class="chamfer btn btn--orange btn--sm" id="images-build-all"><span>Build all</span></button>
<label><input type="checkbox" id="images-rebuild-base"> also rebuild the base image (--no-cache)</label>
<label><input type="checkbox" id="images-force"> force rebuild even when up to date</label>
<a href="#" id="images-refresh">refresh</a>
<span id="images-note" class="dim">checking container runtime…</span>
</div>
</div>
"##,
    rows = rows
  )
}

/// One known-image row in its first observable state: present, status `checking…`.
fn images_skeleton_row(name: &str, tag: &str, selectable: bool) -> String {
  let checkbox = if selectable {
    format!(
      r#"<input type="checkbox" class="image-select" value="{name}" disabled>"#,
      name = esc(name)
    )
  } else {
    String::new()
  };
  format!(
    r#"<tr data-image="{name}" data-pending="1"><td class="image-select-cell">{checkbox}</td><td><code>{tag}</code></td><td class="image-status-cell"><span class="chamfer session-status checking"><span>checking…</span></span></td><td class="dim image-created-cell">—</td><td class="dim image-size-cell">—</td></tr>
"#,
    name = esc(name),
    tag = esc(tag),
    checkbox = checkbox
  )
}

/// The "Start a job" tab: open a git repo (POST `/api/v1/repos/open`, which reports whether it is
/// runnable and why not), pick a harness definition, fill the rendered param form, and start a
/// job (POST `/api/v1/jobs/start`, deep-linking to the spawned session). Start is disabled until
/// the repo is runnable.
fn start_panel() -> &'static str {
  r##"<div class="card card--accent-left-green">
<p class="section-label">Start a job</p>
<p class="dim">Open a git repository to configure and start a harness-definition job in it —
the daemon runs it just like <code>scsh run</code>. The repo must be committed, clean, and have a
gitignored scratch dir (<code>tmp/</code> or <code>.harness/tmp</code>). One job per repository at a time.</p>
<div class="images-controls">
<div class="chamfer input-wrap" style="flex:1;min-width:16rem">
<input class="input" type="text" id="repo-path" placeholder="/path/to/a/git/repo (type, paste, or Pick…)">
</div>
<button type="button" class="chamfer btn btn--purple btn--sm" id="repo-pick"><span>Pick…</span></button>
<button type="button" class="chamfer btn btn--cyan btn--sm" id="repo-open"><span>Open</span></button>
<span id="repo-note" class="dim"></span>
</div>
<div id="repo-blockers" class="blockers" hidden></div>
<div id="defs-panel" hidden>
<p class="dim">definitions in <code id="open-repo-path"></code></p>
<div id="defs-list"></div>
<div id="def-form"></div>
</div>
</div>
"##
}

/// The "Directories" tab: a table of every opened repository and any repo with jobs, its jobs
/// grouped underneath — built client-side from the live WebSocket session snapshot.
fn dirs_panel() -> &'static str {
  r##"<div class="card card--accent-top-magenta">
<p class="section-label">Directories</p>
<p class="dim">Repositories you have opened, and every repository that has jobs, with their jobs.</p>
<div class="table-scroll"><table>
<thead><tr><th>Repository</th><th>Jobs</th></tr></thead>
<tbody id="repos-body"><tr><td colspan="2" class="dim">No repositories open yet — open one under “Start a job”.</td></tr></tbody>
</table></div>
</div>
"##
}

fn index_session_row(session: &Session, now: u64) -> String {
  let lifecycle = session.lifecycle_status(now);
  let id = esc(&session.id);
  let profile = esc(session.profile.as_deref().unwrap_or("default"));
  let n_procs = session.procs.len();
  let status = format!(
    r#"<span class="chamfer session-status {}"><span>{}</span></span>"#,
    lifecycle.css_class(),
    esc(lifecycle.label())
  );
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
